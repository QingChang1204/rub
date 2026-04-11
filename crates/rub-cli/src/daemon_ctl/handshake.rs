use crate::connection_hardening::{
    AttemptError, RetryFailure, RetryPolicy, classify_error_code, classify_io_transient,
    run_with_bounded_retry,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::LaunchPolicyInfo;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use std::path::Path;

use super::{connect_ipc_with_retry, preferred_socket_path_for_session};

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct HandshakePayload {
    pub(crate) daemon_session_id: String,
    pub(crate) launch_policy: LaunchPolicyInfo,
}

pub(crate) async fn fetch_launch_policy(
    client: &mut IpcClient,
) -> Result<LaunchPolicyInfo, RubError> {
    Ok(fetch_handshake_info(client).await?.launch_policy)
}

pub(crate) async fn fetch_handshake_info(
    client: &mut IpcClient,
) -> Result<HandshakePayload, RubError> {
    fetch_handshake_info_with_timeout(client, 3_000).await
}

pub(crate) async fn fetch_handshake_info_with_timeout(
    client: &mut IpcClient,
    timeout_ms: u64,
) -> Result<HandshakePayload, RubError> {
    let request = IpcRequest::new("_handshake", serde_json::json!({}), timeout_ms.max(1));
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

    let data = response.data.unwrap_or_default();
    serde_json::from_value(data).map_err(|e| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            format!("Invalid handshake payload: {e}"),
        )
    })
}

pub(crate) async fn fetch_launch_policy_for_session(
    rub_home: &Path,
    session: &str,
) -> Result<LaunchPolicyInfo, RubError> {
    let socket_path = preferred_socket_path_for_session(rub_home, session)?;
    let (launch_policy, _attribution) = run_with_bounded_retry(RetryPolicy::default(), || async {
        let (mut client, _connect_attr) = connect_ipc_with_retry(
            &socket_path,
            ErrorCode::IpcProtocolError,
            format!("Failed to connect to session '{session}' for launch policy check"),
            "daemon_ctl.launch_policy.socket_path",
            "preferred_socket_path_for_session",
        )
        .await
        .map_err(RetryFailure::into_attempt_error)?;
        fetch_launch_policy(&mut client)
            .await
            .map_err(handshake_attempt_error)
    })
    .await
    .map_err(RetryFailure::into_error)?;
    Ok(launch_policy)
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
