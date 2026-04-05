//! DaemonRouter — FIFO command queue + dispatch (AUTH.DaemonRouter).
//! Owns command transaction lifecycle, epoch management, and replay cache.

mod addressing;
pub(crate) mod automation_fence;
mod diagnostics;
mod dialogs;
mod dispatch;
mod downloads;
mod element_semantics;
mod extract;
mod extract_postprocess;
mod find;
mod frame_scope;
mod frames;
mod history;
mod inspect;
mod interaction;
mod interference;
mod navigation;
mod network_inspection;
mod observation_filter;
mod observation_scope;
mod observe;
mod orchestration;
mod policy;
mod projection;
mod query;
pub(crate) mod request_args;
mod runtime;
pub(crate) mod secret_resolution;
mod snapshot;
mod state_format;
mod storage;
mod timeout;
mod triggers;
mod url_normalization;
mod wait_after;
mod workflow;

use std::time::Instant;
use std::{future::Future, pin::Pin, sync::Arc};
use tracing::info;
use uuid::Uuid;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::{NetworkRule, NetworkRuleSpec, NetworkRuleStatus, Timing};
use rub_core::port::BrowserPort;
use rub_ipc::codec::{MAX_FRAME_BYTES, encoded_frame_len};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};

use crate::session::{ReplayCommandClaim, SessionState};

use diagnostics::agent_capabilities;
use diagnostics::detection_risks;
use dispatch::execute_named_command_with_fence;
use policy::{command_allowed_during_handoff, response_dom_epoch};
use projection::{attach_interaction_projection, attach_select_projection};
use timeout::{TimeoutPhase, timeout_context};
#[cfg(test)]
use timeout::{augment_wait_timeout_error, wait_timeout_error};
use wait_after::apply_post_wait_if_requested;

#[derive(Debug, Clone)]
struct ReplayFenceOwner {
    command_id: String,
    fingerprint: String,
    finalize: ReplayFinalizeMode,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ReplayFinalizeMode {
    ReleaseOnly,
    CacheCommittedResponse,
}

impl ReplayFenceOwner {
    fn new(command_id: String, fingerprint: String) -> Self {
        Self {
            command_id,
            fingerprint,
            finalize: ReplayFinalizeMode::ReleaseOnly,
        }
    }

    fn mark_execution_started(&mut self) {
        self.finalize = ReplayFinalizeMode::CacheCommittedResponse;
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TransactionDeadline {
    started_at: Instant,
    timeout_ms: u64,
}

impl TransactionDeadline {
    fn new(timeout_ms: u64) -> Self {
        Self {
            started_at: Instant::now(),
            timeout_ms,
        }
    }

    fn elapsed_ms(self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    fn remaining_ms(self) -> u64 {
        self.timeout_ms.saturating_sub(self.elapsed_ms())
    }

    fn remaining_duration(self) -> Option<std::time::Duration> {
        let remaining_ms = self.remaining_ms();
        if remaining_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(remaining_ms))
        }
    }
}

/// The central command router. Owns the FIFO dispatch queue.
pub struct DaemonRouter {
    browser: Arc<dyn BrowserPort>,
    /// Serializes command execution (FIFO).
    exec_semaphore: tokio::sync::Semaphore,
}

struct RequestPreflight {
    request_id: String,
    deadline: TransactionDeadline,
    internal_command: bool,
}

struct PreparedCommandDispatch {
    request_id: String,
    deadline: TransactionDeadline,
    internal_command: bool,
    replay_owner: Option<ReplayFenceOwner>,
}

enum DispatchPreparation {
    Final(IpcResponse),
    Prepared(PreparedCommandDispatch),
}

impl PreparedCommandDispatch {
    fn request_id(&self) -> &str {
        &self.request_id
    }

    fn deadline(&self) -> TransactionDeadline {
        self.deadline
    }

    fn queue_ms(&self) -> u64 {
        self.deadline.elapsed_ms()
    }

    fn mark_execution_started(&mut self) {
        if let Some(owner) = self.replay_owner.as_mut() {
            owner.mark_execution_started();
        }
    }

    async fn finalize(
        self,
        request: &IpcRequest,
        response: IpcResponse,
        state: &Arc<SessionState>,
    ) -> IpcResponse {
        finalize_response(
            request,
            response,
            self.internal_command,
            self.replay_owner,
            state,
        )
        .await
    }
}

pub(crate) struct RouterTransactionGuard<'a> {
    _permit: tokio::sync::SemaphorePermit<'a>,
    state: Arc<SessionState>,
}

impl Drop for RouterTransactionGuard<'_> {
    fn drop(&mut self) {
        self.state
            .in_flight_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl DaemonRouter {
    pub fn new(browser: Arc<dyn BrowserPort>) -> Self {
        Self {
            browser,
            exec_semaphore: tokio::sync::Semaphore::new(1), // FIFO: one at a time
        }
    }

    pub(crate) fn browser_port(&self) -> Arc<dyn BrowserPort> {
        self.browser.clone()
    }

    /// Dispatch a request. This is the main entry point.
    pub async fn dispatch(&self, request: IpcRequest, state: &Arc<SessionState>) -> IpcResponse {
        let prepared = match self.prepare_dispatch(request.clone(), state, true).await {
            DispatchPreparation::Final(response) => return response,
            DispatchPreparation::Prepared(prepared) => prepared,
        };

        self.execute_queued_prepared_request(&request, state, prepared)
            .await
    }

    async fn acquire_fifo_permit<'a>(
        &'a self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
    ) -> Result<tokio::sync::SemaphorePermit<'a>, IpcResponse> {
        let Some(timeout) = deadline.remaining_duration() else {
            return Err(queue_timeout_response(command, request_id, deadline));
        };
        match tokio::time::timeout(timeout, self.exec_semaphore.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(IpcResponse::error(
                request_id,
                ErrorEnvelope::new(ErrorCode::IpcTimeout, "Command queue closed"),
            )),
            Err(_) => Err(queue_timeout_response(command, request_id, deadline)),
        }
    }

    async fn begin_request_transaction<'a>(
        &'a self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<RouterTransactionGuard<'a>, IpcResponse> {
        let permit = self
            .acquire_fifo_permit(command, request_id, deadline)
            .await?;
        if state.is_shutdown_requested() {
            return Err(IpcResponse::error(
                request_id,
                ErrorEnvelope::new(
                    ErrorCode::SessionBusy,
                    format!(
                        "Session '{}' is draining for shutdown; command '{}' is temporarily rejected",
                        state.session_name, command
                    ),
                )
                .with_context(serde_json::json!({
                    "command": command,
                    "reason": "session_shutting_down_after_queue_wait",
                })),
            ));
        }
        state
            .in_flight_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(RouterTransactionGuard {
            _permit: permit,
            state: state.clone(),
        })
    }

