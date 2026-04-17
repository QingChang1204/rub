use std::sync::Arc;

use tracing::info;
use uuid::Uuid;

use crate::router::DaemonRouter;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::IpcRequest;
use rub_ipc::protocol::IpcResponse;

use super::{IPC_READ_TIMEOUT, IPC_WRITE_TIMEOUT};

/// Handle a single IPC connection from a CLI client.
pub(super) async fn handle_connection(
    stream: tokio::net::UnixStream,
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    handle_connection_inner(stream, router, &state).await
}

async fn handle_connection_inner(
    stream: tokio::net::UnixStream,
    router: Arc<DaemonRouter>,
    state: &Arc<SessionState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = tokio::io::BufReader::new(reader);

    let request: Option<IpcRequest> =
        match tokio::time::timeout(IPC_READ_TIMEOUT, NdJsonCodec::read(&mut buf_reader)).await {
            Err(_) => {
                let response = protocol_read_failure_response(
                    ErrorEnvelope::new(
                        ErrorCode::IpcTimeout,
                        format!(
                            "Timed out waiting for an NDJSON request commit fence after {}s",
                            IPC_READ_TIMEOUT.as_secs()
                        ),
                    )
                    .with_context(serde_json::json!({
                        "phase": "ipc_read",
                        "reason": "ipc_read_timeout",
                    })),
                );
                let _ = write_response_with_timeout(&mut writer, &response).await;
                return Ok(());
            }
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                let response = protocol_read_failure_response(read_failure_envelope(error));
                let _ = write_response_with_timeout(&mut writer, &response).await;
                return Ok(());
            }
        };
    let Some(request) = request else {
        return Ok(());
    };

    info!(command = %request.command, command_id = ?request.command_id, "Received request");

    let pending = router.dispatch_for_external_delivery(request, state).await;
    match write_response_with_timeout(&mut writer, pending.response()).await {
        Ok(()) => {
            pending.commit_after_delivery(state).await;
        }
        Err(error) => {
            pending
                .commit_after_delivery_failure(state, error.to_string())
                .await;
            return Err(error);
        }
    }

    Ok(())
}

pub(super) async fn write_response_with_timeout<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    response: &IpcResponse,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_response_with_timeout_duration(writer, response, IPC_WRITE_TIMEOUT).await
}

async fn write_response_with_timeout_duration<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    response: &IpcResponse,
    timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match tokio::time::timeout(timeout, NdJsonCodec::write(writer, response)).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "Timed out waiting for an NDJSON response commit fence after {}s",
                timeout.as_secs()
            ),
        )
        .into()),
    }
}

pub(super) struct ConnectedClientGuard {
    state: Arc<SessionState>,
}

impl ConnectedClientGuard {
    pub(super) fn new(state: Arc<SessionState>) -> Self {
        state
            .connected_client_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self { state }
    }
}

impl Drop for ConnectedClientGuard {
    fn drop(&mut self) {
        self.state
            .connected_client_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(super) struct PreRequestResponseFenceGuard {
    state: Arc<SessionState>,
}

impl PreRequestResponseFenceGuard {
    pub(super) fn new(state: Arc<SessionState>) -> Self {
        state
            .pre_request_response_fence_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self { state }
    }
}

impl Drop for PreRequestResponseFenceGuard {
    fn drop(&mut self) {
        self.state
            .pre_request_response_fence_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(super) fn protocol_read_failure_response(
    envelope: ErrorEnvelope,
) -> rub_ipc::protocol::IpcResponse {
    rub_ipc::protocol::IpcResponse::error(Uuid::now_v7().to_string(), envelope)
}

pub(super) fn read_failure_envelope(
    error: Box<dyn std::error::Error + Send + Sync>,
) -> ErrorEnvelope {
    match error.downcast::<rub_ipc::protocol::IpcProtocolDecodeError>() {
        Ok(protocol_error) => protocol_error.into_envelope(),
        Err(error) => match error.downcast::<std::io::Error>() {
            Ok(io_error) => {
                let reason = match io_error.kind() {
                    std::io::ErrorKind::UnexpectedEof => "partial_ndjson_frame",
                    std::io::ErrorKind::InvalidData
                        if rub_ipc::codec::is_oversized_frame_io_error(io_error.as_ref()) =>
                    {
                        "oversized_ndjson_frame"
                    }
                    std::io::ErrorKind::InvalidData => "invalid_ndjson_frame",
                    _ => "ipc_read_failure",
                };
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("Invalid NDJSON request: {io_error}"),
                )
                .with_context(serde_json::json!({
                    "phase": "ipc_read",
                    "reason": reason,
                }))
            }
            Err(error) => match error.downcast::<serde_json::Error>() {
                Ok(json_error) => ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("Invalid JSON request body: {json_error}"),
                )
                .with_context(serde_json::json!({
                    "phase": "ipc_read",
                    "reason": "invalid_json_request",
                })),
                Err(error) => ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("Failed to parse IPC request: {error}"),
                )
                .with_context(serde_json::json!({
                    "phase": "ipc_read",
                    "reason": "ipc_read_failure",
                })),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{PreRequestResponseFenceGuard, write_response_with_timeout_duration};
    use crate::session::SessionState;
    use rub_ipc::protocol::{IpcResponse, ResponseStatus};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn response_write_timeout_bounds_stalled_writer_fence() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let response = IpcResponse {
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            request_id: "req-timeout".to_string(),
            command_id: None,
            status: ResponseStatus::Success,
            timing: rub_core::model::Timing::default(),
            data: Some(serde_json::json!({
                "result": "x".repeat(2048),
            })),
            error: None,
        };

        let error = write_response_with_timeout_duration(
            &mut writer,
            &response,
            std::time::Duration::from_millis(1),
        )
        .await
        .expect_err("stalled response writer should time out");

        let io_error = error
            .downcast::<std::io::Error>()
            .expect("timeout helper should return an io::Error");
        assert_eq!(io_error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn pre_request_response_guard_tracks_shutdown_fence_until_drop() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-test"),
            None,
        ));
        let guard = PreRequestResponseFenceGuard::new(state.clone());
        assert_eq!(
            state
                .pre_request_response_fence_count
                .load(Ordering::SeqCst),
            1
        );
        drop(guard);
        assert_eq!(
            state
                .pre_request_response_fence_count
                .load(Ordering::SeqCst),
            0
        );
    }
}
