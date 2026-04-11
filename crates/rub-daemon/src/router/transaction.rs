use std::sync::Arc;

use tracing::{info, warn};
use uuid::Uuid;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_ipc::codec::{MAX_FRAME_BYTES, encoded_frame_len};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};

use crate::session::{ReplayCommandClaim, SessionState};

use super::TransactionDeadline;
use super::dispatch;
use super::policy::command_allowed_during_handoff;
use super::timeout::{TimeoutPhase, timeout_context};

#[derive(Debug, Clone)]
pub(super) struct ReplayFenceOwner {
    pub(super) command_id: String,
    fingerprint: String,
    finalize: ReplayFinalizeMode,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ReplayFinalizeMode {
    ReleaseOnly,
    CacheCommittedResponse,
}

impl ReplayFenceOwner {
    pub(super) fn new(command_id: String, fingerprint: String) -> Self {
        Self {
            command_id,
            fingerprint,
            finalize: ReplayFinalizeMode::ReleaseOnly,
        }
    }

    pub(super) fn mark_execution_started(&mut self) {
        self.finalize = ReplayFinalizeMode::CacheCommittedResponse;
    }
}

pub(super) struct RequestPreflight {
    pub(super) request_id: String,
    pub(super) deadline: TransactionDeadline,
    pub(super) internal_command: bool,
}

pub(super) struct PreparedCommandDispatch {
    request_id: String,
    deadline: TransactionDeadline,
    internal_command: bool,
    replay_owner: Option<ReplayFenceOwner>,
}

pub(super) enum DispatchPreparation {
    Final(IpcResponse),
    Prepared(PreparedCommandDispatch),
}

impl PreparedCommandDispatch {
    pub(super) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(super) fn deadline(&self) -> TransactionDeadline {
        self.deadline
    }

    pub(super) fn queue_ms(&self) -> u64 {
        self.deadline.elapsed_ms()
    }

    pub(super) fn mark_execution_started(&mut self) {
        if let Some(owner) = self.replay_owner.as_mut() {
            owner.mark_execution_started();
        }
    }