    pub(crate) async fn begin_automation_transaction<'a>(
        &'a self,
        state: &Arc<SessionState>,
        timeout_ms: u64,
        command: &str,
    ) -> Result<RouterTransactionGuard<'a>, ErrorEnvelope> {
        let request_id = Uuid::now_v7().to_string();
        self.begin_request_transaction(
            command,
            &request_id,
            TransactionDeadline::new(timeout_ms),
            state,
        )
        .await
        .map_err(|response| {
            response.error.unwrap_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::IpcTimeout,
                    format!("Automation transaction '{command}' failed to acquire FIFO"),
                )
            })
        })
    }

    pub(crate) fn dispatch_within_active_transaction_preserving_replay<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        Box::pin(async move {
            match self.prepare_dispatch(request.clone(), state, false).await {
                DispatchPreparation::Final(response) => response,
                DispatchPreparation::Prepared(prepared) => {
                    self.execute_prepared_request(&request, state, prepared)
                        .await
                }
            }
        })
    }

    pub(crate) fn dispatch_within_active_transaction<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        self.dispatch_within_active_transaction_preserving_replay(request, state)
    }

    async fn execute_request_once(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        request_id: &str,
        queue_ms: u64,
        deadline: TransactionDeadline,
    ) -> IpcResponse {
        let exec_start = Instant::now();
        let exec_budget_ms = deadline.remaining_ms();
        let Some(exec_timeout) = deadline.remaining_duration() else {
            return execution_timeout_response(request, request_id, queue_ms, 0);
        };
        let result = if request_owns_authoritative_timeout(request) {
            self.execute_command(request, deadline, state).await
        } else {
            match tokio::time::timeout(exec_timeout, self.execute_command(request, deadline, state))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(execution_timeout_error(request, queue_ms, exec_budget_ms)),
            }
        };
        let exec_ms = exec_start.elapsed().as_millis() as u64;
        let timing = Timing {
            queue_ms,
            exec_ms,
            total_ms: queue_ms + exec_ms,
        };

        tracing::debug!(
            command = request.command.as_str(),
            request_id,
            queue_ms,
            exec_ms,
            total_ms = timing.total_ms,
            success = result.is_ok(),
            "command_complete"
        );

        match result {
            Ok(data) => IpcResponse::success(request_id, data).with_timing(timing),
            Err(err) => IpcResponse::error(request_id, err.into_envelope()).with_timing(timing),
        }
    }

    async fn execute_prepared_request(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        mut prepared: PreparedCommandDispatch,
    ) -> IpcResponse {
        let queue_ms = prepared.queue_ms();
        prepared.mark_execution_started();
        let response = self
            .execute_request_once(
                request,
                state,
                prepared.request_id(),
                queue_ms,
                prepared.deadline(),
            )
            .await;
        prepared.finalize(request, response, state).await
    }

    async fn execute_queued_prepared_request(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        prepared: PreparedCommandDispatch,
    ) -> IpcResponse {
        // Acquire FIFO slot (INV-006: queue timeout) and keep the request in
        // the single in-flight transaction fence until the command commits.
        let transaction = match self
            .begin_request_transaction(
                request.command.as_str(),
                prepared.request_id(),
                prepared.deadline(),
                state,
            )
            .await
        {
            Ok(permit) => permit,
            Err(response) => {
                return prepared.finalize(request, response, state).await;
            }
        };

        let response = self
            .execute_prepared_request(request, state, prepared)
            .await;
        drop(transaction);
        response
    }

    async fn prepare_dispatch(
        &self,
        request: IpcRequest,
        state: &Arc<SessionState>,
        allow_handshake: bool,
    ) -> DispatchPreparation {
        let preflight = prepare_request_preflight(&request);
        let request_id = preflight.request_id.clone();

        if allow_handshake && request.command == "_handshake" {
            let response = match runtime::cmd_handshake(self, state).await {
                Ok(data) => IpcResponse::success(&request_id, data),
                Err(error) => IpcResponse::error(&request_id, error.into_envelope()),
            };
            return DispatchPreparation::Final(
                finalize_response(&request, response, true, None, state).await,
            );
        }

        if let Some(response) = preflight_rejection_response(&request, &preflight, state) {
            return DispatchPreparation::Final(
                finalize_response(&request, response, preflight.internal_command, None, state)
                    .await,
            );
        }

        match prepare_command_dispatch(&request, state, preflight).await {
            Ok(prepared) => DispatchPreparation::Prepared(prepared),
            Err(response) => DispatchPreparation::Final(response),
        }
    }

    /// Execute a specific command against the browser.
    async fn execute_command(
        &self,
        request: &IpcRequest,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        execute_named_command_with_fence(
            self,
            request.command.as_str(),
            &request.args,
            deadline,
            state,
        )
        .await
    }

    /// Shutdown the browser (called on daemon exit).
    pub async fn shutdown(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.browser.close().await.map_err(|e| Box::new(e) as _)
    }
}

fn request_owns_authoritative_timeout(request: &IpcRequest) -> bool {
    request.command == "download"
        && request.args.get("sub").and_then(|value| value.as_str()) == Some("save")
}

async fn prepare_replay_fence(
    request: &IpcRequest,
    state: &Arc<SessionState>,
    request_id: &str,
    deadline: TransactionDeadline,
) -> Result<Option<ReplayFenceOwner>, IpcResponse> {
    let Some(command_id) = request.command_id.as_ref() else {
        return Ok(None);
    };
    let fingerprint = replay_request_fingerprint(request);

    loop {
        match state.claim_replay_command(command_id, fingerprint.clone()) {
            ReplayCommandClaim::Cached(cached) => {
                info!(command_id = %command_id, "Returning cached response (at-most-once)");
                return Err(attach_request_command_id(request, *cached));
            }
            ReplayCommandClaim::Owner => {
                return Ok(Some(ReplayFenceOwner::new(
                    command_id.clone(),
                    fingerprint.clone(),
                )));
            }
            ReplayCommandClaim::Conflict => {
                return Err(attach_request_command_id(
                    request,
                    replay_fingerprint_conflict_response(
                        request_id,
                        request.command.as_str(),
                        command_id,
                    ),
                ));
            }
            ReplayCommandClaim::Wait(mut receiver) => {
                let Some(wait_timeout) = deadline.remaining_duration() else {
                    return Err(attach_request_command_id(
                        request,
                        replay_timeout_response(request, request_id, command_id, deadline),
                    ));
                };
                let wait = tokio::time::timeout(wait_timeout, async {
                    if *receiver.borrow() == crate::session::ReplayFenceState::Released {
                        return Ok::<(), tokio::sync::watch::error::RecvError>(());
                    }
                    loop {
                        receiver.changed().await?;
                        if *receiver.borrow() == crate::session::ReplayFenceState::Released {
                            return Ok(());
                        }
                    }
                })
                .await;
                match wait {
                    Ok(Ok(())) => continue,
                    Ok(Err(_)) => {
                        return Err(attach_request_command_id(
                            request,
                            IpcResponse::error(
                                request_id,
                                ErrorEnvelope::new(
                                    ErrorCode::IpcProtocolError,
                                    format!(
                                        "Command replay fence for '{}' closed before publishing a cached response",
                                        request.command
                                    ),
                                )
                                .with_context(serde_json::json!({
                                    "command": request.command,
                                    "command_id": command_id,
                                    "reason": "replay_fence_channel_closed",
                                })),
                            ),
                        ));
                    }
                    Err(_) => {
                        return Err(attach_request_command_id(
                            request,
                            replay_timeout_response(request, request_id, command_id, deadline),
                        ));
                    }
                }
            }
        }
    }
}

