//! Daemon lifecycle — fork, readiness signaling, and graceful shutdown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Notify, Semaphore};
use tracing::{error, info};

use crate::router::DaemonRouter;
use crate::session::{
    RegistryEntry, SessionState, authoritative_entry_by_session_name, cleanup_projections,
    deregister_session, ensure_rub_home, promote_session_authority, register_pending_session,
    rfc3339_now,
};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_ipc::protocol::IPC_PROTOCOL_VERSION;

mod io;
mod projection;
mod shutdown;

use io::{
    ConnectedClientGuard, PreRequestResponseFenceGuard, handle_connection,
    protocol_read_failure_response, write_response_with_timeout,
};
use projection::{
    publish_pid_projection, publish_socket_projection, publish_startup_commit_marker,
    restore_previous_authority, signal_ready, startup_ready_marker_path,
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
            let _ = restore_previous_authority(&self.home, previous_authority);
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
    let shutdown = Arc::new(Notify::new());
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
                        let (_reader, mut writer) = stream.into_split();
                        let response = protocol_read_failure_response(
                            ErrorEnvelope::new(
                                ErrorCode::SessionBusy,
                                "Daemon is temporarily at its pre-request connection limit",
                            )
                            .with_context(serde_json::json!({
                                "phase": "ipc_accept",
                                "reason": "pre_framing_connection_limit",
                                "limit": MAX_PRE_FRAMING_CONNECTIONS,
                            })),
                        );
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
                    shutdown.notify_waiters();
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
mod tests {
    use super::{
        StartupCommitGuard, protocol_read_failure_response, publish_pid_projection,
        publish_socket_projection, publish_startup_commit_marker, signal_ready,
        wait_for_transaction_drain,
    };
    use crate::daemon::io::read_failure_envelope;
    use crate::daemon::shutdown::{
        wait_for_transaction_drain_with_timeout, wait_for_worker_shutdown_with_timeout,
    };
    use crate::rub_paths::RubPaths;
    use crate::session::{RegistryEntry, SessionState, read_registry, write_registry};
    use rub_core::error::ErrorCode;
    use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_home() -> std::path::PathBuf {
        let sequence = TEMP_HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rub-daemon-test-{}-{sequence}", std::process::id()))
    }

    #[test]
    fn publish_pid_projection_writes_canonical_pid_projection() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let session_paths = RubPaths::new(&home).session("default");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        if let Some(parent) = session_paths.socket_path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(session_paths.socket_path(), b"socket").unwrap();
        let state = SessionState::new("default", home.clone(), None);

        publish_pid_projection(&state, 4242).unwrap();

        assert_eq!(
            std::fs::read_to_string(session_paths.canonical_pid_path())
                .unwrap()
                .trim(),
            "4242"
        );

        let _ = std::fs::remove_file(session_paths.socket_path());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn publish_socket_projection_links_canonical_socket_to_actual_socket() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let state = SessionState::new_with_id("default", "sess-default", home.clone(), None);
        let runtime_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
        let projection_paths = RubPaths::new(&home).session("default");
        std::fs::create_dir_all(runtime_paths.session_dir()).unwrap();
        if let Some(parent) = runtime_paths.socket_path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(runtime_paths.socket_path(), b"socket").unwrap();

        publish_socket_projection(&state).unwrap();

        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::read_link(projection_paths.canonical_socket_path()).unwrap(),
                runtime_paths.socket_path()
            );
        }

        let _ = std::fs::remove_file(runtime_paths.socket_path());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_failure_envelope_classifies_partial_frames_as_protocol_errors() {
        let error = std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "NDJSON frame terminated before newline commit fence",
        );
        let envelope = read_failure_envelope(Box::new(error));
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("partial_ndjson_frame")
        );
    }

    #[test]
    fn protocol_read_failure_response_wraps_structured_error_envelope() {
        let response =
            protocol_read_failure_response(read_failure_envelope(Box::new(serde_json::Error::io(
                std::io::Error::new(std::io::ErrorKind::InvalidData, "bad json"),
            ))));
        assert_eq!(response.status, rub_ipc::protocol::ResponseStatus::Error);
        assert_eq!(
            response.error.as_ref().map(|error| error.code),
            Some(ErrorCode::IpcProtocolError)
        );
    }

    #[test]
    fn read_failure_envelope_preserves_request_contract_reason() {
        let envelope =
            read_failure_envelope(Box::new(rub_ipc::protocol::IpcProtocolDecodeError::new(
                rub_ipc::protocol::IpcRequest::from_value_strict(serde_json::json!({
                    "ipc_protocol_version": "1.0",
                    "command": "doctor",
                    "args": {},
                    "timeout_ms": 1000,
                    "unexpected": "field",
                }))
                .expect_err("strict decode should reject unknown fields"),
            )));
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("invalid_ipc_request_contract")
        );
    }

    #[test]
    fn signal_ready_reports_write_failures() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        unsafe {
            std::env::set_var("RUB_DAEMON_READY_FILE", &home);
        }
        let error = signal_ready().expect_err("directory path should fail ready marker write");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::IsADirectory
                | std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::Other
        ));
        unsafe {
            std::env::remove_var("RUB_DAEMON_READY_FILE");
        }

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn startup_guard_rolls_back_registry_and_projections_before_commit() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let state = SessionState::new_with_id("default", "sess-default", home.clone(), None);
        let runtime_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
        let projection_paths = RubPaths::new(&home).session("default");
        std::fs::create_dir_all(runtime_paths.session_dir()).unwrap();
        std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
        std::fs::write(runtime_paths.socket_path(), b"socket").unwrap();
        std::fs::write(runtime_paths.pid_path(), b"4242").unwrap();
        publish_socket_projection(&state).unwrap();
        publish_pid_projection(&state, 4242).unwrap();
        publish_startup_commit_marker(&state).unwrap();

        let entry = RegistryEntry {
            session_id: "sess-default".to_string(),
            session_name: "default".to_string(),
            pid: 4242,
            socket_path: runtime_paths.socket_path().display().to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        write_registry(
            &home,
            &crate::session::RegistryData {
                sessions: vec![entry.clone()],
            },
        )
        .unwrap();

        {
            let _guard = StartupCommitGuard::new(&home, entry, None);
        }

        assert!(!runtime_paths.socket_path().exists());
        assert!(!runtime_paths.pid_path().exists());
        assert!(!projection_paths.canonical_socket_path().exists());
        assert!(!projection_paths.canonical_pid_path().exists());
        assert!(!projection_paths.startup_committed_path().exists());
        assert!(read_registry(&home).unwrap().sessions.is_empty());

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn shutdown_drain_flushes_pending_post_commit_projections() {
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-default",
            temp_home(),
            None,
        ));
        let request = IpcRequest::new(
            "pipe",
            serde_json::json!({
                "spec": "[]",
                "spec_source": { "kind": "file", "path": "/tmp/workflow.json" }
            }),
            30_000,
        )
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
        let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}))
            .with_command_id("cmd-1")
            .expect("static command_id must be valid");

        state.submit_post_commit_projection(&request, &response);
        wait_for_transaction_drain(&state).await;

        assert_eq!(state.pending_post_commit_projection_count(), 0);
        assert_eq!(state.command_history(5).await.entries.len(), 1);
        assert_eq!(state.workflow_capture(5).await.entries.len(), 1);
    }

    #[tokio::test]
    async fn shutdown_drain_waits_past_soft_timeout_until_transactions_finish() {
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-default",
            temp_home(),
            None,
        ));
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);

        let drain_state = state.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            drain_state
                .in_flight_count
                .store(0, std::sync::atomic::Ordering::SeqCst);
        });

        let start = tokio::time::Instant::now();
        wait_for_transaction_drain_with_timeout(
            &state,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_millis(1),
        )
        .await;
        let elapsed = start.elapsed();

        releaser.await.unwrap();
        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "drain returned before in-flight transaction quiesced"
        );
    }

    #[tokio::test]
    async fn shutdown_drain_waits_past_soft_timeout_until_connected_request_fence_clears() {
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-default",
            temp_home(),
            None,
        ));
        state
            .connected_client_count
            .store(1, std::sync::atomic::Ordering::SeqCst);

        let drain_state = state.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            drain_state
                .connected_client_count
                .store(0, std::sync::atomic::Ordering::SeqCst);
        });

        let start = tokio::time::Instant::now();
        wait_for_transaction_drain_with_timeout(
            &state,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_millis(1),
        )
        .await;
        let elapsed = start.elapsed();
        let metrics = state.automation_scheduler_metrics().await;
        releaser.await.unwrap();

        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "drain returned before the connected request fence quiesced"
        );
        assert_eq!(
            metrics["shutdown_drain"]["soft_timeout_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            metrics["shutdown_drain"]["connected_only_soft_release_count"],
            serde_json::json!(0)
        );
        assert_eq!(
            metrics["shutdown_drain"]["max_observed_in_flight_count"],
            serde_json::json!(0)
        );
        assert_eq!(
            metrics["shutdown_drain"]["max_observed_connected_client_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            metrics["shutdown_drain"]["max_observed_pre_request_response_fence_count"],
            serde_json::json!(0)
        );
    }

    #[tokio::test]
    async fn shutdown_drain_waits_past_soft_timeout_until_pre_request_response_fence_clears() {
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-default",
            temp_home(),
            None,
        ));
        state
            .pre_request_response_fence_count
            .store(1, std::sync::atomic::Ordering::SeqCst);

        let drain_state = state.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            drain_state
                .pre_request_response_fence_count
                .store(0, std::sync::atomic::Ordering::SeqCst);
        });

        let start = tokio::time::Instant::now();
        wait_for_transaction_drain_with_timeout(
            &state,
            std::time::Duration::from_millis(5),
            std::time::Duration::from_millis(1),
        )
        .await;
        let elapsed = start.elapsed();
        let metrics = state.automation_scheduler_metrics().await;
        releaser.await.unwrap();

        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "drain returned before the pre-request response fence quiesced"
        );
        assert_eq!(
            metrics["shutdown_drain"]["soft_timeout_count"],
            serde_json::json!(1)
        );
        assert_eq!(
            metrics["shutdown_drain"]["max_observed_in_flight_count"],
            serde_json::json!(0)
        );
        assert_eq!(
            metrics["shutdown_drain"]["max_observed_connected_client_count"],
            serde_json::json!(0)
        );
        assert_eq!(
            metrics["shutdown_drain"]["max_observed_pre_request_response_fence_count"],
            serde_json::json!(1)
        );
    }

    #[tokio::test]
    async fn worker_shutdown_waits_past_soft_timeout_without_abort() {
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completed_worker = completed.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            completed_worker.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let start = tokio::time::Instant::now();
        wait_for_worker_shutdown_with_timeout(
            handle,
            "test_worker",
            std::time::Duration::from_millis(5),
        )
        .await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "worker shutdown returned before the worker naturally finished"
        );
        assert!(
            completed.load(std::sync::atomic::Ordering::SeqCst),
            "worker should complete instead of being aborted at the soft timeout"
        );
    }
}
