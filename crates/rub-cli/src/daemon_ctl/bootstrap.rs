use std::path::Path;
use std::time::Instant;

use rub_core::error::RubError;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;

use super::connect::{TransientSocketPolicy, detect_or_connect_hardened};
use super::registry::{cleanup_stale, process_matches_failed_startup_identity};
use super::startup::{
    StartupSignalFiles, acquire_startup_lock_until, start_daemon, wait_for_ready_until,
};
use super::{DaemonConnection, terminate_spawned_daemon_force};

pub struct BootstrapClient {
    pub client: IpcClient,
    pub connected_to_existing_daemon: bool,
    pub daemon_session_id: Option<String>,
}

struct BootstrapResolution {
    client: IpcClient,
    connected_to_existing_daemon: bool,
    daemon_session_id: Option<String>,
}

impl BootstrapResolution {
    fn connected(client: IpcClient, daemon_session_id: Option<String>) -> Self {
        Self {
            client,
            connected_to_existing_daemon: true,
            daemon_session_id,
        }
    }

    fn started(client: IpcClient, daemon_session_id: String) -> Self {
        Self {
            client,
            connected_to_existing_daemon: false,
            daemon_session_id: Some(daemon_session_id),
        }
    }
}

pub async fn bootstrap_client(
    rub_home: &Path,
    session_name: &str,
    command_deadline: Instant,
    extra_args: &[String],
    attachment_identity: Option<&str>,
) -> Result<BootstrapClient, RubError> {
    let resolution = match detect_or_connect_hardened(
        rub_home,
        session_name,
        TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => BootstrapResolution::connected(client, daemon_session_id),
        DaemonConnection::NeedStart => {
            resolve_bootstrap_after_lock(
                rub_home,
                session_name,
                command_deadline,
                extra_args,
                attachment_identity,
            )
            .await?
        }
    };

    Ok(BootstrapClient {
        client: resolution.client,
        connected_to_existing_daemon: resolution.connected_to_existing_daemon,
        daemon_session_id: resolution.daemon_session_id,
    })
}

async fn resolve_bootstrap_after_lock(
    rub_home: &Path,
    session_name: &str,
    command_deadline: Instant,
    extra_args: &[String],
    attachment_identity: Option<&str>,
) -> Result<BootstrapResolution, RubError> {
    let startup_session_id = rub_daemon::session::new_session_id();
    let startup_lock = acquire_startup_lock_until(
        rub_home,
        session_name,
        attachment_identity,
        command_deadline,
    )
    .await?;

    let resolution = match detect_or_connect_hardened(
        rub_home,
        session_name,
        TransientSocketPolicy::FailAfterLock,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => Ok(BootstrapResolution::connected(client, daemon_session_id)),
        DaemonConnection::NeedStart => {
            start_new_daemon_bootstrap(
                rub_home,
                session_name,
                &startup_session_id,
                extra_args,
                command_deadline,
            )
            .await
        }
    };

    drop(startup_lock);
    resolution
}

async fn start_new_daemon_bootstrap(
    rub_home: &Path,
    session_name: &str,
    startup_session_id: &str,
    extra_args: &[String],
    command_deadline: Instant,
) -> Result<BootstrapResolution, RubError> {
    let signals = start_daemon(rub_home, session_name, startup_session_id, extra_args)?;
    let ready = wait_for_ready_until(rub_home, session_name, &signals, command_deadline).await;
    if ready.is_err() {
        cleanup_failed_startup(rub_home, session_name, &signals).await;
    }
    ready.map(|(client, daemon_session_id)| BootstrapResolution::started(client, daemon_session_id))
}

async fn cleanup_failed_startup(rub_home: &Path, session_name: &str, signals: &StartupSignalFiles) {
    let _ = terminate_failed_startup_process(rub_home, session_name, signals).await;

    let runtime_paths = RubPaths::new(rub_home).session_runtime(session_name, &signals.session_id);
    for _ in 0..20 {
        if !rub_core::process::is_process_alive(signals.daemon_pid)
            && runtime_paths
                .actual_socket_paths()
                .into_iter()
                .all(|path| !path.exists())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    if !rub_core::process::is_process_alive(signals.daemon_pid) {
        let cleanup_entry = rub_daemon::session::RegistryEntry {
            session_id: signals.session_id.clone(),
            session_name: session_name.to_string(),
            pid: signals.daemon_pid,
            socket_path: runtime_paths.socket_path().display().to_string(),
            created_at: String::new(),
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let _ = rub_daemon::session::deregister_session(rub_home, &signals.session_id);
        cleanup_stale(rub_home, &cleanup_entry);
    }

    let _ = std::fs::remove_file(&signals.ready_file);
    let _ = std::fs::remove_file(&signals.error_file);
}

async fn terminate_failed_startup_process(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
) -> std::io::Result<()> {
    if !process_matches_failed_startup_identity(
        rub_home,
        session_name,
        signals.session_id.as_str(),
        signals.daemon_pid,
    )? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Refused to kill pid {} because it no longer matches failed-startup daemon authority for session '{}' under {}",
                signals.daemon_pid,
                session_name,
                rub_home.display()
            ),
        ));
    }
    terminate_spawned_daemon_force(signals.daemon_pid).await
}
