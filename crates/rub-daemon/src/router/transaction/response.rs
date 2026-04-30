use std::sync::Arc;

use tracing::warn;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::Timing;
use rub_core::recovery_contract::already_executed_response_evicted_do_not_rerun_contract;
use rub_ipc::codec::{MAX_FRAME_BYTES, encoded_frame_len};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};

use crate::router::timeout::{TimeoutPhase, timeout_context};
use crate::router::timeout_projection::merge_timeout_projection_context;
use crate::session::SessionState;
use crate::workflow_capture::WorkflowCaptureDeliveryState;

use super::fingerprint;
use super::{ReplayFenceOwner, ReplayFinalizeMode, TransactionDeadline};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum PostCommitProjectionFence {
    Detached,
    Synchronous,
}

pub(crate) async fn finalize_post_commit_followups(
    request: &IpcRequest,
    committed_response: &IpcResponse,
    workflow_capture_response: &IpcResponse,
    workflow_capture_delivery_state: WorkflowCaptureDeliveryState,
    projection_fence: PostCommitProjectionFence,
    state: &Arc<SessionState>,
) {
    if let Err(error) = state
        .record_post_commit_journal(request, committed_response, workflow_capture_delivery_state)
        .await
    {
        warn!(
            command = %request.command,
            command_id = committed_response.command_id.as_deref().unwrap_or(""),
            request_id = %committed_response.request_id,
            journal_failures = state.post_commit_journal_failure_count(),
            error = %error,
            "Post-commit journal append failed after the daemon commit fence"
        );
    }
    state.submit_post_commit_projection_with_capture(
        request,
        committed_response,
        workflow_capture_response,
        workflow_capture_delivery_state,
    );
    match projection_fence {
        PostCommitProjectionFence::Detached => state.spawn_post_commit_projection_drain(),
        PostCommitProjectionFence::Synchronous => state.drain_post_commit_projections().await,
    }
}

pub(crate) async fn finalize_replay_fence(
    replay_owner: Option<&mut ReplayFenceOwner>,
    response: &IpcResponse,
) {
    let Some(owner) = replay_owner else {
        return;
    };
    let command_id = owner.command_id.clone();
    let fingerprint = owner.fingerprint.clone();
    let finalize = owner.finalize;
    let state = owner.state.clone();
    if finalize == ReplayFinalizeMode::CacheCommittedResponse {
        state.mark_replay_command_spent(&command_id, &fingerprint);
        state
            .cache_response(command_id.clone(), fingerprint, response.clone())
            .await;
    }
    state.release_replay_command(&command_id);
    owner.mark_finalized();
}

pub(crate) fn enforce_response_frame_limit(
    request: &IpcRequest,
    response: IpcResponse,
    daemon_request_committed: bool,
) -> IpcResponse {
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
            "daemon_request_committed": daemon_request_committed,
            "safe_to_rerun_with_new_command_id": !daemon_request_committed,
            "recovery_authority": if daemon_request_committed {
                "replay_same_command_id_or_reduce_response_projection"
            } else {
                "request_not_committed"
            },
        })),
    )
    .with_timing(response.timing);
    if let Some(command_id) = response.command_id.as_ref() {
        overflow = overflow
            .with_command_id(command_id.clone())
            .expect("validated command_id must remain protocol-valid");
    }
    if let Some(daemon_session_id) = response.daemon_session_id.as_ref() {
        overflow = overflow
            .with_daemon_session_id(daemon_session_id.clone())
            .expect("validated daemon_session_id must remain protocol-valid");
    }
    overflow
}

pub(crate) fn attach_request_command_id(
    request: &IpcRequest,
    response: IpcResponse,
) -> IpcResponse {
    if let Some(command_id) = request.command_id.as_ref() {
        response
            .with_command_id(command_id.clone())
            .expect("validated request command_id must remain protocol-valid")
    } else {
        response
    }
}

pub(crate) fn protocol_version_mismatch_response(
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

pub(crate) fn daemon_authority_mismatch_response(
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

pub(crate) fn queue_timeout_response(
    command: &str,
    request_id: &str,
    deadline: TransactionDeadline,
) -> IpcResponse {
    let queue_ms = deadline.elapsed_ms_bounded_by_timeout();
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
    .with_timing(Timing {
        queue_ms,
        exec_ms: 0,
        total_ms: queue_ms,
    })
}

pub(crate) fn execution_timeout_error(
    request: &IpcRequest,
    queue_ms: u64,
    exec_budget_ms: u64,
    transaction_timeout_ms: u64,
    partial_commit_projection: Option<serde_json::Value>,
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
    let mut envelope = ErrorEnvelope::new(code, msg);
    envelope.context = merge_timeout_projection_context(
        Some(timeout_context(
            request.command.as_str(),
            TimeoutPhase::Execution,
            transaction_timeout_ms,
            queue_ms,
            Some(exec_budget_ms),
        )),
        partial_commit_projection,
    );
    RubError::Domain(envelope)
}

pub(crate) fn execution_timeout_response(
    request: &IpcRequest,
    request_id: &str,
    queue_ms: u64,
    exec_budget_ms: u64,
    transaction_timeout_ms: u64,
) -> IpcResponse {
    IpcResponse::error(
        request_id,
        execution_timeout_error(
            request,
            queue_ms,
            exec_budget_ms,
            transaction_timeout_ms,
            None,
        )
        .into_envelope(),
    )
}

pub(crate) fn replay_timeout_response(
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
            "transaction_timeout_ms": deadline.timeout_ms,
            "elapsed_ms": deadline.elapsed_ms(),
            "reason": "replay_fence_wait_timeout",
        })),
    )
}

pub(crate) fn replay_request_fingerprint(request: &IpcRequest) -> String {
    fingerprint::replay_request_fingerprint(request)
}

pub(crate) fn replay_fingerprint_conflict_response(
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

pub(crate) fn replay_spent_response_evicted_response(
    request: &IpcRequest,
    request_id: &str,
    command_id: &str,
) -> IpcResponse {
    IpcResponse::error(
        request_id,
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "command_id '{command_id}' already entered execution for '{}' but the original response is no longer retained",
                request.command
            ),
        )
        .with_suggestion(
            "Do not rerun this command with a new command_id unless the action is known to be safe. Inspect command history or doctor if you need the original outcome.",
        )
        .with_context(serde_json::json!({
            "command": request.command,
            "command_id": command_id,
            "reason": "replay_command_id_already_spent_original_response_evicted",
            "original_response_retained": false,
            "safe_to_rerun": false,
            "recovery_contract": already_executed_response_evicted_do_not_rerun_contract(),
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
