use std::path::Path;
use std::process::Command;

use rub_ipc::handshake::{
    SocketSessionIdentityConfirmation as SocketIdentityConfirmation,
    confirm_daemon_session_identity,
};

pub(crate) fn process_matches_registry_entry(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<bool> {
    if !process_matches_daemon_identity(
        rub_home,
        &entry.session_name,
        Some(entry.session_id.as_str()),
        entry.pid,
    )? {
        return Ok(false);
    }
    Ok(!matches!(
        socket_identity_confirmation(Path::new(&entry.socket_path), &entry.session_id)?,
        SocketIdentityConfirmation::ConfirmedMismatch
    ))
}

pub(crate) fn process_matches_daemon_identity(
    rub_home: &Path,
    session_name: &str,
    session_id: Option<&str>,
    pid: u32,
) -> std::io::Result<bool> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if command.is_empty() {
        return Ok(false);
    }
    Ok(command_matches_daemon_identity(
        &command,
        rub_home,
        session_name,
        session_id,
    ))
}

pub(crate) fn command_matches_daemon_identity(
    command: &str,
    rub_home: &Path,
    session_name: &str,
    session_id: Option<&str>,
) -> bool {
    if !command.contains("__daemon")
        || extract_flag_value(command, "--session").as_deref() != Some(session_name)
        || extract_flag_value(command, "--rub-home").as_deref()
            != Some(rub_home.to_string_lossy().as_ref())
    {
        return false;
    }
    match session_id {
        Some(session_id) => {
            extract_flag_value(command, "--session-id").as_deref() == Some(session_id)
        }
        None => true,
    }
}

pub(crate) fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
    rub_core::process::extract_flag_value(command, flag)
}

pub(crate) fn process_matches_failed_startup_identity(
    rub_home: &Path,
    session_name: &str,
    session_id: &str,
    socket_path: &Path,
    pid: u32,
) -> std::io::Result<bool> {
    if !process_matches_daemon_identity(rub_home, session_name, Some(session_id), pid)? {
        return Ok(false);
    }
    Ok(!matches!(
        socket_identity_confirmation(socket_path, session_id)?,
        SocketIdentityConfirmation::ConfirmedMismatch
    ))
}

fn socket_identity_confirmation(
    socket_path: &Path,
    expected_session_id: &str,
) -> std::io::Result<SocketIdentityConfirmation> {
    confirm_daemon_session_identity(socket_path, expected_session_id)
}

#[cfg(test)]
mod tests {
    use super::{SocketIdentityConfirmation, socket_identity_confirmation};

    #[cfg(unix)]
    fn spawn_handshake_server(
        socket_path: &std::path::Path,
        daemon_session_id: &str,
    ) -> std::thread::JoinHandle<()> {
        use rub_ipc::codec::NdJsonCodec;
        use rub_ipc::protocol::IpcResponse;
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
            let _: rub_ipc::protocol::IpcRequest = NdJsonCodec::read_blocking(&mut reader)
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
            std::path::PathBuf::from(format!("/tmp/rub-id-{}", uuid::Uuid::now_v7().simple()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create unique socket dir");
        (dir.join("s.sock"), dir)
    }

    #[cfg(unix)]
    #[test]
    fn registry_entry_socket_identity_confirms_matching_session() {
        let (socket_path, socket_dir) = unique_socket_path();
        let server = spawn_handshake_server(&socket_path, "sess-live");
        let confirmation =
            socket_identity_confirmation(&socket_path, "sess-live").expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(confirmation, SocketIdentityConfirmation::ConfirmedMatch);
    }

    #[cfg(unix)]
    #[test]
    fn registry_entry_socket_identity_rejects_mismatched_session() {
        let (socket_path, socket_dir) = unique_socket_path();
        let server = spawn_handshake_server(&socket_path, "sess-other");
        let confirmation =
            socket_identity_confirmation(&socket_path, "sess-live").expect("probe should complete");
        server.join().expect("handshake server should join");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&socket_dir);

        assert_eq!(confirmation, SocketIdentityConfirmation::ConfirmedMismatch);
    }
}
