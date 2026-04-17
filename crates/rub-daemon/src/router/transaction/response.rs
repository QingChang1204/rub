use std::sync::Arc;

use tracing::warn;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_ipc::codec::{MAX_FRAME_BYTES, encoded_frame_len};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};

use crate::router::timeout::{TimeoutPhase, timeout_context};
use crate::session::SessionState;
use crate::workflow_capture::WorkflowCaptureDeliveryState;

use super::fingerprint;
use super::{ReplayFenceOwner, ReplayFinalizeMode, TransactionDeadline};

pub(crate) async fn finalize_post_commit_followups(
    request: &IpcRequest,
    response: &IpcResponse,
    workflow_capture_response: &IpcResponse,
    workflow_capture_delivery_state: WorkflowCaptureDeliveryState,
    state: &Arc<SessionState>,
) {
    if let Err(error) = state.record_post_commit_journal(request, response).await {
        warn!(
            command = %request.command,
            command_id = response.command_id.as_deref().unwrap_or(""),
            request_id = %response.request_id,
            journal_failures = state.post_commit_journal_failure_count(),
            error = %error,
            "Post-commit journal append failed after the daemon commit fence"
        );
    }
    state.submit_post_commit_projection_with_capture(
        request,
        response,
        workflow_capture_response,
        workflow_capture_delivery_state,
    );
    state.spawn_post_commit_projection_drain();
}

pub(crate) async fn finalize_replay_fence(
    replay_owner: Option<&ReplayFenceOwner>,
    response: &IpcResponse,
    state: &Arc<SessionState>,
) {
    let Some(owner) = replay_owner else {
        return;
    };
    if owner.finalize == ReplayFinalizeMode::CacheCommittedResponse {
        state.mark_replay_command_spent(&owner.command_id, &owner.fingerprint);
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

pub(crate) fn enforce_response_frame_limit(
    request: &IpcRequest,
    response: IpcResponse,
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

pub(crate) fn response_delivery_failure_response(
    request: &IpcRequest,
    response: &IpcResponse,
    delivery_error: &str,
) -> IpcResponse {
    let mut failure = IpcResponse::error(
        response.request_id.clone(),
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Command '{}' committed in the daemon, but NDJSON response delivery failed: {}",
                request.command, delivery_error
            ),
        )
        .with_context(serde_json::json!({
            "command": request.command,
            "reason": "ipc_response_delivery_failed_after_execution_commit",
            "phase": "ipc_response_write",
            "original_status": response.status,
            "upstream_commit_truth": "daemon_execution_committed",
            "client_visible_commit": "response_not_delivered",
            "recovery_contract": if response.command_id.is_some() {
                "retry_same_command_id_receives_cached_delivery_failure"
            } else {
                "no_replay_command_id"
            },
        }))
        .with_suggestion(
            "Previous execution may already have committed in the daemon. Retry with the same command_id or inspect history/doctor if state is unclear.",
        ),
    )
    .with_timing(response.timing);
    if let Some(command_id) = response.command_id.as_ref() {
        failure = failure
            .with_command_id(command_id.clone())
            .expect("validated command_id must remain protocol-valid");
    }
    failure
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

pub(crate) fn execution_timeout_error(
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

pub(crate) fn execution_timeout_response(
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
            "transaction_timeout_ms": request.timeout_ms,
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
            "recovery_contract": "already_executed_response_evicted_do_not_rerun",
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
