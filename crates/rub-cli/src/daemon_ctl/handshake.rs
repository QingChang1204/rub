use crate::connection_hardening::{AttemptError, classify_error_code, classify_io_transient};
use crate::main_support::command_timeout_error;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::LaunchPolicyInfo;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID;
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use std::time::Instant;
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct HandshakePayload {
    pub(crate) daemon_session_id: String,
    pub(crate) ipc_protocol_version: String,
    pub(crate) launch_policy: LaunchPolicyInfo,
    pub(crate) attachment_identity: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RawHandshakePayload {
    #[serde(default)]
    daemon_session_id: Option<String>,
    pub(crate) launch_policy: LaunchPolicyInfo,
    pub(crate) attachment_identity: Option<String>,
}

pub(crate) async fn fetch_handshake_info(
    client: &mut IpcClient,
) -> Result<HandshakePayload, RubError> {
    fetch_handshake_info_with_timeout(client, 3_000).await
}

pub(crate) async fn fetch_handshake_info_until(
    client: &mut IpcClient,
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<HandshakePayload, RubError> {
    let remaining_timeout_ms = super::remaining_budget_ms(deadline);
    if remaining_timeout_ms == 0 {
        return Err(command_timeout_error(timeout_ms, phase));
    }
    fetch_handshake_info_with_timeout(client, remaining_timeout_ms.max(1)).await
}

pub(crate) async fn fetch_handshake_info_with_timeout(
    client: &mut IpcClient,
    timeout_ms: u64,
) -> Result<HandshakePayload, RubError> {
    let request = IpcRequest::new("_handshake", serde_json::json!({}), timeout_ms.max(1))
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::IpcProtocolError,
                format!("Failed to construct handshake probe request: {error}"),
                serde_json::json!({
                    "reason": "handshake_probe_command_id_bind_failed",
                    "command_id": HANDSHAKE_PROBE_COMMAND_ID,
                }),
            )
        })?;
    let response = client.send(&request).await.map_err(handshake_send_error)?;

    if response.status == ResponseStatus::Error {
        let envelope = response.error.unwrap_or_else(|| {
            rub_core::error::ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                "Handshake returned an empty error envelope",
            )
        });
        return Err(RubError::Domain(envelope));
    }

    let echoed_daemon_session_id = response.daemon_session_id.clone().ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "Handshake response did not carry protocol-level daemon authority",
            serde_json::json!({
                "reason": "handshake_missing_protocol_daemon_session_id",
            }),
        )
    })?;

    let ipc_protocol_version = response.ipc_protocol_version.clone();
    let data = response.data.unwrap_or_default();
    let payload: RawHandshakePayload = serde_json::from_value(data).map_err(|e| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            format!("Invalid handshake payload: {e}"),
        )
    })?;
    if let Some(payload_daemon_session_id) = payload.daemon_session_id.as_deref()
        && payload_daemon_session_id != echoed_daemon_session_id
    {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "Handshake daemon authority diverged between protocol echo and payload metadata",
            serde_json::json!({
                "reason": "handshake_daemon_session_id_mismatch",
                "protocol_daemon_session_id": echoed_daemon_session_id,
                "payload_daemon_session_id": payload_daemon_session_id,
            }),
        ));
    }
    Ok(HandshakePayload {
        daemon_session_id: echoed_daemon_session_id,
        ipc_protocol_version,
        launch_policy: payload.launch_policy,
        attachment_identity: payload.attachment_identity,
    })
}

fn handshake_send_error(error: IpcClientError) -> RubError {
    match error {
        IpcClientError::Protocol(envelope) => RubError::domain_with_context(
            envelope.code,
            format!("Failed to fetch handshake info: {}", envelope.message),
            envelope.context.unwrap_or_else(|| serde_json::json!({})),
        ),
        IpcClientError::Transport(io_error) => {
            let mut context = serde_json::Map::from_iter([(
                "reason".to_string(),
                serde_json::json!("handshake_transport_failed"),
            )]);
            if let Some(transport_reason) = classify_io_transient(&io_error) {
                context.insert(
                    "transport_reason".to_string(),
                    serde_json::json!(transport_reason),
                );
            }
            RubError::domain_with_context(
                ErrorCode::IpcProtocolError,
                format!("Failed to fetch handshake info: {io_error}"),
                serde_json::Value::Object(context),
            )
        }
    }
}

fn handshake_transport_reason(error: &RubError) -> Option<String> {
    match error {
        RubError::Domain(envelope) => envelope
            .context
            .as_ref()
            .and_then(|context| context.get("transport_reason"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        _ => None,
    }
}

pub(crate) fn handshake_attempt_error(error: RubError) -> AttemptError {
    if let Some(reason) = handshake_transport_reason(&error) {
        return AttemptError::retryable(error, reason);
    }
    let envelope = error.into_envelope();
    AttemptError::terminal(
        RubError::Domain(envelope.clone()),
        classify_error_code(envelope.code),
    )
}

#[cfg(test)]
mod tests {
    use super::{handshake_attempt_error, handshake_send_error};
    use crate::connection_hardening::ConnectionFailureClass;
    use rub_ipc::client::IpcClientError;

    #[test]
    fn handshake_attempt_error_prefers_transport_reason_context() {
        let error = handshake_send_error(IpcClientError::Transport(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "partial NDJSON frame",
        )));

        let attempt = handshake_attempt_error(error);
        assert_eq!(attempt.transient_reason.as_deref(), Some("unexpected_eof"));
        assert_eq!(
            attempt.final_failure_class,
            ConnectionFailureClass::TransportTransient
        );
    }
}
