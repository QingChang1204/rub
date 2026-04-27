use super::UpgradeStatus;
use super::projection::cleanup_runtime_path_state;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::SessionPaths;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::protocol::{IpcRequest, ResponseStatus, UPGRADE_CHECK_PROBE_COMMAND_ID};

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

#[cfg(test)]
pub(super) async fn fetch_upgrade_status_for_session(
    session_paths: &SessionPaths,
) -> Result<Option<(UpgradeStatus, PathBuf)>, RubError> {
    let timeout_ms = 3_000;
    fetch_upgrade_status_for_session_with_deadline(
        session_paths,
        Instant::now() + Duration::from_millis(timeout_ms),
        timeout_ms,
        "cleanup_upgrade_check",
    )
    .await
}

fn parse_upgrade_status_payload(
    socket_path: &Path,
    data: serde_json::Value,
) -> Result<UpgradeStatus, RubError> {
    let idle = data
        .get("idle")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| {
            cleanup_upgrade_status_error(
                ErrorCode::IpcProtocolError,
                "Failed to fetch upgrade status: daemon returned malformed upgrade check payload"
                    .to_string(),
                socket_path,
                Some(serde_json::json!({
                    "upgrade_check": data,
                })),
                "cleanup_upgrade_check_payload_invalid",
            )
        })?;
    Ok(UpgradeStatus { idle })
}

fn upgrade_check_probe_request(timeout_ms: u64) -> IpcRequest {
    IpcRequest::new("_upgrade_check", serde_json::json!({}), timeout_ms)
        .with_command_id(UPGRADE_CHECK_PROBE_COMMAND_ID)
        .expect("upgrade-check probe command_id must be valid")
}

async fn connect_cleanup_probe_client(
    socket_path: &Path,
    session_paths: &SessionPaths,
) -> Result<IpcClient, std::io::Error> {
    match session_paths.session_id() {
        Some(session_id) => IpcClient::connect_bound(socket_path, session_id).await,
        None => IpcClient::connect(socket_path).await,
    }
}