    pub(super) async fn finalize(
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

pub(super) async fn prepare_replay_fence(
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

pub(super) fn prepare_request_preflight(request: &IpcRequest) -> RequestPreflight {
    let request_id = Uuid::now_v7().to_string();
    let deadline = TransactionDeadline::new(request.timeout_ms);
    let internal_command = dispatch::is_internal_command(request.command.as_str());
    RequestPreflight {
        request_id,
        deadline,
        internal_command,
    }
}

pub(super) async fn prepare_command_dispatch(
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

pub(super) fn preflight_rejection_response(
    request: &IpcRequest,
    preflight: &RequestPreflight,
    state: &Arc<SessionState>,
    in_process_dispatch: bool,
) -> Option<IpcResponse> {
    if !preflight.internal_command && request.ipc_protocol_version != IPC_PROTOCOL_VERSION {
        return Some(protocol_version_mismatch_response(
            &preflight.request_id,
            request,
        ));
    }
    if preflight.internal_command
        && dispatch::is_in_process_only_command(request.command.as_str())
        && !in_process_dispatch
    {
        return Some(IpcResponse::error(
            &preflight.request_id,
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                format!(
                    "Command '{}' is reserved for in-process dispatch only",
                    request.command
                ),
            )
            .with_context(serde_json::json!({
                "command": request.command,
                "reason": "in_process_only_internal_command",
            })),
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

pub(super) async fn finalize_response(
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
        if let Err(error) = state.record_post_commit_journal(request, &response).await {
            warn!(
                command = %request.command,
                command_id = response.command_id.as_deref().unwrap_or(""),
                request_id = %response.request_id,
                journal_failures = state.post_commit_journal_failure_count(),
                error = %error,
                "Post-commit journal append failed after the daemon commit fence"
            );
        }
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

pub(super) fn protocol_version_mismatch_response(
    request_id: &str,
    request: &IpcRequest,
) -> IpcResponse {
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

pub(super) fn queue_timeout_response(
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

pub(super) fn execution_timeout_error(
    request: &IpcRequest,
    queue_ms: u64,
    exec_budget_ms: u64,
) -> RubError {
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

pub(super) fn execution_timeout_response(
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

pub(super) fn replay_request_fingerprint(request: &IpcRequest) -> String {
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

pub(crate) fn attach_response_metadata(
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
    use super::{finalize_response, preflight_rejection_response, prepare_request_preflight};
    use crate::session::SessionState;
    use rub_ipc::protocol::{IpcRequest, IpcResponse};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn unique_home(label: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "rub-post-commit-journal-{label}-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        home
    }

    #[tokio::test]
    async fn finalize_response_appends_redacted_post_commit_journal() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let home = unique_home("success");
            std::fs::create_dir_all(&home).expect("create rub_home");
            let secrets = home.join("secrets.env");
            std::fs::write(&secrets, "RUB_TOKEN=token-123\n").expect("write secrets");
            std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600))
                .expect("set permissions");

            let state = Arc::new(SessionState::new("default", home.clone(), None));
            let request = IpcRequest::new(
                "type",
                serde_json::json!({ "selector": "#password", "text": "token-123", "clear": true }),
                1_000,
            )
            .with_command_id("cmd-1")
            .expect("static command_id must be valid");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "echo": "token-123",
                    "ok": true
                }),
            );

            let committed = finalize_response(&request, response, false, None, &state).await;
            let history = state.command_history(5).await;
            let journal = state
                .read_post_commit_journal_entries_for_tests()
                .expect("read journal");

            assert_eq!(committed.command_id.as_deref(), Some("cmd-1"));
            assert_eq!(history.entries.len(), 1);
            assert_eq!(journal.len(), 1);
            assert_eq!(
                journal[0]["journal_state"]["commit_relation"],
                serde_json::json!("downstream_of_daemon_commit_fence")
            );
            assert_eq!(
                journal[0]["journal_state"]["durability"],
                serde_json::json!("durable")
            );
            assert_eq!(journal[0]["command"], serde_json::json!("type"));
            assert_eq!(journal[0]["command_id"], serde_json::json!("cmd-1"));
            assert_eq!(
                journal[0]["request"]["args"]["text"],
                serde_json::json!("$RUB_TOKEN")
            );
            assert_eq!(
                journal[0]["response"]["data"]["echo"],
                serde_json::json!("$RUB_TOKEN")
            );
            assert_eq!(
                journal[0]["response"]["request_id"],
                serde_json::json!("req-1")
            );

            let _ = std::fs::remove_dir_all(home);
        }
    }

    #[tokio::test]
    async fn finalize_response_journal_failure_does_not_rewrite_commit_truth() {
        let home = unique_home("failure");
        std::fs::create_dir_all(&home).expect("create rub_home");
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        let request = IpcRequest::new(
            "open",
            serde_json::json!({ "url": "https://example.com" }),
            1_000,
        )
        .with_command_id("cmd-2")
        .expect("static command_id must be valid");
        let response = IpcResponse::success("req-2", serde_json::json!({ "ok": true }));

        state.force_post_commit_journal_failure_once();
        let committed = finalize_response(&request, response, false, None, &state).await;
        let history = state.command_history(5).await;

        assert_eq!(committed.command_id.as_deref(), Some("cmd-2"));
        assert!(committed.data.is_some());
        assert_eq!(state.post_commit_journal_failure_count(), 1);
        assert_eq!(
            state
                .read_post_commit_journal_entries_for_tests()
                .expect("read journal")
                .len(),
            0
        );
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].command, "open");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn external_preflight_rejects_in_process_only_internal_commands() {
        let state = Arc::new(SessionState::new(
            "default",
            unique_home("in-process-only"),
            None,
        ));
        let request = IpcRequest::new("_trigger_pipe", serde_json::json!({ "spec": "[]" }), 1_000);
        let preflight = prepare_request_preflight(&request);

        let external = preflight_rejection_response(&request, &preflight, &state, false)
            .expect("external dispatch should reject in-process-only command");
        assert_eq!(
            external
                .error
                .as_ref()
                .and_then(|error| error.context.as_ref())
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("in_process_only_internal_command")
        );

        assert!(
            preflight_rejection_response(&request, &preflight, &state, true).is_none(),
            "in-process dispatch should retain authority to call reserved trigger wrappers"
        );
    }
}
