use std::path::Path;
use std::path::PathBuf;
use std::{error::Error, fmt};
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

use crate::codec::NdJsonCodec;
use crate::protocol::{IPC_PROTOCOL_VERSION, IpcProtocolDecodeError, IpcRequest, IpcResponse};
use rub_core::error::{ErrorCode, ErrorEnvelope};

/// CLI-side IPC client. Connects to a daemon's Unix socket.
pub struct IpcClient {
    stream: Option<UnixStream>,
    deferred_socket_path: Option<PathBuf>,
    bound_daemon_session_id: Option<String>,
    used: bool,
}

const IPC_CLIENT_TIMEOUT_BUFFER_MS: u64 = 1_000;

fn preserve_io_error(error: Box<dyn std::error::Error + Send + Sync>) -> std::io::Error {
    match error.downcast::<std::io::Error>() {
        Ok(io_error) => *io_error,
        Err(error) => std::io::Error::other(error.to_string()),
    }
}

#[derive(Debug)]
pub enum IpcClientError {
    Transport(std::io::Error),
    Protocol(ErrorEnvelope),
}

impl IpcClientError {
    fn transport(error: std::io::Error) -> Self {
        Self::Transport(error)
    }

    fn protocol(envelope: ErrorEnvelope) -> Self {
        Self::Protocol(envelope)
    }

    pub fn protocol_envelope(&self) -> Option<&ErrorEnvelope> {
        match self {
            Self::Protocol(envelope) => Some(envelope),
            Self::Transport(_) => None,
        }
    }

    fn response_read_envelope(error: Box<dyn Error + Send + Sync>) -> ErrorEnvelope {
        match error.downcast::<IpcProtocolDecodeError>() {
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
                        _ => "ipc_response_read_failure",
                    };
                    ErrorEnvelope::new(
                        ErrorCode::IpcProtocolError,
                        format!("Invalid IPC response frame: {io_error}"),
                    )
                    .with_context(serde_json::json!({
                        "phase": "ipc_response_read",
                        "reason": reason,
                    }))
                }
                Err(error) => match error.downcast::<serde_json::Error>() {
                    Ok(json_error) => ErrorEnvelope::new(
                        ErrorCode::IpcProtocolError,
                        format!("Invalid JSON response body: {json_error}"),
                    )
                    .with_context(serde_json::json!({
                        "phase": "ipc_response_read",
                        "reason": "invalid_json_response",
                    })),
                    Err(error) => ErrorEnvelope::new(
                        ErrorCode::IpcProtocolError,
                        format!("Failed to decode IPC response: {error}"),
                    )
                    .with_context(serde_json::json!({
                        "phase": "ipc_response_read",
                        "reason": "ipc_response_read_failure",
                    })),
                },
            },
        }
    }
}

impl fmt::Display for IpcClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Protocol(envelope) => write!(f, "{}", envelope.message),
        }
    }
}

impl Error for IpcClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Protocol(_) => None,
        }
    }
}

impl IpcClient {
    /// Connect to the daemon socket at the given path.
    pub async fn connect(socket_path: &Path) -> Result<Self, std::io::Error> {
        let stream = UnixStream::connect(socket_path).await?;
        Ok(Self {
            stream: Some(stream),
            deferred_socket_path: None,
            bound_daemon_session_id: None,
            used: false,
        })
    }