fn prepare_request_preflight(request: &IpcRequest) -> RequestPreflight {
    let request_id = Uuid::now_v7().to_string();
    let deadline = TransactionDeadline::new(request.timeout_ms);
    let internal_command = dispatch::is_internal_command(request.command.as_str());
    RequestPreflight {
        request_id,
        deadline,
        internal_command,
    }
}

async fn prepare_command_dispatch(
    request: &IpcRequest,
    state: &Arc<SessionState>,
    preflight: RequestPreflight,
) -> Result<PreparedCommandDispatch, IpcResponse> {
    let RequestPreflight {
        request_id,
        deadline,
        internal_command,
    } = preflight;

    let replay_owner = prepare_replay_fence(request, state, &request_id, deadline).await?;
    if let Some(response) = handoff_blocked_response(request, state, &request_id).await {
        return Err(PreparedCommandDispatch {
            request_id,
            deadline,
            internal_command,
            replay_owner,
        }
        .finalize(request, response, state)
        .await);
    }

    Ok(PreparedCommandDispatch {
        request_id,
        deadline,
        internal_command,
        replay_owner,
    })
}

fn preflight_rejection_response(
    request: &IpcRequest,
    preflight: &RequestPreflight,
    state: &Arc<SessionState>,
) -> Option<IpcResponse> {
    if !preflight.internal_command && request.ipc_protocol_version != IPC_PROTOCOL_VERSION {
        return Some(protocol_version_mismatch_response(
            &preflight.request_id,
            request,
        ));
    }
    if !preflight.internal_command
        && let Some(expected_daemon_session_id) = request.daemon_session_id.as_deref()
        && expected_daemon_session_id != state.session_id
    {
        return Some(daemon_authority_mismatch_response(
            &preflight.request_id,
            request,
            &state.session_id,
        ));
    }
    if state.is_shutdown_requested() {
        return Some(IpcResponse::error(
            &preflight.request_id,
            ErrorEnvelope::new(
                ErrorCode::SessionBusy,
                format!(
                    "Session '{}' is draining for shutdown; command '{}' is temporarily rejected",
                    state.session_name, request.command
                ),
            )
            .with_context(serde_json::json!({
                "command": request.command,
                "reason": "session_shutting_down",
            })),
        ));
    }
    None
}

async fn handoff_blocked_response(
    request: &IpcRequest,
    state: &Arc<SessionState>,
    request_id: &str,
) -> Option<IpcResponse> {
    if !state.is_handoff_active().await || command_allowed_during_handoff(request.command.as_str())
    {
        return None;
    }

    Some(IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::AutomationPaused,
            format!(
                "Automation is paused for human verification handoff; command '{}' is temporarily blocked",
                request.command,
            ),
        )
        .with_context(serde_json::json!({
            "command": request.command,
            "handoff": state.human_verification_handoff().await,
        })),
    ))
}

async fn finalize_response(
    request: &IpcRequest,
    mut response: IpcResponse,
    internal_command: bool,
    replay_owner: Option<ReplayFenceOwner>,
    state: &Arc<SessionState>,
) -> IpcResponse {
    if let Some(ref owner) = replay_owner {
        response = response
            .with_command_id(owner.command_id.clone())
            .expect("validated replay command_id must remain protocol-valid");
    } else if let Some(ref cmd_id) = request.command_id {
        response = response
            .with_command_id(cmd_id.clone())
            .expect("validated request command_id must remain protocol-valid");
    }

    response = enforce_response_frame_limit(request, response);

    finalize_replay_fence(replay_owner.as_ref(), &response, state).await;

    if !internal_command {
        state.submit_post_commit_projection(request, &response);
        state.spawn_post_commit_projection_drain();
    }

    response
}

async fn finalize_replay_fence(
    replay_owner: Option<&ReplayFenceOwner>,
    response: &IpcResponse,
    state: &Arc<SessionState>,
) {
    let Some(owner) = replay_owner else {
        return;
    };
    if owner.finalize == ReplayFinalizeMode::CacheCommittedResponse {
        // Replay cache must store the exact on-wire response shape that the
        // first caller observed after all response-commit fencing, including
        // frame-limit downgrades. Otherwise a replay can diverge from the
        // original committed response authority.
        state
            .cache_response(
                owner.command_id.clone(),
                owner.fingerprint.clone(),
                response.clone(),
            )
            .await;
    }
    state.release_replay_command(&owner.command_id);
}

fn enforce_response_frame_limit(request: &IpcRequest, response: IpcResponse) -> IpcResponse {
    let encoded_len = encoded_frame_len(&response).unwrap_or(usize::MAX);
    if encoded_len <= MAX_FRAME_BYTES {
        return response;
    }

    let mut overflow = IpcResponse::error(
        &response.request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Command '{}' response exceeded the IPC frame limit; reduce payload size or save large artifacts to disk",
                request.command
            ),
        )
        .with_context(serde_json::json!({
            "reason": "response_exceeds_ipc_frame_limit",
            "command": request.command,
            "max_frame_bytes": MAX_FRAME_BYTES,
            "encoded_frame_bytes": encoded_len,
        })),
    )
    .with_timing(response.timing);
    if let Some(command_id) = response.command_id.as_ref() {
        overflow = overflow
            .with_command_id(command_id.clone())
            .expect("validated command_id must remain protocol-valid");
    }
    overflow
}

fn attach_request_command_id(request: &IpcRequest, response: IpcResponse) -> IpcResponse {
    if let Some(command_id) = request.command_id.as_ref() {
        response
            .with_command_id(command_id.clone())
            .expect("validated request command_id must remain protocol-valid")
    } else {
        response
    }
}

fn protocol_version_mismatch_response(request_id: &str, request: &IpcRequest) -> IpcResponse {
    IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcVersionMismatch,
            format!(
                "Client version {} != daemon version {}",
                request.ipc_protocol_version, IPC_PROTOCOL_VERSION
            ),
        )
        .with_context(serde_json::json!({
            "cli_protocol_version": request.ipc_protocol_version,
            "daemon_protocol_version": IPC_PROTOCOL_VERSION,
        })),
    )
}

