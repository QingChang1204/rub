use std::sync::Arc;

use tracing::info;
use uuid::Uuid;

use crate::router::DaemonRouter;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::IpcRequest;

use super::IPC_READ_TIMEOUT;

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
                let _ = NdJsonCodec::write(&mut writer, &response).await;
                return Ok(());
            }
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                let response = protocol_read_failure_response(read_failure_envelope(error));
                let _ = NdJsonCodec::write(&mut writer, &response).await;
                return Ok(());
            }
        };
    let Some(request) = request else {
        return Ok(());
    };

    let _connected_client = ConnectedClientGuard::new(state);
    info!(command = %request.command, command_id = ?request.command_id, "Received request");

    let response = router.dispatch(request, state).await;
    NdJsonCodec::write(&mut writer, &response).await?;

    Ok(())
}

struct ConnectedClientGuard<'a> {
    state: &'a Arc<SessionState>,
}

impl<'a> ConnectedClientGuard<'a> {
    fn new(state: &'a Arc<SessionState>) -> Self {
        state
            .connected_client_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self { state }
    }
}

impl Drop for ConnectedClientGuard<'_> {
    fn drop(&mut self) {
        self.state
            .connected_client_count
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
                        if io_error
                            .to_string()
                            .contains("NDJSON frame exceeds maximum on-wire size") =>
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
