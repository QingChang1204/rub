use std::path::Path;
use std::process::Command;

use rub_ipc::handshake::{
    SocketSessionIdentityConfirmation as SocketIdentityConfirmation,
    confirm_daemon_session_identity,
};
use rub_ipc::protocol::IPC_PROTOCOL_VERSION;

use rub_daemon::rub_paths::RubPaths;

pub(crate) fn process_matches_registry_entry_for_termination(
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
    let runtime_committed = runtime_commit_matches_registry_entry(rub_home, entry);
    let compatibility_owned = entry.ipc_protocol_version != IPC_PROTOCOL_VERSION;
    Ok(socket_identity_authorizes_registry_termination(
        socket_identity_confirmation(Path::new(&entry.socket_path), &entry.session_id)?,
        runtime_committed,
        compatibility_owned,
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
    Ok(socket_identity_confirms_expected_session(
        socket_identity_confirmation(socket_path, session_id)?,
    ))
}

fn socket_identity_confirms_expected_session(confirmation: SocketIdentityConfirmation) -> bool {
    matches!(confirmation, SocketIdentityConfirmation::ConfirmedMatch)
}

fn socket_identity_authorizes_registry_termination(
    confirmation: SocketIdentityConfirmation,
    runtime_committed: bool,
    compatibility_owned: bool,
) -> bool {
    matches!(confirmation, SocketIdentityConfirmation::ConfirmedMatch)
        || (runtime_committed
            && compatibility_owned
            && matches!(
                confirmation,
                SocketIdentityConfirmation::ProtocolVersionMismatch
                    | SocketIdentityConfirmation::Inconclusive
            ))
}

fn socket_identity_confirmation(
    socket_path: &Path,
    expected_session_id: &str,
) -> std::io::Result<SocketIdentityConfirmation> {
    confirm_daemon_session_identity(socket_path, expected_session_id)
}

fn runtime_commit_matches_registry_entry(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> bool {
    let runtime = RubPaths::new(rub_home).session_runtime(&entry.session_name, &entry.session_id);
    let pid_matches = std::fs::read_to_string(runtime.pid_path())
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        == Some(entry.pid);
    let committed_matches = std::fs::read_to_string(runtime.startup_committed_path())
        .ok()
        .is_some_and(|raw| raw.trim() == entry.session_id);
    let socket_matches = Path::new(&entry.socket_path) == runtime.socket_path();
    pid_matches && committed_matches && socket_matches
}

#[cfg(test)]
mod tests {
    use super::{
        SocketIdentityConfirmation, socket_identity_authorizes_registry_termination,
        socket_identity_confirmation, socket_identity_confirms_expected_session,
    };
    use rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID;

    #[cfg(unix)]
    fn spawn_handshake_server(
        socket_path: &std::path::Path,
        daemon_session_id: &str,
    ) -> std::thread::JoinHandle<()> {
        use rub_ipc::codec::NdJsonCodec;
        use rub_ipc::protocol::{IpcRequest, IpcResponse};
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
            let request: IpcRequest = NdJsonCodec::read_blocking(&mut reader)
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
                    "daemon_session_id": daemon_session_id.clone(),
                }),
            )
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

    #[test]
    fn only_confirmed_socket_identity_authorizes_process_match() {
        assert!(socket_identity_confirms_expected_session(
            SocketIdentityConfirmation::ConfirmedMatch
        ));
        assert!(!socket_identity_confirms_expected_session(
            SocketIdentityConfirmation::ConfirmedMismatch
        ));
        assert!(!socket_identity_confirms_expected_session(
            SocketIdentityConfirmation::ProtocolVersionMismatch
        ));
        assert!(!socket_identity_confirms_expected_session(
            SocketIdentityConfirmation::ProbeContractFailure
        ));
        assert!(!socket_identity_confirms_expected_session(
            SocketIdentityConfirmation::Inconclusive
        ));
    }

    #[test]
    fn protocol_mismatch_socket_identity_still_authorizes_registry_termination() {
        assert!(socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::ConfirmedMatch,
            false,
            false,
        ));
        assert!(socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::ProtocolVersionMismatch,
            true,
            true,
        ));
        assert!(socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::Inconclusive,
            true,
            true,
        ));
        assert!(!socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::ProbeContractFailure,
            true,
            true,
        ));
        assert!(!socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::ProtocolVersionMismatch,
            false,
            true,
        ));
        assert!(!socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::Inconclusive,
            true,
            false,
        ));
        assert!(!socket_identity_authorizes_registry_termination(
            SocketIdentityConfirmation::ConfirmedMismatch,
            true,
            true,
        ));
    }
}
