use std::sync::Arc;

use tracing::info;
use uuid::Uuid;

use crate::router::DaemonRouter;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use rub_ipc::request_correlation::RequestCorrelation;

use super::{IPC_READ_TIMEOUT, IPC_WRITE_TIMEOUT};

const PRE_FRAMING_CORRELATION_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(75);

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

    let request = match read_request_for_live_ingress(&mut buf_reader).await {
        Ok(request) => request,
        Err(error) => {
            let response = protocol_read_failure_response_with_correlation(
                error.envelope,
                error.correlation,
                Some(state.session_id.as_str()),
            );
            let _ = write_response_with_timeout(&mut writer, &response).await;
            return Ok(());
        }
    };
    let Some(request) = request else {
        return Ok(());
    };

    info!(command = %request.command, command_id = ?request.command_id, "Received request");

    let pending = router.dispatch_for_external_delivery(request, state).await;
    let response = pending.response_for_transport(&state.session_id);
    match write_response_with_timeout(&mut writer, &response).await {
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
    response.validate_contract().map_err(|envelope| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            envelope.message,
        )) as Box<dyn std::error::Error + Send + Sync>
    })?;
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

#[cfg(test)]
pub(super) fn protocol_read_failure_response(envelope: ErrorEnvelope) -> IpcResponse {
    protocol_read_failure_response_with_correlation(envelope, RequestCorrelation::default(), None)
}

pub(super) async fn pre_framing_session_busy_response<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut tokio::io::BufReader<R>,
    authoritative_daemon_session_id: Option<&str>,
    limit: usize,
) -> Option<IpcResponse> {
    let correlation = recover_pre_framing_request_correlation(reader).await;
    correlation.command_id.as_ref()?;
    let envelope = ErrorEnvelope::new(
        ErrorCode::SessionBusy,
        "Daemon is temporarily at its pre-request connection limit",
    )
    .with_context(serde_json::json!({
        "phase": "ipc_accept",
        "reason": "pre_framing_connection_limit",
        "limit": limit,
        "request_correlation_recovered": true,
        "request_correlation_contract": "committed_request_frame",
    }));
    Some(protocol_read_failure_response_with_correlation(
        envelope,
        correlation,
        authoritative_daemon_session_id,
    ))
}

async fn recover_pre_framing_request_correlation<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut tokio::io::BufReader<R>,
) -> RequestCorrelation {
    match tokio::time::timeout(
        PRE_FRAMING_CORRELATION_TIMEOUT,
        NdJsonCodec::read_frame_bytes(reader),
    )
    .await
    {
        Ok(Ok(Some(frame))) => RequestCorrelation::from_request_frame(&frame),
        _ => RequestCorrelation::default(),
    }
}

#[derive(Debug, Clone)]
struct ProtocolReadFailure {
    envelope: ErrorEnvelope,
    correlation: RequestCorrelation,
}

async fn read_request_for_live_ingress<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut tokio::io::BufReader<R>,
) -> Result<Option<IpcRequest>, ProtocolReadFailure> {
    let request_frame =
        match tokio::time::timeout(IPC_READ_TIMEOUT, NdJsonCodec::read_frame_bytes(reader)).await {
            Err(_) => {
                return Err(ProtocolReadFailure {
                    envelope: ErrorEnvelope::new(
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
                    correlation: RequestCorrelation::default(),
                });
            }
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                return Err(ProtocolReadFailure {
                    envelope: read_failure_envelope(error),
                    correlation: RequestCorrelation::default(),
                });
            }
        };
    let Some(request_frame) = request_frame else {
        return Ok(None);
    };
    let correlation = RequestCorrelation::from_request_frame(&request_frame);
    let request_value =
        serde_json::from_slice::<serde_json::Value>(&request_frame).map_err(|error| {
            ProtocolReadFailure {
                envelope: read_failure_envelope(Box::new(error)),
                correlation,
            }
        })?;
    let correlation = RequestCorrelation::from_request_value(&request_value);
    let request = IpcRequest::from_value_transport(request_value).map_err(|envelope| {
        ProtocolReadFailure {
            envelope,
            correlation,
        }
    })?;
    Ok(Some(request))
}

