use crate::connection_hardening::{classify_io_transient, classify_transport_message};
use rub_core::error::ErrorEnvelope;
use rub_core::error::{ErrorCode, RubError};
use rub_ipc::client::IpcClientError;
use rub_ipc::protocol::IpcRequest;
use std::time::Instant;

use super::remaining_budget_ms;

pub(crate) fn replay_recoverable_transport_reason(
    error: &(dyn std::error::Error + 'static),
) -> Option<&'static str> {
    if let Some(client_error) = error.downcast_ref::<IpcClientError>() {
        return match client_error {
            IpcClientError::Transport(io_error) => classify_io_transient(io_error),
            IpcClientError::Protocol(envelope) => replay_recoverable_protocol_reason(envelope),
        };
    }
    error
        .downcast_ref::<std::io::Error>()
        .and_then(classify_io_transient)
        .or_else(|| classify_transport_message(&error.to_string()))
}

pub(crate) fn replay_recoverable_protocol_reason(envelope: &ErrorEnvelope) -> Option<&'static str> {
    match envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
    {
        Some("ipc_request_write_transport_failure_after_possible_commit") => {
            Some("request_write_transport_failure_after_possible_commit")
        }
        Some("ipc_request_write_timeout_after_possible_commit") => {
            Some("request_write_timeout_after_possible_commit")
        }
        Some("ipc_response_timeout_after_request_commit") => {
            Some("response_timeout_after_request_commit")
        }
        Some("ipc_response_transport_failure_after_request_commit") => {
            Some("response_transport_failure_after_request_commit")
        }
        _ => None,
    }
}

fn ipc_protocol_envelope_from_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a rub_core::error::ErrorEnvelope> {
    error
        .downcast_ref::<IpcClientError>()
        .and_then(IpcClientError::protocol_envelope)
}

fn merge_ipc_error_context(
    envelope: &rub_core::error::ErrorEnvelope,
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut context = envelope
        .context
        .clone()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(command_id) = command_id {
        context.insert("command_id".to_string(), serde_json::json!(command_id));
    }
    if let Some(extra) = extra_context
        && let Some(extra_object) = extra.as_object()
    {
        context.extend(extra_object.clone());
    }
    context
}

