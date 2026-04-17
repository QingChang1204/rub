use std::sync::Arc;

use tracing::info;
use uuid::Uuid;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};

use crate::session::{ReplayCommandClaim, SessionState};
use crate::workflow_capture::WorkflowCaptureDeliveryState;

use super::TransactionDeadline;
use super::dispatch;
use super::policy::command_allowed_during_handoff;

mod fingerprint;
mod response;

pub(crate) use self::response::{
    attach_request_command_id, attach_response_metadata, daemon_authority_mismatch_response,
    enforce_response_frame_limit, execution_timeout_error, execution_timeout_response,
    finalize_post_commit_followups, finalize_replay_fence, protocol_version_mismatch_response,
    queue_timeout_response, replay_fingerprint_conflict_response, replay_request_fingerprint,
    replay_spent_response_evicted_response, replay_timeout_response,
    response_delivery_failure_response,
};

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
    execution_started: bool,
    replay_owner: Option<ReplayFenceOwner>,
}

pub(crate) struct PendingResponseCommit {
    request: IpcRequest,
    response: IpcResponse,
    internal_command: bool,
    execution_started: bool,
    replay_owner: Option<ReplayFenceOwner>,
}

pub(super) enum DispatchPreparation {
    Final(Box<PendingResponseCommit>),
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
        self.execution_started = true;
        if let Some(owner) = self.replay_owner.as_mut() {
            owner.mark_execution_started();
        }
    }

    pub(super) fn prepare_response_commit(
        self,
        request: &IpcRequest,
        response: IpcResponse,
    ) -> PendingResponseCommit {
        PendingResponseCommit::new(
            request.clone(),
            response,
            self.internal_command,
            self.execution_started,
            self.replay_owner,
        )
    }
}

impl PendingResponseCommit {
    pub(super) fn new(
        request: IpcRequest,
        mut response: IpcResponse,
        internal_command: bool,
        execution_started: bool,
        replay_owner: Option<ReplayFenceOwner>,
    ) -> Self {
        if let Some(ref owner) = replay_owner {
            response = response
                .with_command_id(owner.command_id.clone())
                .expect("validated replay command_id must remain protocol-valid");
        } else if let Some(ref cmd_id) = request.command_id {
            response = response
                .with_command_id(cmd_id.clone())
                .expect("validated request command_id must remain protocol-valid");
        }

        response = enforce_response_frame_limit(&request, response);

        Self {
            request,
            response,
            internal_command,
            execution_started,
            replay_owner,
        }
    }

    pub(crate) fn response(&self) -> &IpcResponse {
        &self.response
    }

    pub(crate) async fn commit_locally(self, state: &Arc<SessionState>) -> IpcResponse {
        let PendingResponseCommit {
            request,
            response,
            internal_command,
            execution_started,
            replay_owner,
        } = self;
        finalize_replay_fence(replay_owner.as_ref(), &response, state).await;
        if execution_started && !internal_command {
            finalize_post_commit_followups(
                &request,
                &response,
                &response,
                WorkflowCaptureDeliveryState::Delivered,
                state,
            )
            .await;
        }
        response
    }

    pub(crate) async fn commit_after_delivery(self, state: &Arc<SessionState>) {
        let PendingResponseCommit {
            request,
            response,
            internal_command,
            execution_started,
            replay_owner,
        } = self;
        finalize_replay_fence(replay_owner.as_ref(), &response, state).await;
        if execution_started && !internal_command {
            finalize_post_commit_followups(
                &request,
                &response,
                &response,
                WorkflowCaptureDeliveryState::Delivered,
                state,
            )
            .await;
        }
    }

    pub(crate) async fn commit_after_delivery_failure(
        self,
        state: &Arc<SessionState>,
        delivery_error: String,
    ) {
        if !self.execution_started {
            finalize_replay_fence(self.replay_owner.as_ref(), &self.response, state).await;
            return;
        }

        let failure_response =
            response_delivery_failure_response(&self.request, &self.response, &delivery_error);
        finalize_replay_fence(self.replay_owner.as_ref(), &failure_response, state).await;
        if !self.internal_command {
            finalize_post_commit_followups(
                &self.request,
                &failure_response,
                &self.response,
                WorkflowCaptureDeliveryState::DeliveryFailedAfterCommit,
                state,
            )
            .await;
        }
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
            ReplayCommandClaim::SpentWithoutCachedResponse => {
                return Err(attach_request_command_id(
                    request,
                    replay_spent_response_evicted_response(request, request_id, command_id),
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
) -> Result<PreparedCommandDispatch, PendingResponseCommit> {
    let RequestPreflight {
        request_id,
        deadline,
        internal_command,
    } = preflight;

    let replay_owner = match prepare_replay_fence(request, state, &request_id, deadline).await {
        Ok(owner) => owner,
        Err(response) => {
            return Err(PendingResponseCommit::new(
                request.clone(),
                response,
                internal_command,
                false,
                None,
            ));
        }
    };
    if let Some(response) = handoff_blocked_response(request, state, &request_id).await {
        return Err(PreparedCommandDispatch {
            request_id,
            deadline,
            internal_command,
            execution_started: false,
            replay_owner,
        }
        .prepare_response_commit(request, response));
    }

    Ok(PreparedCommandDispatch {
        request_id,
        deadline,
        internal_command,
        execution_started: false,
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

#[cfg(test)]
pub(super) async fn finalize_response(
    request: &IpcRequest,
    response: IpcResponse,
    internal_command: bool,
    replay_owner: Option<ReplayFenceOwner>,
    state: &Arc<SessionState>,
) -> IpcResponse {
    PendingResponseCommit::new(
        request.clone(),
        response,
        internal_command,
        true,
        replay_owner,
    )
    .commit_locally(state)
    .await
}

#[cfg(test)]
mod tests;
