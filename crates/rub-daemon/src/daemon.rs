//! Daemon lifecycle — fork, readiness signaling, and graceful shutdown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::router::DaemonRouter;
use crate::session::{
    RegistryEntry, SessionState, authoritative_entry_by_session_name, cleanup_projections,
    deregister_session, ensure_rub_home, promote_session_authority, register_pending_session,
    rfc3339_now,
};
use rub_ipc::protocol::IPC_PROTOCOL_VERSION;

mod io;
mod projection;
mod shutdown;

#[cfg(test)]
use io::protocol_read_failure_response;
use io::{
    ConnectedClientGuard, PreRequestResponseFenceGuard, handle_connection,
    pre_framing_session_busy_response, write_response_with_timeout,
};
use projection::{
    publish_pid_projection, publish_socket_projection, publish_startup_commit_marker,
    restore_previous_authority_if_live, signal_ready, startup_ready_marker_path,
};
use shutdown::{wait_for_shutdown_signal, wait_for_transaction_drain, wait_for_worker_shutdown};

const IPC_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const IPC_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const SHUTDOWN_DRAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
const MAX_PRE_FRAMING_CONNECTIONS: usize = 128;

struct StartupCommitGuard {
    home: PathBuf,
    entry: RegistryEntry,
    previous_authority: Option<RegistryEntry>,
    committed: bool,
}