fn daemon_authority_mismatch_response(
    request_id: &str,
    request: &IpcRequest,
    current_session_id: &str,
) -> IpcResponse {
    IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Command '{}' targeted daemon session '{}' but reached '{}'",
                request.command,
                request.daemon_session_id.as_deref().unwrap_or("unknown"),
                current_session_id
            ),
        )
        .with_context(serde_json::json!({
            "reason": "daemon_authority_mismatch",
            "expected_daemon_session_id": request.daemon_session_id,
            "current_session_id": current_session_id,
            "command": request.command,
        })),
    )
}

fn queue_timeout_response(
    command: &str,
    request_id: &str,
    deadline: TransactionDeadline,
) -> IpcResponse {
    let queue_ms = deadline.elapsed_ms();
    IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcTimeout,
            format!("Command timed out waiting in queue after {queue_ms}ms"),
        )
        .with_suggestion(
            "This session runs one command at a time. Wait for the earlier command to finish, use a separate RUB_HOME for parallel work, or increase --timeout",
        )
        .with_context(timeout_context(
            command,
            TimeoutPhase::Queue,
            deadline.timeout_ms,
            queue_ms,
            None,
        )),
    )
}

fn execution_timeout_error(request: &IpcRequest, queue_ms: u64, exec_budget_ms: u64) -> RubError {
    let (code, msg) = match request.command.as_str() {
        "open" => (
            ErrorCode::PageLoadTimeout,
            "Page load timed out during execution",
        ),
        "exec" => (ErrorCode::JsTimeout, "JavaScript execution timed out"),
        "wait" => (ErrorCode::WaitTimeout, "Wait condition timed out"),
        _ => (ErrorCode::IpcTimeout, "Command execution timed out"),
    };
    RubError::Domain(ErrorEnvelope::new(code, msg).with_context(timeout_context(
        request.command.as_str(),
        TimeoutPhase::Execution,
        request.timeout_ms,
        queue_ms,
        Some(exec_budget_ms),
    )))
}

fn execution_timeout_response(
    request: &IpcRequest,
    request_id: &str,
    queue_ms: u64,
    exec_budget_ms: u64,
) -> IpcResponse {
    IpcResponse::error(
        request_id,
        execution_timeout_error(request, queue_ms, exec_budget_ms).into_envelope(),
    )
}

fn replay_timeout_response(
    request: &IpcRequest,
    request_id: &str,
    command_id: &str,
    deadline: TransactionDeadline,
) -> IpcResponse {
    IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcTimeout,
            format!(
                "Command '{}' timed out waiting for an earlier in-flight request with the same command_id",
                request.command
            ),
        )
        .with_context(serde_json::json!({
            "command": request.command,
            "command_id": command_id,
            "phase": "replay_fence",
            "transaction_timeout_ms": request.timeout_ms,
            "elapsed_ms": deadline.elapsed_ms(),
            "reason": "replay_fence_wait_timeout",
        })),
    )
}

fn replay_request_fingerprint(request: &IpcRequest) -> String {
    let mut fingerprint = String::with_capacity(request.command.len() + 64);
    fingerprint.push_str(&request.command);
    fingerprint.push('\u{1f}');
    append_canonical_json(&request.args, &mut fingerprint);
    fingerprint
}

fn append_canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(value) => {
            out.push_str(&serde_json::to_string(value).expect("json string serialization"))
        }
        serde_json::Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                append_canonical_json(value, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(values) => {
            out.push('{');
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("json key serialization"));
                out.push(':');
                append_canonical_json(&values[key], out);
            }
            out.push('}');
        }
    }
}

fn replay_fingerprint_conflict_response(
    request_id: &str,
    command: &str,
    command_id: &str,
) -> IpcResponse {
    IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "command_id '{command_id}' was reused for a different request than '{command}'"
            ),
        )
        .with_context(serde_json::json!({
            "command": command,
            "command_id": command_id,
            "reason": "replay_command_id_fingerprint_mismatch",
        })),
    )
}

