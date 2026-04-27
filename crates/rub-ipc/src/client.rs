use std::path::Path;
use std::path::PathBuf;
use std::{error::Error, fmt};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

use crate::codec::{NdJsonCodec, is_oversized_frame_io_error};
use crate::protocol::{IpcProtocolDecodeError, IpcRequest, IpcResponse, MAX_IPC_TIMEOUT_MS};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use std::time::Instant;

/// CLI-side IPC client. Connects to a daemon's Unix socket.
pub struct IpcClient {
    stream: Option<UnixStream>,
    deferred_socket_path: Option<PathBuf>,
    bound_daemon_session_id: Option<String>,
    used: bool,
}

const IPC_CLIENT_TIMEOUT_BUFFER_MS: u64 = 1_000;

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

    fn request_encode_error(error: serde_json::Error) -> Self {
        let (message, reason) = match error.io_error_kind() {
            Some(std::io::ErrorKind::InvalidData) => (
                format!("Invalid IPC request frame: {error}"),
                "oversized_ndjson_request",
            ),
            _ => (
                format!("Failed to encode IPC request: {error}"),
                "invalid_json_request",
            ),
        };
        Self::Protocol(
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, message).with_context(
                serde_json::json!({
                    "phase": "ipc_request_write",
                    "reason": reason,
                }),
            ),
        )
    }

    fn replay_sensitive_timeout_suggestion(request: &IpcRequest) -> &'static str {
        if request.command_id.is_some() {
            "Retry only through the same command_id or replay-recovery lane; do not send a fresh command."
        } else {
            "Treat this request as possibly executed. Do not blindly retry without a command_id."
        }
    }

    fn possible_request_write_timeout(request: &IpcRequest, timeout_budget: Duration) -> Self {
        let mut context = serde_json::json!({
            "phase": "ipc_request_write",
            "reason": "ipc_request_write_timeout_after_possible_commit",
            "command": request.command,
            "timeout_ms": timeout_budget.as_millis() as u64,
            "request_commit_state": "possible",
            "command_id_present": request.command_id.is_some(),
        });
        if let Some(context_object) = context.as_object_mut()
            && let Some(command_id) = request.command_id.as_ref()
        {
            context_object.insert("command_id".to_string(), serde_json::json!(command_id));
        }
        Self::Protocol(
            ErrorEnvelope::new(
                ErrorCode::IpcTimeout,
                format!(
                    "IPC request '{}' exceeded local write timeout after the request frame may already have been committed",
                    request.command
                ),
            )
            .with_context(context)
            .with_suggestion(Self::replay_sensitive_timeout_suggestion(request)),
        )
    }

    fn committed_request_timeout(request: &IpcRequest, timeout_budget: Duration) -> Self {
        let mut context = serde_json::json!({
            "phase": "ipc_response_read",
            "reason": "ipc_response_timeout_after_request_commit",
            "command": request.command,
            "timeout_ms": timeout_budget.as_millis() as u64,
            "request_committed": true,
            "command_id_present": request.command_id.is_some(),
        });
        if let Some(context_object) = context.as_object_mut()
            && let Some(command_id) = request.command_id.as_ref()
        {
            context_object.insert("command_id".to_string(), serde_json::json!(command_id));
        }
        Self::Protocol(
            ErrorEnvelope::new(
                ErrorCode::IpcTimeout,
                format!(
                    "IPC request '{}' exceeded local response timeout after the request frame was already committed",
                    request.command
                ),
            )
            .with_context(context)
            .with_suggestion(Self::replay_sensitive_timeout_suggestion(request)),
        )
    }

    fn committed_response_transport_failure(
        request: &IpcRequest,
        timeout_budget: Duration,
        reason: &'static str,
        detail: impl fmt::Display,
    ) -> Self {
        let mut context = serde_json::json!({
            "phase": "ipc_response_read",
            "reason": "ipc_response_transport_failure_after_request_commit",
            "transport_reason": reason,
            "command": request.command,
            "timeout_ms": timeout_budget.as_millis() as u64,
            "request_committed": true,
            "command_id_present": request.command_id.is_some(),
        });
        if let Some(context_object) = context.as_object_mut()
            && let Some(command_id) = request.command_id.as_ref()
        {
            context_object.insert("command_id".to_string(), serde_json::json!(command_id));
        }
        Self::Protocol(
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                format!(
                    "IPC response transport failed after request '{}' was committed: {detail}",
                    request.command
                ),
            )
            .with_context(context)
            .with_suggestion(Self::replay_sensitive_timeout_suggestion(request)),
        )
    }

    pub fn protocol_envelope(&self) -> Option<&ErrorEnvelope> {
        match self {
            Self::Protocol(envelope) => Some(envelope),
            Self::Transport(_) => None,
        }
    }

    fn response_read_error(
        request: &IpcRequest,
        timeout_budget: Duration,
        error: Box<dyn Error + Send + Sync>,
    ) -> Self {
        match error.downcast::<IpcProtocolDecodeError>() {
            Ok(protocol_error) => Self::Protocol(protocol_error.into_envelope()),
            Err(error) => match error.downcast::<std::io::Error>() {
                Ok(io_error) => match io_error.kind() {
                    // Response framing is not committed until a full NDJSON line arrives.
                    // Transport interruptions before that fence must remain replay-recoverable.
                    std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::BrokenPipe => {
                        let reason = response_transport_failure_reason(io_error.kind());
                        Self::committed_response_transport_failure(
                            request,
                            timeout_budget,
                            reason,
                            io_error,
                        )
                    }
                    std::io::ErrorKind::InvalidData
                        if is_oversized_frame_io_error(io_error.as_ref()) =>
                    {
                        Self::Protocol(
                            ErrorEnvelope::new(
                                ErrorCode::IpcProtocolError,
                                format!("Invalid IPC response frame: {io_error}"),
                            )
                            .with_context(serde_json::json!({
                                "phase": "ipc_response_read",
                                "reason": "oversized_ndjson_frame",
                            })),
                        )
                    }
                    std::io::ErrorKind::InvalidData => Self::Protocol(
                        ErrorEnvelope::new(
                            ErrorCode::IpcProtocolError,
                            format!("Invalid IPC response frame: {io_error}"),
                        )
                        .with_context(serde_json::json!({
                            "phase": "ipc_response_read",
                            "reason": "invalid_ndjson_frame",
                        })),
                    ),
                    _ => Self::Transport(*io_error),
                },
                Err(error) => match error.downcast::<serde_json::Error>() {
                    Ok(json_error) => Self::Protocol(
                        ErrorEnvelope::new(
                            ErrorCode::IpcProtocolError,
                            format!("Invalid JSON response body: {json_error}"),
                        )
                        .with_context(serde_json::json!({
                            "phase": "ipc_response_read",
                            "reason": "invalid_json_response",
                        })),
                    ),
                    Err(error) => Self::Protocol(
                        ErrorEnvelope::new(
                            ErrorCode::IpcProtocolError,
                            format!("Failed to decode IPC response: {error}"),
                        )
                        .with_context(serde_json::json!({
                            "phase": "ipc_response_read",
                            "reason": "ipc_response_read_failure",
                        })),
                    ),
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

    #[cfg(feature = "test-utils")]
    #[doc(hidden)]
    pub fn from_connected_stream_for_test(stream: UnixStream) -> Self {
        Self {
            stream: Some(stream),
            deferred_socket_path: None,
            bound_daemon_session_id: None,
            used: false,
        }
    }

    /// Connect to the daemon socket and immediately bind the client to one
    /// verified daemon authority. This is the preferred path for attach/startup
    /// callers that have already completed authority proof and need the real
    /// execution connection to stay inside the same transaction.
    pub async fn connect_bound(
        socket_path: &Path,
        daemon_session_id: impl Into<String>,
    ) -> Result<Self, std::io::Error> {
        Self::connect(socket_path)
            .await?
            .bind_daemon_session_id(daemon_session_id)
            .map_err(std::io::Error::other)
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
        request
            .validate_contract()
            .map_err(IpcClientError::protocol)?;
        let encoded_request =
            NdJsonCodec::encode(&request).map_err(IpcClientError::request_encode_error)?;

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
        let timeout_budget_ms = request
            .timeout_ms
            .checked_add(IPC_CLIENT_TIMEOUT_BUFFER_MS)
            .filter(|timeout_ms| *timeout_ms <= MAX_IPC_TIMEOUT_MS + IPC_CLIENT_TIMEOUT_BUFFER_MS)
            .ok_or_else(|| {
                IpcClientError::protocol(
                    ErrorEnvelope::new(
                        ErrorCode::IpcProtocolError,
                        "IPC request timeout_ms exceeds protocol budget",
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_request_contract",
                        "field": "timeout_ms",
                        "max_timeout_ms": MAX_IPC_TIMEOUT_MS,
                        "actual_timeout_ms": request.timeout_ms,
                    })),
                )
            })?;
        let timeout_budget = Duration::from_millis(timeout_budget_ms);
        let deadline = Instant::now().checked_add(timeout_budget).ok_or_else(|| {
            IpcClientError::protocol(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "IPC request timeout_ms cannot be projected onto a client deadline",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_request_contract",
                    "field": "timeout_ms",
                    "max_timeout_ms": MAX_IPC_TIMEOUT_MS,
                    "actual_timeout_ms": request.timeout_ms,
                })),
            )
        })?;

        timeout(timeout_budget, async {
            writer
                .write_all(&encoded_request)
                .await
                .map_err(IpcClientError::transport)?;
            writer.flush().await.map_err(IpcClientError::transport)
        })
        .await
        .map_err(|_| IpcClientError::possible_request_write_timeout(&request, timeout_budget))??;

        let remaining_budget = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining_budget.is_zero() {
            return Err(IpcClientError::committed_request_timeout(
                &request,
                timeout_budget,
            ));
        }

        let mut buf_reader = BufReader::new(reader);
        let response_value =
            match timeout(remaining_budget, NdJsonCodec::read(&mut buf_reader)).await {
                Ok(Ok(Some(value))) => value,
                Ok(Ok(None)) => {
                    return Err(IpcClientError::committed_response_transport_failure(
                        &request,
                        timeout_budget,
                        "eof_before_response_frame",
                        "daemon closed connection before response frame",
                    ));
                }
                Ok(Err(error)) => {
                    return Err(IpcClientError::response_read_error(
                        &request,
                        timeout_budget,
                        error,
                    ));
                }
                Err(_) => {
                    return Err(IpcClientError::committed_request_timeout(
                        &request,
                        timeout_budget,
                    ));
                }
            };

        let response = IpcResponse::from_value_transport(response_value, &request)
            .map_err(IpcClientError::protocol)?;

        Ok(response)
    }
}

fn response_transport_failure_reason(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::UnexpectedEof => "partial_ndjson_frame",
        std::io::ErrorKind::ConnectionReset => "connection_reset",
        std::io::ErrorKind::ConnectionAborted => "connection_aborted",
        std::io::ErrorKind::TimedOut => "response_read_timed_out",
        std::io::ErrorKind::Interrupted => "response_read_interrupted",
        std::io::ErrorKind::WouldBlock => "response_read_would_block",
        std::io::ErrorKind::BrokenPipe => "broken_pipe",
        _ => "ipc_response_transport_failure",
    }
}

#[cfg(test)]
mod tests;