pub(super) async fn fetch_upgrade_status_for_session_with_deadline(
    session_paths: &SessionPaths,
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<Option<(UpgradeStatus, PathBuf)>, RubError> {
    // Teardown and upgrade checks must target the concrete runtime socket for
    // this session authority. The canonical session-name socket is a shared
    // discovery projection and may already point at a replacement daemon.
    // For session-name-only paths, actual_socket_paths() collapses to the same
    // canonical path, so this stays correct for temp-home discovery.
    for socket_path in session_paths.actual_socket_paths() {
        crate::timeout_budget::ensure_remaining_budget(deadline, timeout_ms, phase)?;
        let mut client = match crate::timeout_budget::run_with_remaining_budget(
            deadline,
            timeout_ms,
            phase,
            async {
                Ok::<_, RubError>(
                    connect_cleanup_probe_client(&socket_path, session_paths)
                        .await
                        .ok(),
                )
            },
        )
        .await?
        {
            Some(client) => client,
            None => continue,
        };
        let request =
            upgrade_check_probe_request(remaining_budget_ms(deadline, timeout_ms, phase)?);
        let response =
            crate::timeout_budget::run_with_remaining_budget(deadline, timeout_ms, phase, async {
                client
                    .send(&request)
                    .await
                    .map_err(|error| cleanup_upgrade_probe_send_error(&socket_path, error))
            })
            .await?;
        if response.status == ResponseStatus::Error {
            return Err(cleanup_upgrade_probe_response_error(&socket_path, response));
        }
        let data = response.data.unwrap_or_default();
        return Ok(Some((
            parse_upgrade_status_payload(&socket_path, data)?,
            socket_path,
        )));
    }
    Ok(None)
}

fn remaining_budget_ms(
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<u64, RubError> {
    crate::timeout_budget::remaining_budget_duration(deadline)
        .map(|remaining| remaining.as_millis().clamp(1, u64::MAX as u128) as u64)
        .ok_or_else(|| crate::main_support::command_timeout_error(timeout_ms, phase))
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

pub(super) async fn wait_for_shutdown_paths_until(
    socket_paths: &[PathBuf],
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<(), RubError> {
    for _ in 0..20 {
        if socket_paths.iter().all(|socket_path| !socket_path.exists()) {
            return Ok(());
        }
        let remaining = crate::timeout_budget::remaining_budget_duration(deadline)
            .ok_or_else(|| crate::main_support::command_timeout_error(timeout_ms, phase))?;
        tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
    }
    crate::timeout_budget::ensure_remaining_budget(deadline, timeout_ms, phase)
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_upgrade_probe_response_error, fetch_upgrade_status_for_session_with_deadline,
        parse_upgrade_status_payload, wait_for_shutdown_paths_until,
    };
    use std::path::Path;
    use std::time::Instant;

    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::Timing;
    use rub_daemon::rub_paths::RubPaths;
    use rub_ipc::codec::NdJsonCodec;
    use rub_ipc::protocol::{IpcRequest, IpcResponse, UPGRADE_CHECK_PROBE_COMMAND_ID};
    use tokio::io::BufReader;
    use tokio::net::UnixListener;

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
                daemon_session_id: None,
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

    #[test]
    fn cleanup_upgrade_probe_malformed_success_payload_is_protocol_error() {
        let error = parse_upgrade_status_payload(
            Path::new("/tmp/rub.sock"),
            serde_json::json!({
                "idle": "yes"
            }),
        )
        .expect_err("malformed success payload must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope.context.expect("context")["reason"],
            serde_json::json!("cleanup_upgrade_check_payload_invalid")
        );
    }

    #[tokio::test]
    async fn cleanup_upgrade_status_respects_shared_deadline() {
        let home = std::env::temp_dir().join(format!("rub-cleanup-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create cleanup budget home");
        let session_paths = RubPaths::new(&home).session("default");

        let error = fetch_upgrade_status_for_session_with_deadline(
            &session_paths,
            Instant::now(),
            1,
            "cleanup_upgrade_check",
        )
        .await
        .expect_err("expired cleanup deadline must fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::IpcTimeout);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn cleanup_upgrade_status_binds_probe_to_session_authority() {
        let home = std::env::temp_dir().join(format!("rub-cleanup-probe-{}", uuid::Uuid::now_v7()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create cleanup probe home");
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(
            session_paths
                .socket_path()
                .parent()
                .expect("socket path parent"),
        )
        .unwrap();
        let _ = std::fs::remove_file(session_paths.socket_path());
        let listener = UnixListener::bind(session_paths.socket_path()).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_upgrade_check");
            assert_eq!(
                request.command_id.as_deref(),
                Some(UPGRADE_CHECK_PROBE_COMMAND_ID)
            );
            assert_eq!(request.daemon_session_id.as_deref(), Some("sess-default"));
            let response = IpcResponse::success("upgrade", serde_json::json!({ "idle": true }))
                .with_command_id(UPGRADE_CHECK_PROBE_COMMAND_ID)
                .expect("probe command_id must be valid")
                .with_daemon_session_id("sess-default")
                .expect("daemon session id must be valid");
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let (status, socket_path) = fetch_upgrade_status_for_session_with_deadline(
            &session_paths,
            Instant::now() + std::time::Duration::from_secs(1),
            1_000,
            "cleanup_upgrade_check",
        )
        .await
        .expect("upgrade check should succeed")
        .expect("socket should respond");
        assert!(status.idle);
        assert_eq!(socket_path, session_paths.socket_path());

        server.await.expect("server task");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn shutdown_wait_respects_shared_deadline() {
        let socket_dir =
            std::env::temp_dir().join(format!("rub-cleanup-socket-{}", std::process::id()));
        let socket_path = socket_dir.join("live.sock");
        let _ = std::fs::remove_dir_all(&socket_dir);
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        std::fs::write(&socket_path, b"live").expect("seed pseudo socket file");

        let error = wait_for_shutdown_paths_until(
            std::slice::from_ref(&socket_path),
            Instant::now(),
            1,
            "cleanup_shutdown_wait",
        )
        .await
        .expect_err("expired cleanup deadline must fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::IpcTimeout);

        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }
}
