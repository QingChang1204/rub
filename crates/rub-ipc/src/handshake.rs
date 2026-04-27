use std::io;
use std::path::Path;
use std::time::Duration;

pub const HANDSHAKE_PROBE_COMMAND_ID: &str = "handshake-probe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketSessionIdentityConfirmation {
    ConfirmedMatch,
    ConfirmedMismatch,
    ProtocolVersionMismatch,
    ProbeContractFailure,
    Inconclusive,
}

pub fn classify_handshake_probe_response(
    response_value: serde_json::Value,
    expected_session_id: &str,
) -> SocketSessionIdentityConfirmation {
    use crate::protocol::{IPC_PROTOCOL_VERSION, IpcResponse, ResponseStatus};

    if let Some(response_protocol_version) = response_value
        .get("ipc_protocol_version")
        .and_then(serde_json::Value::as_str)
        && response_protocol_version != IPC_PROTOCOL_VERSION
    {
        return SocketSessionIdentityConfirmation::ProtocolVersionMismatch;
    }
    let request = match crate::protocol::IpcRequest::new("_handshake", serde_json::json!({}), 500)
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
    {
        Ok(request) => request,
        Err(_) => return SocketSessionIdentityConfirmation::ProbeContractFailure,
    };
    let response = match IpcResponse::from_value_transport(response_value, &request) {
        Ok(response) => response,
        Err(_) => return SocketSessionIdentityConfirmation::ProbeContractFailure,
    };
    if response.command_id.as_deref() != Some(HANDSHAKE_PROBE_COMMAND_ID) {
        return SocketSessionIdentityConfirmation::ProbeContractFailure;
    }
    if response.status != ResponseStatus::Success {
        return SocketSessionIdentityConfirmation::ProbeContractFailure;
    }
    let daemon_session_id = response.daemon_session_id.as_deref();
    if let Some(payload_daemon_session_id) = response
        .data
        .as_ref()
        .and_then(|data| data.get("daemon_session_id"))
        .and_then(serde_json::Value::as_str)
        && daemon_session_id != Some(payload_daemon_session_id)
    {
        return SocketSessionIdentityConfirmation::ProbeContractFailure;
    }
    match daemon_session_id {
        Some(session_id) if session_id == expected_session_id => {
            SocketSessionIdentityConfirmation::ConfirmedMatch
        }
        Some(_) => SocketSessionIdentityConfirmation::ConfirmedMismatch,
        None => SocketSessionIdentityConfirmation::ProbeContractFailure,
    }
}