fn protocol_read_failure_response_with_correlation(
    envelope: ErrorEnvelope,
    correlation: RequestCorrelation,
    authoritative_daemon_session_id: Option<&str>,
) -> IpcResponse {
    let response = IpcResponse::error(Uuid::now_v7().to_string(), envelope);
    correlation.attach_to_response(response, authoritative_daemon_session_id)
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
    use super::{
        PreRequestResponseFenceGuard, pre_framing_session_busy_response,
        protocol_read_failure_response_with_correlation, read_request_for_live_ingress,
        write_response_with_timeout_duration,
    };
    use crate::session::SessionState;
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_ipc::protocol::{IpcResponse, ResponseStatus};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn response_write_timeout_bounds_stalled_writer_fence() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let response = IpcResponse {
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            request_id: "req-timeout".to_string(),
            command_id: None,
            daemon_session_id: None,
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

    #[tokio::test]
    async fn response_write_rejects_invalid_contract_before_transport_commit() {
        let (mut writer, _reader) = tokio::io::duplex(128);
        let response = IpcResponse::success("   ", serde_json::json!({"ok": true}));

        let error = write_response_with_timeout_duration(
            &mut writer,
            &response,
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("invalid contract should fail before transport commit");

        let io_error = error
            .downcast::<std::io::Error>()
            .expect("contract failure should be reported as io::Error");
        assert_eq!(io_error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn live_ingress_uses_transport_aware_decode_for_control_plane_compat_commands() {
        let (mut writer, reader) = tokio::io::duplex(256);
        writer
            .write_all(
                br#"{"ipc_protocol_version":"0.9","command":"_handshake","args":{},"timeout_ms":1000}
"#,
            )
            .await
            .expect("write request frame");
        drop(writer);

        let mut reader = tokio::io::BufReader::new(reader);
        let request = read_request_for_live_ingress(&mut reader)
            .await
            .expect("transport-aware ingress should pass compatibility control-plane commands")
            .expect("compat request");
        assert_eq!(request.command, "_handshake");
        assert_eq!(request.ipc_protocol_version, "0.9");
    }

    #[tokio::test]
    async fn live_ingress_preserves_request_correlation_on_transport_contract_failure() {
        let (mut writer, reader) = tokio::io::duplex(256);
        writer
            .write_all(
                br#"{"ipc_protocol_version":"0.9","command":"doctor","command_id":"cmd-1","daemon_session_id":"sess-1","args":{},"timeout_ms":1000}
"#,
            )
            .await
            .expect("write request frame");
        drop(writer);

        let mut reader = tokio::io::BufReader::new(reader);
        let failure = read_request_for_live_ingress(&mut reader)
            .await
            .expect_err("non-compat request must still fail closed on protocol mismatch");
        assert_eq!(failure.envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            failure
                .envelope
                .context
                .as_ref()
                .and_then(|context| context.get("field"))
                .and_then(|value| value.as_str()),
            Some("ipc_protocol_version")
        );
        let response = protocol_read_failure_response_with_correlation(
            failure.envelope,
            failure.correlation,
            Some("sess-authoritative"),
        );
        assert_eq!(response.command_id.as_deref(), Some("cmd-1"));
        assert_eq!(
            response.daemon_session_id.as_deref(),
            Some("sess-authoritative")
        );
    }

    #[tokio::test]
    async fn live_ingress_recovers_request_correlation_from_framed_invalid_json() {
        let (mut writer, reader) = tokio::io::duplex(256);
        writer
            .write_all(
                br#"{"ipc_protocol_version":"1.1","command":"doctor","command_id":"cmd-json","daemon_session_id":"sess-json","args":{"broken": },"timeout_ms":1000}
"#,
            )
            .await
            .expect("write malformed request frame");
        drop(writer);

        let mut reader = tokio::io::BufReader::new(reader);
        let failure = read_request_for_live_ingress(&mut reader)
            .await
            .expect_err("framed invalid JSON must fail closed");
        assert_eq!(failure.envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            failure
                .envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("invalid_json_request")
        );
        let response = protocol_read_failure_response_with_correlation(
            failure.envelope,
            failure.correlation,
            Some("sess-authoritative"),
        );
        assert_eq!(response.command_id.as_deref(), Some("cmd-json"));
        assert_eq!(
            response.daemon_session_id.as_deref(),
            Some("sess-authoritative")
        );
    }

    #[tokio::test]
    async fn live_ingress_recovers_request_correlation_from_framed_invalid_utf8() {
        let (mut writer, reader) = tokio::io::duplex(256);
        let mut frame = b"{\"ipc_protocol_version\":\"1.1\",\"command\":\"doctor\",\"command_id\":\"cmd-utf8\",\"daemon_session_id\":\"sess-utf8\",\"args\":{\"payload\":\"".to_vec();
        frame.push(0xff);
        frame.extend_from_slice(b"\"},\"timeout_ms\":1000}");
        frame.push(b'\n');
        writer
            .write_all(&frame)
            .await
            .expect("write malformed utf8 frame");
        drop(writer);

        let mut reader = tokio::io::BufReader::new(reader);
        let failure = read_request_for_live_ingress(&mut reader)
            .await
            .expect_err("framed invalid utf8 must fail closed");
        assert_eq!(failure.envelope.code, ErrorCode::IpcProtocolError);
        let response = protocol_read_failure_response_with_correlation(
            failure.envelope,
            failure.correlation,
            Some("sess-authoritative"),
        );
        assert_eq!(response.command_id.as_deref(), Some("cmd-utf8"));
        assert_eq!(
            response.daemon_session_id.as_deref(),
            Some("sess-authoritative")
        );
    }

    #[tokio::test]
    async fn pre_framing_backpressure_response_preserves_committed_request_correlation() {
        let (mut writer, reader) = tokio::io::duplex(512);
        writer
            .write_all(
                br#"{"ipc_protocol_version":"1.1","command":"doctor","command_id":"cmd-busy","args":{},"timeout_ms":1000}
"#,
            )
            .await
            .expect("write request frame");
        drop(writer);

        let mut reader = tokio::io::BufReader::new(reader);
        let response =
            pre_framing_session_busy_response(&mut reader, Some("sess-authoritative"), 128)
                .await
                .expect("complete request frame should receive a structured busy response");

        assert_eq!(response.status, ResponseStatus::Error);
        assert_eq!(response.command_id.as_deref(), Some("cmd-busy"));
        assert_eq!(
            response.daemon_session_id.as_deref(),
            Some("sess-authoritative")
        );
        let error = response
            .error
            .expect("session busy response should be error");
        assert_eq!(error.code, ErrorCode::SessionBusy);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("request_correlation_recovered"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn pre_framing_backpressure_without_request_correlation_closes_transport() {
        let (mut writer, reader) = tokio::io::duplex(512);
        writer
            .write_all(br#"{"ipc_protocol_version":"1.1","command":"doctor","command_id":"cmd"#)
            .await
            .expect("write partial request frame");

        let mut reader = tokio::io::BufReader::new(reader);
        let response =
            pre_framing_session_busy_response(&mut reader, Some("sess-authoritative"), 128).await;

        assert!(
            response.is_none(),
            "without a committed request frame, backpressure must close transport instead of publishing an uncorrelated structured response"
        );
    }

    #[test]
    fn correlated_protocol_read_failure_response_ignores_blank_wire_correlation() {
        let response = protocol_read_failure_response_with_correlation(
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, "bad request"),
            super::RequestCorrelation {
                command_id: Some("cmd-1".to_string()),
                daemon_session_id: None,
            },
            None,
        );
        assert_eq!(response.command_id.as_deref(), Some("cmd-1"));

        let response = protocol_read_failure_response_with_correlation(
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, "bad request"),
            super::RequestCorrelation::from_request_value(&serde_json::json!({
                "command_id": "   ",
                "daemon_session_id": ""
            })),
            None,
        );
        assert_eq!(response.command_id, None);
        assert_eq!(response.daemon_session_id, None);
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
