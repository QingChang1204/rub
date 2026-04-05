use std::path::Path;
use std::path::PathBuf;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

use crate::codec::NdJsonCodec;
use crate::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};

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
    pub async fn send(
        &mut self,
        request: &IpcRequest,
    ) -> Result<IpcResponse, Box<dyn std::error::Error + Send + Sync>> {
        if self.used {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "IpcClient is single-use; open a new connection for each request",
            )
            .into());
        }
        let request = match (
            &self.bound_daemon_session_id,
            request.daemon_session_id.as_deref(),
        ) {
            (Some(bound), Some(explicit)) if explicit != bound => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "IPC request daemon_session_id mismatch: client is bound to {bound:?}, request targeted {explicit:?}",
                    ),
                )
                .into());
            }
            (Some(bound), None) => request
                .clone()
                .with_daemon_session_id(bound.clone())
                .map_err(std::io::Error::other)?,
            _ => request.clone(),
        };

        let stream = if let Some(stream) = self.stream.take() {
            stream
        } else if let Some(socket_path) = self.deferred_socket_path.as_ref() {
            let stream = UnixStream::connect(socket_path).await?;
            self.deferred_socket_path = None;
            stream
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "IpcClient has no remaining socket authority",
            )
            .into());
        };
        self.used = true;
        let (reader, mut writer) = stream.into_split();
        let timeout_budget = Duration::from_millis(
            request
                .timeout_ms
                .saturating_add(IPC_CLIENT_TIMEOUT_BUFFER_MS),
        );

        let response: IpcResponse = timeout(timeout_budget, async {
            NdJsonCodec::write(&mut writer, &request)
                .await
                .map_err(preserve_io_error)?;
            let mut buf_reader = BufReader::new(reader);
            NdJsonCodec::read(&mut buf_reader)
                .await
                .map_err(preserve_io_error)?
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Daemon closed connection",
                    )
                })
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "IPC request '{}' exceeded local round-trip timeout after {}ms",
                    request.command,
                    timeout_budget.as_millis()
                ),
            )
        })??;

        if response.ipc_protocol_version != IPC_PROTOCOL_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "IPC protocol version mismatch in response: expected {}, got {}",
                    IPC_PROTOCOL_VERSION, response.ipc_protocol_version
                ),
            )
            .into());
        }
        if response.command_id != request.command_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "IPC response command_id mismatch: expected {:?}, got {:?}",
                    request.command_id, response.command_id
                ),
            )
            .into());
        }
        if let Err(envelope) = response.validate_contract() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("IPC response contract error: {}", envelope.message),
            )
            .into());
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::IpcClient;
    use crate::codec::NdJsonCodec;
    use crate::protocol::{IpcRequest, IpcResponse, ResponseStatus};
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
}
