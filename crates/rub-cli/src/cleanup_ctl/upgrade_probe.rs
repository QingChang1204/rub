use super::UpgradeStatus;
use super::projection::cleanup_runtime_path_state;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::SessionPaths;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};

pub(super) fn cleanup_upgrade_status_error(
    code: ErrorCode,
    message: String,
    socket_path: &Path,
    existing_context: Option<serde_json::Value>,
    reason: &str,
) -> RubError {
    let mut context = existing_context
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert(
        "socket_path".to_string(),
        serde_json::json!(socket_path.display().to_string()),
    );
    context.insert(
        "socket_path_state".to_string(),
        serde_json::json!(cleanup_runtime_path_state(
            "cli.cleanup.upgrade_check.socket_path",
            "cleanup_session_runtime_socket",
            "session_socket",
        )),
    );
    context.insert("reason".to_string(), serde_json::json!(reason));
    RubError::domain_with_context(code, message, serde_json::Value::Object(context))
}

pub(super) fn registry_entry_for_home_session_id(
    rub_home: &Path,
    session_id: &str,
) -> Option<rub_daemon::session::RegistryEntry> {
    rub_daemon::session::read_registry(rub_home)
        .ok()?
        .sessions
        .into_iter()
        .find(|entry| entry.session_id == session_id)
}

pub(super) async fn fetch_upgrade_status_for_session(
    session_paths: &SessionPaths,
) -> Result<Option<(UpgradeStatus, PathBuf)>, RubError> {
    // Teardown and upgrade checks must target the concrete runtime socket for
    // this session authority. The canonical session-name socket is a shared
    // discovery projection and may already point at a replacement daemon.
    // For session-name-only paths, actual_socket_paths() collapses to the same
    // canonical path, so this stays correct for temp-home discovery.
    for socket_path in session_paths.actual_socket_paths() {
        let mut client = match IpcClient::connect(&socket_path).await {
            Ok(client) => client,
            Err(_) => continue,
        };
        let request = IpcRequest::new("_upgrade_check", serde_json::json!({}), 3_000);
        let response = client
            .send(&request)
            .await
            .map_err(|error| cleanup_upgrade_probe_send_error(&socket_path, error))?;
        if response.status == ResponseStatus::Error {
            return Err(cleanup_upgrade_probe_response_error(&socket_path, response));
        }
        let data = response.data.unwrap_or_default();
        return Ok(Some((
            UpgradeStatus {
                idle: data["idle"].as_bool().unwrap_or(false),
            },
            socket_path,
        )));
    }
    Ok(None)
}

fn cleanup_upgrade_probe_response_error(
    socket_path: &Path,
    response: rub_ipc::protocol::IpcResponse,
) -> RubError {
    if let Some(envelope) = response.error {
        return cleanup_upgrade_status_error(
            envelope.code,
            format!("Failed to fetch upgrade status: {}", envelope.message),
            socket_path,
            envelope.context,
            "cleanup_upgrade_check_response_error",
        );
    }
    cleanup_upgrade_status_error(
        ErrorCode::IpcProtocolError,
        "Failed to fetch upgrade status: daemon returned error status without envelope".to_string(),
        socket_path,
        None,
        "cleanup_upgrade_check_response_missing_error_envelope",
    )
}

fn cleanup_upgrade_probe_send_error(socket_path: &Path, error: IpcClientError) -> RubError {
    match error {
        IpcClientError::Protocol(envelope) => cleanup_upgrade_status_error(
            envelope.code,
            format!("Failed to fetch upgrade status: {}", envelope.message),
            socket_path,
            envelope.context,
            "cleanup_upgrade_check_protocol_failed",
        ),
        IpcClientError::Transport(io_error) => {
            let mut context = serde_json::Map::new();
            if let Some(transport_reason) =
                crate::connection_hardening::classify_io_transient(&io_error)
            {
                context.insert(
                    "transport_reason".to_string(),
                    serde_json::json!(transport_reason),
                );
            }
            cleanup_upgrade_status_error(
                ErrorCode::IpcProtocolError,
                io_error.to_string(),
                socket_path,
                Some(serde_json::Value::Object(context)),
                "cleanup_upgrade_check_transport_failed",
            )
        }
    }
}

pub(super) async fn wait_for_shutdown_paths(socket_paths: &[PathBuf]) {
    for _ in 0..20 {
        if socket_paths.iter().all(|socket_path| !socket_path.exists()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::cleanup_upgrade_probe_response_error;
    use std::path::Path;

    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::Timing;

    #[test]
    fn upgrade_probe_error_response_preserves_socket_path_context() {
        let error = cleanup_upgrade_probe_response_error(
            Path::new("/tmp/rub.sock"),
            rub_ipc::protocol::IpcResponse::error(
                "req-1",
                ErrorEnvelope::new(ErrorCode::DaemonNotRunning, "daemon unavailable").with_context(
                    serde_json::json!({
                        "upstream": "context"
                    }),
                ),
            ),
        )
        .into_envelope();
        let context = error.context.expect("context");
        assert_eq!(context["upstream"], serde_json::json!("context"));
        assert_eq!(
            context["reason"],
            serde_json::json!("cleanup_upgrade_check_response_error")
        );
        assert_eq!(context["socket_path"], serde_json::json!("/tmp/rub.sock"));
    }

    #[test]
    fn upgrade_probe_missing_error_envelope_is_protocol_error() {
        let error = cleanup_upgrade_probe_response_error(
            Path::new("/tmp/rub.sock"),
            rub_ipc::protocol::IpcResponse {
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                command_id: None,
                request_id: "req-2".to_string(),
                status: rub_ipc::protocol::ResponseStatus::Error,
                data: None,
                error: None,
                timing: Timing::default(),
            },
        )
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["reason"],
            serde_json::json!("cleanup_upgrade_check_response_missing_error_envelope")
        );
    }
}