fn ipc_error_from_source(
    fallback_code: ErrorCode,
    prefix: &str,
    error: &(dyn std::error::Error + 'static),
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    if let Some(envelope) = ipc_protocol_envelope_from_error(error) {
        let context = merge_ipc_error_context(envelope, command_id, extra_context);
        return RubError::domain_with_context(
            envelope.code,
            format!("{prefix}: {}", envelope.message),
            serde_json::Value::Object(context),
        );
    }
    ipc_classified_error(fallback_code, prefix, error, command_id, extra_context)
}

pub(crate) fn ipc_transport_error(
    error: &(dyn std::error::Error + 'static),
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    ipc_error_from_source(
        ErrorCode::IpcProtocolError,
        "IPC error",
        error,
        command_id,
        extra_context,
    )
}

pub(crate) fn ipc_timeout_error(
    error: &(dyn std::error::Error + 'static),
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    ipc_error_from_source(
        ErrorCode::IpcTimeout,
        "IPC timeout",
        error,
        command_id,
        extra_context,
    )
}

pub(crate) fn ipc_budget_exhausted_error(
    command_id: Option<&str>,
    original_timeout_ms: u64,
    phase: &str,
) -> RubError {
    ipc_classified_error(
        ErrorCode::IpcTimeout,
        "IPC timeout",
        format!("IPC request exhausted the declared timeout budget of {original_timeout_ms}ms"),
        command_id,
        Some(serde_json::json!({
            "reason": "ipc_replay_budget_exhausted",
            "phase": phase,
            "original_timeout_ms": original_timeout_ms,
        })),
    )
}

pub(crate) fn project_request_onto_deadline(
    request: &IpcRequest,
    deadline: Instant,
) -> Option<IpcRequest> {
    let remaining_timeout_ms = remaining_budget_ms(deadline);
    if remaining_timeout_ms == 0 {
        return None;
    }

    let mut projected = request.clone();
    projected.timeout_ms = projected.timeout_ms.min(remaining_timeout_ms);
    if crate::timeout_budget::embedded_timeout_budget_ms(&projected)
        .is_some_and(|timeout_ms| timeout_ms == 0)
    {
        return None;
    }
    crate::timeout_budget::align_embedded_timeout_authority(&mut projected);
    Some(projected)
}

fn ipc_classified_error(
    code: ErrorCode,
    prefix: &str,
    error: impl std::fmt::Display,
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    let mut context = serde_json::Map::new();
    if let Some(command_id) = command_id {
        context.insert("command_id".to_string(), serde_json::json!(command_id));
    }
    if let Some(extra) = extra_context
        && let Some(extra_object) = extra.as_object()
    {
        context.extend(extra_object.clone());
    }
    RubError::domain_with_context(
        code,
        format!("{prefix}: {error}"),
        serde_json::Value::Object(context),
    )
}

#[cfg(test)]
mod tests {
    use super::replay_recoverable_transport_reason;
    use rub_ipc::client::IpcClientError;

    #[test]
    fn replay_recovery_recognizes_partial_response_transport_failures() {
        let error = IpcClientError::Transport(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "partial NDJSON frame",
        ));
        assert_eq!(
            replay_recoverable_transport_reason(&error),
            Some("unexpected_eof")
        );
    }

    #[test]
    fn replay_recovery_recognizes_post_commit_timeout_protocol_failures() {
        let error = IpcClientError::Protocol(
            rub_core::error::ErrorEnvelope::new(
                rub_core::error::ErrorCode::IpcTimeout,
                "response timed out after request commit",
            )
            .with_context(serde_json::json!({
                "reason": "ipc_response_timeout_after_request_commit",
            })),
        );
        assert_eq!(
            replay_recoverable_transport_reason(&error),
            Some("response_timeout_after_request_commit")
        );
    }

    #[test]
    fn replay_recovery_recognizes_post_commit_response_transport_protocol_failures() {
        let error = IpcClientError::Protocol(
            rub_core::error::ErrorEnvelope::new(
                rub_core::error::ErrorCode::IpcProtocolError,
                "response transport failed after request commit",
            )
            .with_context(serde_json::json!({
                "reason": "ipc_response_transport_failure_after_request_commit",
            })),
        );
        assert_eq!(
            replay_recoverable_transport_reason(&error),
            Some("response_transport_failure_after_request_commit")
        );
    }

    #[test]
    fn replay_recovery_recognizes_possible_commit_write_timeout_protocol_failures() {
        let error = IpcClientError::Protocol(
            rub_core::error::ErrorEnvelope::new(
                rub_core::error::ErrorCode::IpcTimeout,
                "request write timed out after possible commit",
            )
            .with_context(serde_json::json!({
                "reason": "ipc_request_write_timeout_after_possible_commit",
            })),
        );
        assert_eq!(
            replay_recoverable_transport_reason(&error),
            Some("request_write_timeout_after_possible_commit")
        );
    }

    #[test]
    fn replay_recovery_recognizes_possible_commit_write_transport_protocol_failures() {
        let error = IpcClientError::Protocol(
            rub_core::error::ErrorEnvelope::new(
                rub_core::error::ErrorCode::IpcProtocolError,
                "request write transport failed after possible commit",
            )
            .with_context(serde_json::json!({
                "reason": "ipc_request_write_transport_failure_after_possible_commit",
                "transport_error_kind": "BrokenPipe",
            })),
        );
        assert_eq!(
            replay_recoverable_transport_reason(&error),
            Some("request_write_transport_failure_after_possible_commit")
        );
    }
}
