use crate::connection_hardening::{
    AttemptError, RetryFailure, RetryPolicy, classify_error_code, classify_transport_message,
    run_with_bounded_retry,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::LaunchPolicyInfo;
use rub_ipc::client::IpcClient;
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
    let response = client.send(&request).await.map_err(|error| {
        if let Some(envelope) = error.protocol_envelope() {
            RubError::domain_with_context(
                envelope.code,
                format!("Failed to fetch handshake info: {}", envelope.message),
                envelope
                    .context
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({})),
            )
        } else {
            RubError::domain(
                ErrorCode::IpcProtocolError,
                format!("Failed to fetch handshake info: {error}"),
            )
        }
    })?;

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

pub(crate) fn handshake_attempt_error(error: RubError) -> AttemptError {
    let envelope = error.into_envelope();
    if let Some(reason) = classify_transport_message(&envelope.message) {
        AttemptError::retryable(RubError::Domain(envelope), reason)
    } else {
        AttemptError::terminal(
            RubError::Domain(envelope.clone()),
            classify_error_code(envelope.code),
        )
    }
}