    /// Build a single-use client whose first request will lazily connect to
    /// the provided socket path. This lets bootstrap/attach hand the caller a
    /// client bound to a verified daemon authority without opening a second,
    /// unverified connection during the bootstrap transaction itself.
    pub fn deferred(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            stream: None,
            deferred_socket_path: Some(socket_path.into()),
            bound_daemon_session_id: None,
            used: false,
        }
    }

    /// Bind the client to one concrete daemon authority. When the caller does
    /// not set `daemon_session_id` explicitly, the client will project this
    /// bound authority into the first request sent over the connection.
    pub fn bind_daemon_session_id(
        mut self,
        daemon_session_id: impl Into<String>,
    ) -> Result<Self, String> {
        let daemon_session_id = daemon_session_id.into();
        if daemon_session_id.trim().is_empty() {
            return Err("IPC daemon_session_id must be non-empty and non-whitespace".to_string());
        }
        self.bound_daemon_session_id = Some(daemon_session_id);
        Ok(self)
    }

    /// Send a single request and receive a response.
    ///
    /// The daemon serves exactly one request per connection, so each `IpcClient`
    /// instance is intentionally single-use.
    pub async fn send(&mut self, request: &IpcRequest) -> Result<IpcResponse, IpcClientError> {
        if self.used {
            return Err(IpcClientError::transport(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "IpcClient is single-use; open a new connection for each request",
            )));
        }
        let request = match (
            &self.bound_daemon_session_id,
            request.daemon_session_id.as_deref(),
        ) {
            (Some(bound), Some(explicit)) if explicit != bound => {
                return Err(IpcClientError::transport(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "IPC request daemon_session_id mismatch: client is bound to {bound:?}, request targeted {explicit:?}",
                    ),
                )));
            }
            (Some(bound), None) => request
                .clone()
                .with_daemon_session_id(bound.clone())
                .map_err(|error| IpcClientError::transport(std::io::Error::other(error)))?,
            _ => request.clone(),
        };

        let stream = if let Some(stream) = self.stream.take() {
            stream
        } else if let Some(socket_path) = self.deferred_socket_path.as_ref() {
            let stream = UnixStream::connect(socket_path)
                .await
                .map_err(IpcClientError::transport)?;
            self.deferred_socket_path = None;
            stream
        } else {
            return Err(IpcClientError::transport(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "IpcClient has no remaining socket authority",
            )));
        };
        self.used = true;
        let (reader, mut writer) = stream.into_split();
        let timeout_budget = Duration::from_millis(
            request
                .timeout_ms
                .saturating_add(IPC_CLIENT_TIMEOUT_BUFFER_MS),
        );

        let response_value: serde_json::Value = timeout(timeout_budget, async {
            NdJsonCodec::write(&mut writer, &request)
                .await
                .map_err(preserve_io_error)
                .map_err(IpcClientError::transport)?;
            let mut buf_reader = BufReader::new(reader);
            NdJsonCodec::read(&mut buf_reader)
                .await
                .map_err(IpcClientError::response_read_envelope)
                .map_err(IpcClientError::protocol)?
                .ok_or_else(|| {
                    IpcClientError::transport(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Daemon closed connection",
                    ))
                })
        })
        .await
        .map_err(|_| {
            IpcClientError::transport(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "IPC request '{}' exceeded local round-trip timeout after {}ms",
                    request.command,
                    timeout_budget.as_millis()
                ),
            ))
        })??;

        let response =
            IpcResponse::from_value_strict(response_value).map_err(IpcClientError::protocol)?;

        if response.ipc_protocol_version != IPC_PROTOCOL_VERSION {
            return Err(IpcClientError::protocol(
                ErrorEnvelope::new(
                    ErrorCode::IpcVersionMismatch,
                    format!(
                        "IPC protocol version mismatch in response: expected {}, got {}",
                        IPC_PROTOCOL_VERSION, response.ipc_protocol_version
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_protocol_version_mismatch",
                    "expected_protocol_version": IPC_PROTOCOL_VERSION,
                    "actual_protocol_version": response.ipc_protocol_version,
                })),
            ));
        }
        if response.command_id != request.command_id {
            return Err(IpcClientError::protocol(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!(
                        "IPC response command_id mismatch: expected {:?}, got {:?}",
                        request.command_id, response.command_id
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_command_id_mismatch",
                    "expected_command_id": request.command_id,
                    "actual_command_id": response.command_id,
                })),
            ));
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::IpcClient;
    use crate::codec::NdJsonCodec;
    use crate::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use tokio::io::BufReader;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn client_is_single_use() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "doctor");
            let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let mut client = IpcClient::connect(&socket_path).await.expect("connect");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let first = client.send(&request).await.expect("first send");
        assert_eq!(first.status, ResponseStatus::Success);

        let error = client
            .send(&request)
            .await
            .expect_err("second send should fail");
        assert!(
            error.to_string().contains("IpcClient is single-use"),
            "{error}"
        );

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn client_rejects_mismatched_response_protocol_version() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let _: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            let mut response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
            response.ipc_protocol_version = "0.9".to_string();
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let mut client = IpcClient::connect(&socket_path).await.expect("connect");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let error = client
            .send(&request)
            .await
            .expect_err("mismatched protocol response should fail");
        assert!(error.to_string().contains("protocol version mismatch"));
        assert_eq!(
            error
                .protocol_envelope()
                .and_then(|envelope| envelope.context.as_ref())
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("ipc_response_protocol_version_mismatch")
        );

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn client_rejects_mismatched_response_command_id() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let _: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
                .with_command_id("other-command")
                .expect("static command_id must be valid");
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let mut client = IpcClient::connect(&socket_path).await.expect("connect");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_command_id("cmd-1")
            .expect("static command_id must be valid");
        let error = client
            .send(&request)
            .await
            .expect_err("mismatched command_id response should fail");
        assert!(error.to_string().contains("command_id mismatch"));

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn client_rejects_unsolicited_response_command_id() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let _: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
                .with_command_id("unsolicited-command")
                .expect("static command_id must be valid");
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let mut client = IpcClient::connect(&socket_path).await.expect("connect");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let error = client
            .send(&request)
            .await
            .expect_err("unexpected command_id response should fail");
        assert!(error.to_string().contains("command_id mismatch"));

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn deferred_bound_client_projects_verified_daemon_session_id() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert_eq!(request.daemon_session_id.as_deref(), Some("sess-bound"));
            let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let mut client = IpcClient::deferred(&socket_path)
            .bind_daemon_session_id("sess-bound")
            .expect("daemon authority must be valid");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let response = client.send(&request).await.expect("send succeeds");
        assert_eq!(response.status, ResponseStatus::Success);

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn bound_client_rejects_conflicting_explicit_daemon_authority() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let accepted =
                tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept())
                    .await;
            assert!(
                accepted.is_err(),
                "client-side authority mismatch must fail closed before opening a socket"
            );
        });

        let mut client = IpcClient::deferred(&socket_path)
            .bind_daemon_session_id("sess-bound")
            .expect("daemon authority must be valid");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_daemon_session_id("sess-other")
            .expect("explicit authority must be valid");
        let error = client
            .send(&request)
            .await
            .expect_err("conflicting daemon authority must fail locally");
        assert!(error.to_string().contains("daemon_session_id mismatch"));

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn deferred_client_connect_failure_does_not_consume_single_use_authority() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let mut client = IpcClient::deferred(&socket_path);
        let request = IpcRequest::new("doctor", serde_json::json!({}), 250);

        let error = client
            .send(&request)
            .await
            .expect_err("missing socket should fail to connect");
        let error_text = error.to_string();
        assert!(
            error_text.contains("No such file")
                || error_text.contains("not found")
                || error_text.contains("No such")
        );

        let listener = UnixListener::bind(&socket_path).expect("bind listener");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "doctor");
            let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        });

        let response = client
            .send(&request)
            .await
            .expect("client should remain reusable after connect failure");
        assert_eq!(response.status, ResponseStatus::Success);

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn client_surfaces_structured_response_contract_errors() {
        let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&socket_dir).expect("create socket dir");
        let socket_path = socket_dir.join("ipc.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let _: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            let invalid = serde_json::json!({
                "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                "request_id": "req-1",
                "status": "success",
                "data": { "ok": true },
                "error": serde_json::to_value(
                    ErrorEnvelope::new(ErrorCode::IpcProtocolError, "bad"),
                )
                .expect("serialize envelope"),
                "timing": { "queue_ms": 0, "exec_ms": 0, "total_ms": 0 }
            });
            NdJsonCodec::write(&mut writer, &invalid)
                .await
                .expect("write response");
        });

        let mut client = IpcClient::connect(&socket_path).await.expect("connect");
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let error = client
            .send(&request)
            .await
            .expect_err("invalid response contract should fail");
        assert_eq!(
            error
                .protocol_envelope()
                .and_then(|envelope| envelope.context.as_ref())
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("invalid_ipc_response_contract")
        );

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);
    }
}