pub fn confirm_daemon_session_identity(
    socket_path: &Path,
    expected_session_id: &str,
) -> io::Result<SocketSessionIdentityConfirmation> {
    #[cfg(unix)]
    {
        use crate::codec::NdJsonCodec;
        use crate::protocol::IpcRequest;
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
        let request = IpcRequest::new("_handshake", serde_json::json!({}), 500)
            .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
            .map_err(io::Error::other)?;
        let encoded = NdJsonCodec::encode(&request).map_err(io::Error::other)?;
        if stream.write_all(&encoded).is_err() {
            return Ok(SocketSessionIdentityConfirmation::ProbeContractFailure);
        }
        let mut reader = BufReader::new(stream);
        let response_value = match NdJsonCodec::read_blocking::<serde_json::Value, _>(&mut reader) {
            Ok(Some(response)) => response,
            Ok(None) => return Ok(SocketSessionIdentityConfirmation::ProbeContractFailure),
            Err(_) => return Ok(SocketSessionIdentityConfirmation::ProbeContractFailure),
        };
        Ok(classify_handshake_probe_response(
            response_value,
            expected_session_id,
        ))
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
    use super::{
        HANDSHAKE_PROBE_COMMAND_ID, SocketSessionIdentityConfirmation,
        classify_handshake_probe_response, confirm_daemon_session_identity,
    };

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
            let request: crate::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            let response = IpcResponse::success("req-1", serde_json::json!({}))
                .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
                .expect("probe command_id must be valid")
                .with_daemon_session_id(daemon_session_id)
                .expect("daemon_session_id must be valid");
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

    #[cfg(unix)]
    #[test]
    fn handshake_identity_treats_missing_session_id_as_probe_contract_failure() {
        use crate::codec::NdJsonCodec;
        use crate::protocol::IpcResponse;
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let request: crate::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            let response = IpcResponse::success("req-1", serde_json::json!({}))
                .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
                .expect("probe command_id must be valid");
            let encoded = NdJsonCodec::encode(&response).expect("encode response");
            stream
                .write_all(&encoded)
                .expect("write handshake response");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_reports_protocol_version_mismatch() {
        use crate::codec::NdJsonCodec;
        use crate::protocol::IpcResponse;
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let request: crate::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            let mut response = IpcResponse::success("req-1", serde_json::json!({}))
                .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
                .expect("probe command_id must be valid")
                .with_daemon_session_id("sess-live")
                .expect("daemon_session_id must be valid");
            response.ipc_protocol_version = "0.9".to_string();
            let encoded = NdJsonCodec::encode(&response).expect("encode response");
            stream
                .write_all(&encoded)
                .expect("write handshake response");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProtocolVersionMismatch
        );
    }

    #[test]
    fn handshake_probe_treats_missing_protocol_version_as_probe_contract_failure() {
        let confirmation = classify_handshake_probe_response(
            serde_json::json!({
                "request_id": "req-1",
                "status": "success",
                "command_id": HANDSHAKE_PROBE_COMMAND_ID,
                "daemon_session_id": "sess-live",
                "data": {
                    "daemon_session_id": "sess-live"
                },
                "timing": {}
            }),
            "sess-live",
        );

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[test]
    fn handshake_probe_treats_malformed_protocol_version_field_as_probe_contract_failure() {
        let confirmation = classify_handshake_probe_response(
            serde_json::json!({
                "ipc_protocol_version": 1,
                "request_id": "req-1",
                "status": "success",
                "command_id": HANDSHAKE_PROBE_COMMAND_ID,
                "daemon_session_id": "sess-live",
                "data": {
                    "daemon_session_id": "sess-live"
                },
                "timing": {}
            }),
            "sess-live",
        );

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_treats_command_id_mismatch_as_probe_contract_failure() {
        use crate::codec::NdJsonCodec;
        use crate::protocol::IpcResponse;
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let request: crate::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            let response = IpcResponse::success("req-1", serde_json::json!({}))
                .with_command_id("different-probe")
                .expect("static command_id must be valid")
                .with_daemon_session_id("sess-live")
                .expect("daemon_session_id must be valid");
            let encoded = NdJsonCodec::encode(&response).expect("encode response");
            stream
                .write_all(&encoded)
                .expect("write handshake response");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_rejects_payload_daemon_authority_divergence() {
        use crate::codec::NdJsonCodec;
        use crate::protocol::IpcResponse;
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let request: crate::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": "sess-other",
                }),
            )
            .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
            .expect("probe command_id must be valid")
            .with_daemon_session_id("sess-live")
            .expect("daemon_session_id must be valid");
            let encoded = NdJsonCodec::encode(&response).expect("encode response");
            stream
                .write_all(&encoded)
                .expect("write handshake response");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_treats_invalid_json_response_as_probe_contract_failure() {
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let request: crate::protocol::IpcRequest =
                crate::codec::NdJsonCodec::read_blocking(&mut reader)
                    .expect("read request")
                    .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            stream
                .write_all(b"{not-json}\n")
                .expect("write invalid handshake response");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_treats_partial_frame_eof_as_probe_contract_failure() {
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept handshake connection");
            let mut reader = std::io::BufReader::new(
                stream
                    .try_clone()
                    .expect("clone accepted stream for reading"),
            );
            let request: crate::protocol::IpcRequest =
                crate::codec::NdJsonCodec::read_blocking(&mut reader)
                    .expect("read request")
                    .expect("request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );
            stream
                .write_all(br#"{"ipc_protocol_version":"1.0","request_id":"req-1""#)
                .expect("write partial handshake response");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_identity_treats_empty_post_connect_eof_as_probe_contract_failure() {
        use std::os::unix::net::UnixListener;

        let (socket_path, socket_dir) = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind handshake socket");
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("accept handshake connection");
        });

        let confirmation = confirm_daemon_session_identity(&socket_path, "sess-live")
            .expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(
            confirmation,
            SocketSessionIdentityConfirmation::ProbeContractFailure
        );
    }
}