impl StartupCommitGuard {
    fn new(home: &Path, entry: RegistryEntry, previous_authority: Option<RegistryEntry>) -> Self {
        Self {
            home: home.to_path_buf(),
            entry,
            previous_authority,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for StartupCommitGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        let _ = deregister_session(&self.home, &self.entry.session_id);
        cleanup_projections(&self.home, &self.entry);
        if let Some(previous_authority) = self.previous_authority.as_ref() {
            match restore_previous_authority_if_live(&self.home, &self.entry, previous_authority) {
                Ok(projection::RestorePreviousAuthorityOutcome::Restored) => {}
                Ok(projection::RestorePreviousAuthorityOutcome::SkippedNotLive) => {
                    warn!(
                        session_name = previous_authority.session_name,
                        session_id = previous_authority.session_id,
                        "Skipped startup rollback authority restore because the previous authority is no longer live"
                    );
                }
                Ok(projection::RestorePreviousAuthorityOutcome::SkippedSuperseded) => {
                    warn!(
                        session_name = previous_authority.session_name,
                        session_id = previous_authority.session_id,
                        failed_session_id = self.entry.session_id,
                        "Skipped startup rollback authority restore because a newer startup authority already owns the session"
                    );
                }
                Err(error) => {
                    warn!(
                        session_name = previous_authority.session_name,
                        session_id = previous_authority.session_id,
                        error = %error,
                        "Failed to restore previous authority during startup rollback"
                    );
                }
            }
        }
        if let Some(path) = startup_ready_marker_path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Run the daemon main loop.
/// This is called after forking (or directly for foreground mode).
pub async fn run_daemon(
    session_name: &str,
    rub_home: &Path,
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    ensure_rub_home(rub_home)?;

    let socket_path = state.socket_path();
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(
        crate::rub_paths::RubPaths::new(&state.rub_home)
            .session_runtime(&state.session_name, &state.session_id)
            .session_dir(),
    )?;
    std::fs::create_dir_all(
        crate::rub_paths::RubPaths::new(&state.rub_home)
            .session(&state.session_name)
            .projection_dir(),
    )?;

    let _socket_bind_guard = rub_ipc::server::prepare_socket_path_for_bind(&socket_path).await?;
    let listener = tokio::net::UnixListener::bind(&socket_path)?;

    let pid = std::process::id();
    let launch_identity = state.launch_identity().await;
    let registry_entry = RegistryEntry {
        session_id: state.session_id.clone(),
        session_name: session_name.to_string(),
        pid,
        socket_path: socket_path.to_string_lossy().to_string(),
        created_at: rfc3339_now(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: state.user_data_dir.clone(),
        attachment_identity: launch_identity.attachment_identity,
        connection_target: launch_identity.connection_target,
    };
    let previous_authority = authoritative_entry_by_session_name(rub_home, session_name)?
        .filter(|entry| entry.session_id != registry_entry.session_id);
    let mut startup_guard =
        StartupCommitGuard::new(rub_home, registry_entry.clone(), previous_authority);

    // Write PID file into the runtime namespace before the startup commit.
    let pid_path = state.pid_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_path, pid.to_string())?;

    // Shutdown signal
    let shutdown = state.shutdown_notifier();
    let shutdown_clone = shutdown.clone();
    let shutdown_state = state.clone();

    // Signal handler
    tokio::spawn(async move {
        if let Ok(()) = wait_for_shutdown_signal().await {
            shutdown_state.request_shutdown();
            shutdown_clone.notify_waiters();
        }
    });

    let trigger_worker = crate::trigger_worker::spawn_trigger_worker(
        router.clone(),
        state.clone(),
        shutdown.clone(),
    );
    let orchestration_worker = crate::orchestration_worker::spawn_orchestration_worker(
        router.clone(),
        state.clone(),
        shutdown.clone(),
    );

    // Startup commit fence: once registry, canonical projections, and the
    // committed marker are published, discovery may treat the daemon as live.
    // The staged registry entry below is intentionally pending-only until the
    // commit marker lands so replacement startup never creates a "new but not
    // yet committed" authority gap for the same session name.
    //
    // The starter's private ready file is only a bootstrap handshake helper;
    // canonical discovery is keyed off the public committed authority.
    signal_ready()?;
    register_pending_session(rub_home, registry_entry.clone())?;
    publish_socket_projection(state.as_ref())?;
    publish_pid_projection(state.as_ref(), pid)?;
    publish_startup_commit_marker(state.as_ref())?;
    promote_session_authority(rub_home, session_name, &state.session_id)?;
    startup_guard.commit();
    info!(session = session_name, socket = %socket_path.display(), "Daemon listening");

    info!(pid = pid, "Daemon started");

    let idle_timeout = std::time::Duration::from_secs(30 * 60); // 30 minutes
    let mut last_activity = std::time::Instant::now();
    let connection_slots = Arc::new(Semaphore::new(MAX_PRE_FRAMING_CONNECTIONS));

    loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {
                last_activity = std::time::Instant::now();
                let router = router.clone();
                let state = state.clone();
                let Ok(permit) = connection_slots.clone().try_acquire_owned() else {
                    let reject_state = state.clone();
                    let reject_response_fence =
                        PreRequestResponseFenceGuard::new(reject_state.clone());
                    tokio::spawn(async move {
                        let _reject_response_fence = reject_response_fence;
                        let (reader, mut writer) = stream.into_split();
                        let mut reader = tokio::io::BufReader::new(reader);
                        let Some(response) = pre_framing_session_busy_response(
                            &mut reader,
                            Some(reject_state.session_id.as_str()),
                            MAX_PRE_FRAMING_CONNECTIONS,
                        )
                        .await
                        else {
                            return;
                        };
                        let _ = write_response_with_timeout(&mut writer, &response).await;
                    });
                    continue;
                };
                let connected_client_fence = ConnectedClientGuard::new(state.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    let _connected_client_fence = connected_client_fence;
                    if let Err(e) = handle_connection(stream, router, state).await {
                        error!(error = %e, "Connection handler error");
                    }
                });
            }
            _ = shutdown.notified() => {
                info!("Shutdown signal received");
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                // Check idle timeout every minute
                if last_activity.elapsed() > idle_timeout
                    && state.connected_client_count.load(std::sync::atomic::Ordering::SeqCst) == 0
                    && !state.has_active_triggers().await
                    && !state.has_active_orchestrations().await
                    && !state.has_active_human_control().await
                {
                    info!("Idle timeout reached, shutting down");
                    state.request_shutdown();
                    break;
                }
            }
        }
    }

    // Cleanup
    info!("Cleaning up session");
    drop(listener);
    shutdown.notify_waiters();
    wait_for_worker_shutdown(trigger_worker, "trigger_worker").await;
    wait_for_worker_shutdown(orchestration_worker, "orchestration_worker").await;
    wait_for_transaction_drain(&state).await;

    // Tell router to shut down browser
    if let Err(e) = router.shutdown().await {
        // Preserve discoverability on shutdown failure. If browser/profile
        // release did not commit, deregistering here would hide the still-live
        // recovery target and make cleanup less truthful.
        error!(error = %e, "Error during browser shutdown");
        return Err(e);
    }

    deregister_session(rub_home, &state.session_id)?;
    cleanup_projections(rub_home, &registry_entry);

    info!("Daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests;