fn attach_response_metadata(
    mut data: serde_json::Value,
    dom_epoch: Option<u64>,
) -> serde_json::Value {
    if let Some(epoch) = dom_epoch
        && let Some(object) = data.as_object_mut()
    {
        object.insert("dom_epoch".to_string(), serde_json::json!(epoch));
    }

    data
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonRouter, TimeoutPhase, TransactionDeadline, attach_interaction_projection,
        attach_select_projection, augment_wait_timeout_error, command_allowed_during_handoff,
        detection_risks, finalize_response, prepare_replay_fence,
        protocol_version_mismatch_response, replay_request_fingerprint,
        request_owns_authoritative_timeout, timeout_context, wait_timeout_error,
    };
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::{
        IdentityProbeStatus, IdentitySelfProbeInfo, InteractionActuation, InteractionConfirmation,
        InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
        InteractionSemanticClass, LaunchPolicyInfo, SelectOutcome,
    };
    use rub_ipc::codec::MAX_FRAME_BYTES;
    use rub_ipc::protocol::{IpcRequest, IpcResponse};
    use std::sync::atomic::AtomicU64;
    use std::{path::PathBuf, sync::Arc};

    use crate::session::{ReplayCommandClaim, SessionState};

    fn test_router() -> DaemonRouter {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        DaemonRouter::new(adapter)
    }

    #[test]
    fn epoch_command_matrix_matches_current_contract() {
        for command in [
            "open",
            "click",
            "exec",
            "back",
            "keys",
            "type",
            "switch",
            "close-tab",
            "hover",
            "upload",
            "select",
        ] {
            assert!(
                super::policy::command_increments_epoch(command),
                "{command} should increment epoch"
            );
        }

        for command in [
            "state",
            "screenshot",
            "doctor",
            "sessions",
            "tabs",
            "wait",
            "scroll",
            "fill",
            "pipe",
            "get-text",
            "bbox",
            "cookies",
        ] {
            assert!(
                !super::policy::command_increments_epoch(command),
                "{command} should not increment epoch"
            );
        }
    }

    #[test]
    fn fill_and_pipe_publish_current_dom_epoch_without_incrementing() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-epoch"),
            None,
        ));
        let current_epoch = state.increment_epoch();

        assert_eq!(
            super::policy::response_dom_epoch("fill", &serde_json::json!({}), &state),
            Some(current_epoch)
        );
        assert_eq!(state.current_epoch(), current_epoch);

        assert_eq!(
            super::policy::response_dom_epoch("pipe", &serde_json::json!({}), &state),
            Some(current_epoch)
        );
        assert_eq!(state.current_epoch(), current_epoch);
    }

    #[test]
    fn dialog_accept_and_dismiss_commit_new_dom_epoch() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-dialog-epoch"),
            None,
        ));
        let base_epoch = state.current_epoch();

        assert_eq!(
            super::policy::response_dom_epoch(
                "dialog",
                &serde_json::json!({ "sub": "accept" }),
                &state,
            ),
            Some(base_epoch + 1)
        );
        assert_eq!(state.current_epoch(), base_epoch + 1);

        assert_eq!(
            super::policy::response_dom_epoch(
                "dialog",
                &serde_json::json!({ "sub": "dismiss" }),
                &state,
            ),
            Some(base_epoch + 2)
        );
        assert_eq!(state.current_epoch(), base_epoch + 2);

        assert_eq!(
            super::policy::response_dom_epoch(
                "dialog",
                &serde_json::json!({ "sub": "status" }),
                &state,
            ),
            None
        );
        assert_eq!(state.current_epoch(), base_epoch + 2);
    }

    #[test]
    fn download_save_owns_its_authoritative_timeout_at_router_boundary() {
        let request = IpcRequest::new(
            "download",
            serde_json::json!({
                "sub": "save",
                "file": "/tmp/assets.json",
                "output_dir": "/tmp/out",
            }),
            30_000,
        );
        assert!(request_owns_authoritative_timeout(&request));

        let wait_request = IpcRequest::new(
            "download",
            serde_json::json!({
                "sub": "wait",
            }),
            30_000,
        );
        assert!(!request_owns_authoritative_timeout(&wait_request));
    }

    #[test]
    fn doctor_detection_risks_follow_structured_contract() {
        let launch_policy = LaunchPolicyInfo {
            headless: true,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: None,
            connection_target: None,
            stealth_level: Some("L1".to_string()),
            stealth_patches: Some(vec!["webdriver_undefined".to_string()]),
            stealth_default_enabled: Some(true),
            humanize_enabled: Some(false),
            humanize_speed: Some("normal".to_string()),
            stealth_coverage: Some(rub_core::model::StealthCoverageInfo {
                coverage_mode: Some("page_frame_only".to_string()),
                page_hook_installations: Some(1),
                page_hook_failures: Some(0),
                iframe_targets_detected: Some(0),
                worker_targets_detected: Some(0),
                service_worker_targets_detected: Some(0),
                shared_worker_targets_detected: Some(0),
                user_agent_override: Some(true),
                user_agent_metadata_override: Some(true),
                observed_target_types: vec!["page".to_string()],
                self_probe: None,
            }),
        };

        let risks = detection_risks(&launch_policy);
        assert_eq!(risks.len(), 2);
        assert_eq!(risks[0].risk, "headless_mode");
        assert_eq!(risks[0].severity, "medium");
        assert_eq!(risks[1].risk, "no_user_data_dir");
    }

    #[test]
    fn doctor_detection_risks_report_self_probe_failures() {
        let launch_policy = LaunchPolicyInfo {
            headless: true,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: Some("/tmp/profile".to_string()),
            connection_target: None,
            stealth_level: Some("L1".to_string()),
            stealth_patches: Some(vec!["webdriver_undefined".to_string()]),
            stealth_default_enabled: Some(true),
            humanize_enabled: Some(false),
            humanize_speed: Some("normal".to_string()),
            stealth_coverage: Some(rub_core::model::StealthCoverageInfo {
                coverage_mode: Some("page_frame_worker_bridge".to_string()),
                page_hook_installations: Some(1),
                page_hook_failures: Some(0),
                iframe_targets_detected: Some(1),
                worker_targets_detected: Some(1),
                service_worker_targets_detected: Some(0),
                shared_worker_targets_detected: Some(0),
                user_agent_override: Some(true),
                user_agent_metadata_override: Some(true),
                observed_target_types: vec![
                    "page".to_string(),
                    "iframe".to_string(),
                    "worker".to_string(),
                ],
                self_probe: Some(IdentitySelfProbeInfo {
                    page_main_world: Some(IdentityProbeStatus::Passed),
                    iframe_context: Some(IdentityProbeStatus::Failed),
                    worker_context: Some(IdentityProbeStatus::Unknown),
                    ua_consistency: Some(IdentityProbeStatus::Failed),
                    webgl_surface: Some(IdentityProbeStatus::Failed),
                    canvas_surface: Some(IdentityProbeStatus::Unknown),
                    audio_surface: Some(IdentityProbeStatus::Failed),
                    permissions_surface: Some(IdentityProbeStatus::Failed),
                    viewport_surface: Some(IdentityProbeStatus::Failed),
                    touch_surface: Some(IdentityProbeStatus::Unknown),
                    window_metrics_surface: Some(IdentityProbeStatus::Failed),
                    unsupported_surfaces: vec!["service_worker".to_string()],
                }),
            }),
        };

        let risks = detection_risks(&launch_policy);
        let risk_names: Vec<_> = risks.iter().map(|risk| risk.risk).collect();
        assert!(risk_names.contains(&"headless_mode"));
        assert!(risk_names.contains(&"iframe_context_unverified"));
        assert!(risk_names.contains(&"worker_context_unverified"));
        assert!(risk_names.contains(&"ua_consistency_unverified"));
        assert!(risk_names.contains(&"webgl_surface_unverified"));
        assert!(risk_names.contains(&"canvas_surface_unverified"));
        assert!(risk_names.contains(&"audio_surface_unverified"));
        assert!(risk_names.contains(&"permissions_surface_unverified"));
        assert!(risk_names.contains(&"viewport_surface_unverified"));
        assert!(risk_names.contains(&"touch_surface_unverified"));
        assert!(risk_names.contains(&"window_metrics_surface_unverified"));
    }

    #[test]
    fn interaction_projection_preserves_confirmation_contract() {
        let outcome = InteractionOutcome {
            semantic_class: InteractionSemanticClass::ToggleState,
            element_verified: true,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Confirmed,
                kind: Some(InteractionConfirmationKind::ToggleState),
                details: Some(serde_json::json!({ "after_checked": true })),
            }),
        };
        let mut value = serde_json::json!({ "index": 1 });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_interaction_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: None,
                runtime_after: None,
                interference_before: None,
                interference_after: None,
                observatory_events: &[],
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &[],
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(value["interaction"]["semantic_class"], "toggle_state");
        assert_eq!(value["interaction"]["element_verified"], true);
        assert_eq!(value["interaction"]["actuation"], "pointer");
        assert_eq!(value["interaction"]["interaction_confirmed"], true);
        assert_eq!(value["interaction"]["confirmation_status"], "confirmed");
        assert_eq!(value["interaction"]["confirmation_kind"], "toggle_state");
        assert_eq!(
            value["interaction"]["confirmation_details"]["after_checked"],
            true
        );
    }

    #[test]
    fn select_projection_preserves_confirmation_contract() {
        let outcome = SelectOutcome {
            semantic_class: InteractionSemanticClass::SelectChoice,
            element_verified: false,
            selected_value: "2".to_string(),
            selected_text: "Two".to_string(),
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Confirmed,
                kind: Some(InteractionConfirmationKind::SelectionApplied),
                details: Some(serde_json::json!({ "selected_value": "2" })),
            }),
        };
        let mut value = serde_json::json!({
            "result": {
                "value": "2",
                "text": "Two"
            }
        });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_select_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: None,
                runtime_after: None,
                interference_before: None,
                interference_after: None,
                observatory_events: &[],
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &[],
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(value["interaction"]["semantic_class"], "select_choice");
        assert_eq!(value["interaction"]["element_verified"], false);
        assert_eq!(value["interaction"]["actuation"], "programmatic");
        assert_eq!(value["interaction"]["interaction_confirmed"], true);
        assert_eq!(value["interaction"]["confirmation_status"], "confirmed");
        assert_eq!(
            value["interaction"]["confirmation_kind"],
            "selection_applied"
        );
        assert_eq!(
            value["interaction"]["confirmation_details"]["selected_value"],
            "2"
        );
        assert_eq!(value["result"]["value"], "2");
        assert_eq!(value["result"]["text"], "Two");
    }

    #[test]
    fn interaction_projection_attaches_runtime_state_delta() {
        use rub_core::model::{
            AuthState, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
            RuntimeStateSnapshot, StateInspectorInfo, StateInspectorStatus,
        };

        let outcome = InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: true,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Confirmed,
                kind: Some(InteractionConfirmationKind::PageMutation),
                details: Some(serde_json::json!({ "context_changed": false })),
            }),
        };
        let before = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Active,
                auth_state: AuthState::Anonymous,
                cookie_count: 0,
                local_storage_keys: Vec::new(),
                session_storage_keys: Vec::new(),
                auth_signals: Vec::new(),
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability: RouteStability::Stable,
                loading_present: false,
                skeleton_present: false,
                overlay_state: OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: Vec::new(),
                degraded_reason: None,
            },
        };
        let after = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Active,
                auth_state: AuthState::Unknown,
                cookie_count: 0,
                local_storage_keys: vec!["authToken".to_string()],
                session_storage_keys: Vec::new(),
                auth_signals: vec![
                    "local_storage_present".to_string(),
                    "auth_like_storage_key_present".to_string(),
                ],
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability: RouteStability::Transitioning,
                loading_present: true,
                skeleton_present: false,
                overlay_state: OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: vec![
                    "loading_present".to_string(),
                    "route_transitioning".to_string(),
                ],
                degraded_reason: None,
            },
        };

        let mut value = serde_json::json!({ "index": 1 });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_interaction_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: Some(&before),
                runtime_after: Some(&after),
                interference_before: None,
                interference_after: None,
                observatory_events: &[],
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &[],
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(
            value["interaction"]["runtime_state_delta"]["changed"],
            serde_json::json!([
                "state_inspector.auth_state",
                "state_inspector.local_storage_keys",
                "state_inspector.auth_signals",
                "readiness_state.route_stability",
                "readiness_state.loading_present",
                "readiness_state.blocking_signals"
            ])
        );
    }

    #[test]
    fn interaction_projection_attaches_context_turnover() {
        let outcome = InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: true,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Degraded,
                kind: Some(InteractionConfirmationKind::PageMutation),
                details: Some(serde_json::json!({
                    "context_changed": true,
                    "before_page": {
                        "url": "https://example.com/a",
                        "title": "A",
                        "context_replaced": false
                    },
                    "after_page": {
                        "url": "https://example.com/b",
                        "title": "B",
                        "context_replaced": true
                    }
                })),
            }),
        };

        let mut value = serde_json::json!({ "index": 1 });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_interaction_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: None,
                runtime_after: None,
                interference_before: None,
                interference_after: None,
                observatory_events: &[],
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &[],
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(
            value["interaction"]["context_turnover"]["context_changed"],
            true
        );
        assert_eq!(
            value["interaction"]["context_turnover"]["context_replaced"],
            true
        );
        assert_eq!(
            value["interaction"]["context_turnover"]["before_page"]["url"],
            "https://example.com/a"
        );
        assert_eq!(
            value["interaction"]["context_turnover"]["after_page"]["url"],
            "https://example.com/b"
        );
    }

    #[test]
    fn interaction_projection_attaches_runtime_observatory_events() {
        use rub_core::model::{
            ConsoleErrorEvent, InteractionActuation, InteractionConfirmation,
            InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
            InteractionSemanticClass, RuntimeObservatoryEvent, RuntimeObservatoryEventPayload,
        };

        let outcome = InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: true,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Confirmed,
                kind: Some(InteractionConfirmationKind::PageMutation),
                details: None,
            }),
        };
        let events = vec![RuntimeObservatoryEvent {
            sequence: 7,
            payload: RuntimeObservatoryEventPayload::ConsoleError(ConsoleErrorEvent {
                level: "error".to_string(),
                message: "boom".to_string(),
                source: Some("main".to_string()),
            }),
        }];

        let mut value = serde_json::json!({ "index": 1 });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_interaction_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: None,
                runtime_after: None,
                interference_before: None,
                interference_after: None,
                observatory_events: &events,
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &[],
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(
            value["interaction"]["runtime_observatory_events"][0]["kind"],
            "console_error"
        );
        assert_eq!(
            value["interaction"]["runtime_observatory_events"][0]["sequence"],
            7
        );
        assert_eq!(
            value["interaction"]["runtime_observatory_events"][0]["event"]["message"],
            "boom"
        );
    }

    #[test]
    fn interaction_projection_attaches_network_request_grouping() {
        use rub_core::model::{
            InteractionActuation, InteractionConfirmation, InteractionConfirmationKind,
            InteractionConfirmationStatus, InteractionOutcome, InteractionSemanticClass,
            NetworkRequestLifecycle, NetworkRequestRecord,
        };
        use std::collections::BTreeMap;

        let outcome = InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: true,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Confirmed,
                kind: Some(InteractionConfirmationKind::PageMutation),
                details: None,
            }),
        };
        let requests = vec![
            NetworkRequestRecord {
                request_id: "req-1".to_string(),
                sequence: 12,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: "https://example.com/api/orders".to_string(),
                method: "POST".to_string(),
                tab_target_id: None,
                status: Some(200),
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_body: None,
                response_body: None,
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
                error_text: None,
                frame_id: None,
                resource_type: Some("xhr".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            NetworkRequestRecord {
                request_id: "req-2".to_string(),
                sequence: 13,
                lifecycle: NetworkRequestLifecycle::Failed,
                url: "https://example.com/api/error".to_string(),
                method: "GET".to_string(),
                tab_target_id: None,
                status: Some(500),
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_body: None,
                response_body: None,
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
                error_text: Some("HTTP 500".to_string()),
                frame_id: None,
                resource_type: Some("fetch".to_string()),
                mime_type: Some("application/json".to_string()),
            },
        ];

        let mut value = serde_json::json!({ "index": 1 });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_interaction_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: None,
                runtime_after: None,
                interference_before: None,
                interference_after: None,
                observatory_events: &[],
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &requests,
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(
            value["interaction"]["network_requests"]["requests"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            2
        );
        assert_eq!(
            value["interaction"]["network_requests"]["terminal_count"],
            2
        );
        assert_eq!(
            value["interaction"]["network_requests"]["last_request"]["request_id"],
            "req-2"
        );
        assert_eq!(
            value["interaction"]["observed_effects"]["network_requests"]["requests"][0]["request_id"],
            "req-1"
        );
    }

    #[test]
    fn interaction_projection_attaches_observed_effects() {
        use rub_core::model::{
            ConsoleErrorEvent, InteractionActuation, InteractionConfirmation,
            InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
            InteractionSemanticClass, RuntimeObservatoryEvent, RuntimeObservatoryEventPayload,
        };

        let outcome = InteractionOutcome {
            semantic_class: InteractionSemanticClass::Activate,
            element_verified: true,
            actuation: Some(InteractionActuation::Pointer),
            confirmation: Some(InteractionConfirmation {
                status: InteractionConfirmationStatus::Confirmed,
                kind: Some(InteractionConfirmationKind::FocusChange),
                details: Some(serde_json::json!({
                    "before_active": false,
                    "after_active": true,
                })),
            }),
        };
        let events = vec![RuntimeObservatoryEvent {
            sequence: 9,
            payload: RuntimeObservatoryEventPayload::ConsoleError(ConsoleErrorEvent {
                level: "error".to_string(),
                message: "focused".to_string(),
                source: Some("main".to_string()),
            }),
        }];

        let mut value = serde_json::json!({ "index": 1 });
        let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
        attach_interaction_projection(
            &mut value,
            &outcome,
            crate::router::projection::ProjectionSignals {
                frame_runtime: &frame_runtime,
                runtime_before: None,
                runtime_after: None,
                interference_before: None,
                interference_after: None,
                observatory_events: &events,
                observatory_authoritative: true,
                observatory_degraded_reason: None,
                network_requests: &[],
                network_authoritative: true,
                network_degraded_reason: None,
                download_events: &[],
            },
        );

        assert_eq!(
            value["interaction"]["observed_effects"]["before_active"],
            false
        );
        assert_eq!(
            value["interaction"]["observed_effects"]["after_active"],
            true
        );
        assert_eq!(
            value["interaction"]["observed_effects"]["runtime_observatory_events"][0]["sequence"],
            9
        );
    }

    #[test]
    fn timeout_context_reports_phase_and_budget() {
        let context = timeout_context("exec", TimeoutPhase::Execution, 1000, 120, Some(880));
        assert_eq!(context["command"], "exec");
        assert_eq!(context["phase"], "execution");
        assert_eq!(context["transaction_timeout_ms"], 1000);
        assert_eq!(context["queue_ms"], 120);
        assert_eq!(context["exec_budget_ms"], 880);
    }

    #[test]
    fn wait_timeout_context_merges_probe_and_execution_attribution() {
        let err = wait_timeout_error(
            &serde_json::json!({ "selector": ".ready" }),
            5_000,
            250,
            Some(4_750),
        );
        let RubError::Domain(envelope) = augment_wait_timeout_error(
            err,
            &serde_json::json!({ "selector": ".ready" }),
            5_000,
            250,
        ) else {
            panic!("expected domain error");
        };

        assert_eq!(envelope.code, ErrorCode::WaitTimeout);
        let context = envelope
            .context
            .expect("wait timeout should include context");
        assert_eq!(context["command"], "wait");
        assert_eq!(context["phase"], "execution");
        assert_eq!(context["kind"], "selector");
        assert_eq!(context["value"], ".ready");
        assert_eq!(context["transaction_timeout_ms"], 5_000);
        assert_eq!(context["queue_ms"], 250);
        assert_eq!(context["exec_budget_ms"], 4_750);
    }

    #[test]
    fn handoff_allowlist_blocks_mutating_commands_but_keeps_runtime_surfaces_reachable() {
        assert!(!command_allowed_during_handoff("click"));
        assert!(!command_allowed_during_handoff("exec"));
        assert!(command_allowed_during_handoff("doctor"));
        assert!(command_allowed_during_handoff("runtime"));
        assert!(command_allowed_during_handoff("handoff"));
        assert!(command_allowed_during_handoff("takeover"));
        assert!(command_allowed_during_handoff("state"));
        assert!(command_allowed_during_handoff("tabs"));
    }

    #[tokio::test]
    async fn finalize_response_releases_replay_owner_without_caching_early_queue_errors() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
            .with_command_id("cmd-queue")
            .expect("static command_id must be valid");
        let replay_owner = prepare_replay_fence(
            &request,
            &state,
            "req-1",
            TransactionDeadline::new(request.timeout_ms),
        )
        .await
        .expect("first request should claim replay owner")
        .expect("replay owner should be present");

        let queue_timeout = IpcResponse::error(
            "req-1",
            rub_core::error::ErrorEnvelope::new(
                ErrorCode::IpcTimeout,
                "Command timed out waiting in queue after 250ms",
            ),
        );
        let finalized =
            finalize_response(&request, queue_timeout, false, Some(replay_owner), &state).await;

        assert_eq!(finalized.command_id.as_deref(), Some("cmd-queue"));
        let replay_owner = prepare_replay_fence(
            &request,
            &state,
            "req-2",
            TransactionDeadline::new(request.timeout_ms),
        )
        .await
        .expect("replay fence should be reclaimable after early finalize")
        .expect("replay owner should be present");
        assert_eq!(replay_owner.command_id, "cmd-queue");
    }

    #[tokio::test]
    async fn prepare_replay_fence_rejects_conflicting_request_fingerprint() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let first = IpcRequest::new("doctor", serde_json::json!({ "verbose": true }), 250)
            .with_command_id("cmd-conflict")
            .expect("static command_id must be valid");
        let conflicting = IpcRequest::new("doctor", serde_json::json!({ "verbose": false }), 250)
            .with_command_id("cmd-conflict")
            .expect("static command_id must be valid");

        let _owner = prepare_replay_fence(
            &first,
            &state,
            "req-1",
            TransactionDeadline::new(first.timeout_ms),
        )
        .await
        .expect("first request should become replay owner")
        .expect("replay owner should be present");

        let conflict = prepare_replay_fence(
            &conflicting,
            &state,
            "req-2",
            TransactionDeadline::new(conflicting.timeout_ms),
        )
        .await
        .expect_err("conflicting replay fingerprint should fail");
        assert_eq!(
            conflict.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcProtocolError)
        );
        assert_eq!(conflict.command_id.as_deref(), Some("cmd-conflict"));
    }

    #[tokio::test]
    async fn prepare_replay_fence_uses_remaining_transaction_budget() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let request = IpcRequest::new("doctor", serde_json::json!({}), 10)
            .with_command_id("cmd-budget")
            .expect("static command_id must be valid");
        let _owner = prepare_replay_fence(
            &request,
            &state,
            "req-1",
            TransactionDeadline::new(request.timeout_ms),
        )
        .await
        .expect("first request should become replay owner");

        let deadline = TransactionDeadline::new(request.timeout_ms);
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;

        let timeout = prepare_replay_fence(&request, &state, "req-2", deadline)
            .await
            .expect_err("expired deadline should fail before replay wait");
        assert_eq!(
            timeout.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcTimeout)
        );
        assert_eq!(timeout.command_id.as_deref(), Some("cmd-budget"));
        assert_eq!(
            timeout
                .error
                .as_ref()
                .and_then(|error| error.context.as_ref())
                .and_then(|ctx| ctx["phase"].as_str()),
            Some("replay_fence")
        );
    }

    #[tokio::test]
    async fn replay_wait_without_cached_response_reclaims_released_owner() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
            .with_command_id("cmd-missing")
            .expect("static command_id must be valid");

        let _owner = prepare_replay_fence(
            &request,
            &state,
            "req-1",
            TransactionDeadline::new(request.timeout_ms),
        )
        .await
        .expect("first request should become replay owner")
        .expect("replay owner should be present");

        let waiting_state = state.clone();
        let waiting_request = request.clone();
        let waiter = tokio::spawn(async move {
            prepare_replay_fence(
                &waiting_request,
                &waiting_state,
                "req-2",
                TransactionDeadline::new(waiting_request.timeout_ms),
            )
            .await
            .expect("released replay fence without cache should let waiter claim ownership")
        });

        tokio::task::yield_now().await;
        state.release_replay_command("cmd-missing");

        let replay_owner = waiter
            .await
            .expect("waiter should complete")
            .expect("released replay fence without cache should let waiter reclaim ownership");
        assert_eq!(replay_owner.command_id, "cmd-missing");

        match state.claim_replay_command("cmd-missing", replay_request_fingerprint(&request)) {
            ReplayCommandClaim::Wait(_) => {}
            other => panic!("reclaimed replay fence should now be owned in-flight, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finalize_response_preserves_command_id_for_protocol_version_mismatch() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
            .with_command_id("cmd-version")
            .expect("static command_id must be valid");
        let response = protocol_version_mismatch_response(
            "req-1",
            &IpcRequest {
                ipc_protocol_version: "0.0.0".to_string(),
                ..request.clone()
            },
        );
        let finalized = finalize_response(&request, response, false, None, &state).await;
        assert_eq!(finalized.command_id.as_deref(), Some("cmd-version"));
        assert_eq!(
            finalized.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcVersionMismatch)
        );
    }

    #[tokio::test]
    async fn finalize_response_turns_oversized_payload_into_structured_domain_error() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let request = IpcRequest::new("state", serde_json::json!({}), 250)
            .with_command_id("cmd-large")
            .expect("static command_id must be valid");
        let oversized = IpcResponse::success(
            "req-large",
            serde_json::json!({
                "payload": "x".repeat(MAX_FRAME_BYTES),
            }),
        );

        let finalized = finalize_response(&request, oversized, false, None, &state).await;

        assert_eq!(finalized.command_id.as_deref(), Some("cmd-large"));
        assert_eq!(
            finalized.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcProtocolError)
        );
        assert_eq!(finalized.status, rub_ipc::protocol::ResponseStatus::Error);
        assert_eq!(
            finalized
                .error
                .as_ref()
                .and_then(|error| error.context.as_ref())
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("response_exceeds_ipc_frame_limit")
        );
    }

    #[tokio::test]
    async fn replay_cache_commits_oversized_response_in_its_final_on_wire_shape() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let request = IpcRequest::new("state", serde_json::json!({}), 250)
            .with_command_id("cmd-large-replay")
            .expect("static command_id must be valid");
        let mut replay_owner = prepare_replay_fence(
            &request,
            &state,
            "req-1",
            TransactionDeadline::new(request.timeout_ms),
        )
        .await
        .expect("first request should claim replay owner")
        .expect("replay owner should be present");
        replay_owner.mark_execution_started();

        let oversized = IpcResponse::success(
            "req-large",
            serde_json::json!({
                "payload": "x".repeat(MAX_FRAME_BYTES),
            }),
        );

        let finalized =
            finalize_response(&request, oversized, false, Some(replay_owner), &state).await;
        assert_eq!(finalized.command_id.as_deref(), Some("cmd-large-replay"));
        assert_eq!(
            finalized.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcProtocolError)
        );

        let replay = prepare_replay_fence(
            &request,
            &state,
            "req-2",
            TransactionDeadline::new(request.timeout_ms),
        )
        .await
        .expect_err("replay should return cached finalized response");
        assert_eq!(replay.command_id.as_deref(), Some("cmd-large-replay"));
        assert_eq!(
            replay.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcProtocolError)
        );
        assert_eq!(
            replay
                .error
                .as_ref()
                .and_then(|error| error.context.as_ref())
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("response_exceeds_ipc_frame_limit")
        );
        assert!(
            replay.data.is_none(),
            "replay cache should not preserve the oversized pre-fence success payload"
        );
    }

    #[tokio::test]
    async fn unknown_command_is_classified_as_invalid_input() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let result = super::dispatch::dispatch_named_command(
            &test_router(),
            "definitely-not-a-command",
            &serde_json::json!({}),
            TransactionDeadline::new(1_000),
            &state,
        )
        .await
        .expect_err("unknown command should fail");
        let envelope = result.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("Unknown command"));
    }

    #[tokio::test]
    async fn handshake_bypasses_fifo_when_router_is_busy() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-test"),
            None,
        ));
        let router = test_router();
        let _permit: tokio::sync::SemaphorePermit<'_> = router
            .exec_semaphore
            .acquire()
            .await
            .expect("test should hold fifo permit");

        let response = router
            .dispatch(
                IpcRequest::new("_handshake", serde_json::json!({}), 50),
                &state,
            )
            .await;
        assert_eq!(response.status, rub_ipc::protocol::ResponseStatus::Success);
        assert_eq!(
            response
                .data
                .as_ref()
                .and_then(|data| data["ipc_protocol_version"].as_str()),
            Some(rub_ipc::protocol::IPC_PROTOCOL_VERSION)
        );
    }

    #[tokio::test]
    async fn orchestration_target_dispatch_stays_out_of_user_post_commit_projection() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-internal"),
            None,
        ));
        let router = test_router();

        let response = router
            .dispatch(
                IpcRequest::new("_orchestration_target_dispatch", serde_json::json!({}), 50),
                &state,
            )
            .await;
        assert_eq!(response.status, rub_ipc::protocol::ResponseStatus::Error);
        assert_eq!(state.pending_post_commit_projection_count(), 0);
        assert!(
            state.command_history(5).await.entries.is_empty(),
            "internal orchestration transport must not leak into user history"
        );
        assert!(
            state.workflow_capture(5).await.entries.is_empty(),
            "internal orchestration transport must not leak into workflow capture"
        );
    }
}
