use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketSessionIdentityConfirmation {
    ConfirmedMatch,
    ConfirmedMismatch,
    Inconclusive,
}

pub fn confirm_daemon_session_identity(
    socket_path: &Path,
    expected_session_id: &str,
) -> io::Result<SocketSessionIdentityConfirmation> {
    #[cfg(unix)]
    {
        use crate::codec::NdJsonCodec;
        use crate::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
        use std::io::{BufReader, Write};
        use std::os::unix::net::UnixStream;

        if !socket_path.exists() {
            return Ok(SocketSessionIdentityConfirmation::Inconclusive);
        }
        let Ok(mut stream) = UnixStream::connect(socket_path) else {
            return Ok(SocketSessionIdentityConfirmation::Inconclusive);
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        let request = IpcRequest::new("_handshake", serde_json::json!({}), 500);
        let encoded = NdJsonCodec::encode(&request).map_err(io::Error::other)?;
        if stream.write_all(&encoded).is_err() {
            return Ok(SocketSessionIdentityConfirmation::Inconclusive);
        }
        let mut reader = BufReader::new(stream);
        let response = match NdJsonCodec::read_blocking::<IpcResponse, _>(&mut reader) {
            Ok(Some(response)) => response,
            Ok(None) => return Ok(SocketSessionIdentityConfirmation::Inconclusive),
            Err(_) => return Ok(SocketSessionIdentityConfirmation::Inconclusive),
        };
        if response.ipc_protocol_version != IPC_PROTOCOL_VERSION {
            return Ok(SocketSessionIdentityConfirmation::Inconclusive);
        }
        if response.status != ResponseStatus::Success {
            return Ok(SocketSessionIdentityConfirmation::Inconclusive);
        }
        let daemon_session_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("daemon_session_id"))
            .and_then(serde_json::Value::as_str);
        Ok(match daemon_session_id {
            Some(session_id) if session_id == expected_session_id => {
                SocketSessionIdentityConfirmation::ConfirmedMatch
            }
            Some(_) | None => SocketSessionIdentityConfirmation::ConfirmedMismatch,
        })
    }

    #[cfg(not(unix))]
    {
        let _ = socket_path;
        let _ = expected_session_id;
        Ok(SocketSessionIdentityConfirmation::Inconclusive)
    }
}

#[cfg(test)]
mod tests {
    use super::{SocketSessionIdentityConfirmation, confirm_daemon_session_identity};

    #[cfg(unix)]
    fn spawn_handshake_server(
        socket_path: &std::path::Path,
        daemon_session_id: &str,
    ) -> std::thread::JoinHandle<()> {
        use crate::codec::NdJsonCodec;
        use crate::protocol::IpcResponse;
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path).expect("bind handshake socket");
        let daemon_session_id = daemon_session_id.to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let _: crate::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
                .expect("read request")
                .expect("request");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": daemon_session_id,
                }),
            );
            let encoded = NdJsonCodec::encode(&response).expect("encode response");
            stream
                .write_all(&encoded)
                .expect("write handshake response");
        })
    }

    #[cfg(unix)]
    fn unique_socket_path() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::path::PathBuf::from(format!("/tmp/rub-ipc-id-{}", uuid::Uuid::now_v7().simple()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create unique socket dir");
        (dir.join("s.sock"), dir)
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_confirms_matching_session() {
        let (socket_path, socket_dir) = unique_socket_path();
        let server = spawn_handshake_server(&socket_path, "sess-live");
        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ConfirmedMatch
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_rejects_mismatched_session() {
        let (socket_path, socket_dir) = unique_socket_path();
        let server = spawn_handshake_server(&socket_path, "sess-other");
        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ConfirmedMismatch
        );
    }
}
