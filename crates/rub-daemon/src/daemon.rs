//! Daemon lifecycle — fork, readiness signaling, and graceful shutdown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tracing::{error, info};
use uuid::Uuid;

use crate::router::DaemonRouter;
use crate::session::{
    RegistryEntry, SessionState, authoritative_entry_by_session_name, cleanup_projections,
    deregister_session, ensure_rub_home, promote_session_authority, register_pending_session,
    register_session_with_displaced, rfc3339_now,
};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::fs::{FileCommitOutcome, atomic_write_bytes, sync_parent_dir};
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest};

const IPC_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
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
                    tokio::spawn(async move {
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
                        let _ = NdJsonCodec::write(&mut writer, &response).await;
                    });
                    continue;
                };
                tokio::spawn(async move {
                    let _permit = permit;
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

/// Handle a single IPC connection from a CLI client.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    handle_connection_inner(stream, router, &state).await
}

async fn handle_connection_inner(
    stream: tokio::net::UnixStream,
    router: Arc<DaemonRouter>,
    state: &Arc<SessionState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = tokio::io::BufReader::new(reader);

    // Read one request per connection
    let request: Option<IpcRequest> =
        match tokio::time::timeout(IPC_READ_TIMEOUT, NdJsonCodec::read(&mut buf_reader)).await {
            Err(_) => {
                let response = protocol_read_failure_response(
                    ErrorEnvelope::new(
                        ErrorCode::IpcTimeout,
                        format!(
                            "Timed out waiting for an NDJSON request commit fence after {}s",
                            IPC_READ_TIMEOUT.as_secs()
                        ),
                    )
                    .with_context(serde_json::json!({
                        "phase": "ipc_read",
                        "reason": "ipc_read_timeout",
                    })),
                );
                let _ = NdJsonCodec::write(&mut writer, &response).await;
                return Ok(());
            }
            Ok(Ok(request)) => request,
            Ok(Err(error)) => {
                let response = protocol_read_failure_response(read_failure_envelope(error));
                let _ = NdJsonCodec::write(&mut writer, &response).await;
                return Ok(());
            }
        };
    let Some(request) = request else {
        return Ok(()); // Client disconnected
    };

    let _connected_client = ConnectedClientGuard::new(state);
    info!(command = %request.command, command_id = ?request.command_id, "Received request");

    // Dispatch through router
    let response = router.dispatch(request, state).await;

    // Write response
    NdJsonCodec::write(&mut writer, &response).await?;

    Ok(())
}

struct ConnectedClientGuard<'a> {
    state: &'a Arc<SessionState>,
}

impl<'a> ConnectedClientGuard<'a> {
    fn new(state: &'a Arc<SessionState>) -> Self {
        state
            .connected_client_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self { state }
    }
}

impl Drop for ConnectedClientGuard<'_> {
    fn drop(&mut self) {
        self.state
            .connected_client_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn protocol_read_failure_response(envelope: ErrorEnvelope) -> rub_ipc::protocol::IpcResponse {
    rub_ipc::protocol::IpcResponse::error(Uuid::now_v7().to_string(), envelope)
}

fn read_failure_envelope(error: Box<dyn std::error::Error + Send + Sync>) -> ErrorEnvelope {
    match error.downcast::<std::io::Error>() {
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
                _ => "ipc_read_failure",
            };
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                format!("Invalid NDJSON request: {io_error}"),
            )
            .with_context(serde_json::json!({
                "phase": "ipc_read",
                "reason": reason,
            }))
        }
        Err(error) => match error.downcast::<serde_json::Error>() {
            Ok(json_error) => ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                format!("Invalid JSON request body: {json_error}"),
            )
            .with_context(serde_json::json!({
                "phase": "ipc_read",
                "reason": "invalid_json_request",
            })),
            Err(error) => ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                format!("Failed to parse IPC request: {error}"),
            )
            .with_context(serde_json::json!({
                "phase": "ipc_read",
                "reason": "ipc_read_failure",
            })),
        },
    }
}

async fn wait_for_transaction_drain(state: &Arc<SessionState>) {
    wait_for_transaction_drain_with_timeout(
        state,
        SHUTDOWN_DRAIN_TIMEOUT,
        SHUTDOWN_DRAIN_POLL_INTERVAL,
    )
    .await;
}

async fn wait_for_transaction_drain_with_timeout(
    state: &Arc<SessionState>,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut timeout_logged = false;
    loop {
        let in_flight = state
            .in_flight_count
            .load(std::sync::atomic::Ordering::SeqCst);
        let connected = state
            .connected_client_count
            .load(std::sync::atomic::Ordering::SeqCst);
        if in_flight == 0 && connected == 0 {
            break;
        }
        if !timeout_logged && tokio::time::Instant::now() >= deadline {
            error!(
                in_flight_count = in_flight,
                connected_client_count = connected,
                "Shutdown drain exceeded the soft budget; continuing to wait because teardown must not cut an in-flight transaction"
            );
            timeout_logged = true;
        }
        tokio::time::sleep(poll_interval).await;
    }

    if state.pending_post_commit_projection_count() > 0 {
        state.drain_post_commit_projections().await;
    }
}

async fn wait_for_worker_shutdown(handle: JoinHandle<()>, worker_name: &str) {
    wait_for_worker_shutdown_with_timeout(handle, worker_name, SHUTDOWN_DRAIN_TIMEOUT).await;
}

async fn wait_for_worker_shutdown_with_timeout(
    mut handle: JoinHandle<()>,
    worker_name: &str,
    timeout: std::time::Duration,
) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            error!(worker = worker_name, error = %error, "Shutdown worker task exited with join error");
        }
        Err(_) => {
            error!(
                worker = worker_name,
                "Shutdown worker exceeded the soft budget; continuing to wait because aborting it could drop an in-flight automation transaction guard"
            );
            match handle.await {
                Ok(()) => {}
                Err(error) => {
                    error!(
                        worker = worker_name,
                        error = %error,
                        "Shutdown worker task exited with join error after the soft budget"
                    );
                }
            }
        }
    }
}

/// Wait for SIGTERM or SIGINT.
async fn wait_for_shutdown_signal() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = sigterm.recv() => { info!("Received SIGTERM"); }
            _ = sigint.recv() => { info!("Received SIGINT"); }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        info!("Received Ctrl-C");
    }
    Ok(())
}

fn signal_ready() -> std::io::Result<()> {
    if let Some(path) = std::env::var_os("RUB_DAEMON_READY_FILE") {
        atomic_write_durable_bytes(Path::new(&path), b"ready", 0o600)?;
    }
    Ok(())
}

fn restore_previous_authority(home: &Path, entry: &RegistryEntry) -> std::io::Result<()> {
    let _ = register_session_with_displaced(home, entry.clone())?;
    restore_socket_projection(home, entry)?;
    restore_pid_projection(home, entry)?;
    restore_startup_commit_marker(home, entry)?;
    Ok(())
}

fn startup_ready_marker_path() -> Option<PathBuf> {
    std::env::var_os("RUB_DAEMON_READY_FILE").map(PathBuf::from)
}

fn publish_pid_projection(state: &SessionState, pid: u32) -> std::io::Result<()> {
    let session_paths =
        crate::rub_paths::RubPaths::new(&state.rub_home).session(&state.session_name);
    std::fs::create_dir_all(session_paths.projection_dir())?;
    atomic_write_durable_bytes(
        &session_paths.canonical_pid_path(),
        pid.to_string().as_bytes(),
        0o600,
    )?;

    Ok(())
}

fn publish_startup_commit_marker(state: &SessionState) -> std::io::Result<()> {
    let session_paths =
        crate::rub_paths::RubPaths::new(&state.rub_home).session(&state.session_name);
    if let Some(parent) = session_paths.startup_committed_path().parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write_durable_bytes(
        &session_paths.startup_committed_path(),
        state.session_id.as_bytes(),
        0o600,
    )?;
    Ok(())
}

fn restore_startup_commit_marker(home: &Path, entry: &RegistryEntry) -> std::io::Result<()> {
    let session_paths = crate::rub_paths::RubPaths::new(home).session(&entry.session_name);
    if let Some(parent) = session_paths.startup_committed_path().parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write_durable_bytes(
        &session_paths.startup_committed_path(),
        entry.session_id.as_bytes(),
        0o600,
    )?;
    Ok(())
}

fn restore_pid_projection(home: &Path, entry: &RegistryEntry) -> std::io::Result<()> {
    let session_paths = crate::rub_paths::RubPaths::new(home).session(&entry.session_name);
    std::fs::create_dir_all(session_paths.projection_dir())?;
    atomic_write_durable_bytes(
        &session_paths.canonical_pid_path(),
        entry.pid.to_string().as_bytes(),
        0o600,
    )?;
    Ok(())
}

fn publish_socket_projection(state: &SessionState) -> std::io::Result<()> {
    let runtime_paths = crate::rub_paths::RubPaths::new(&state.rub_home)
        .session_runtime(&state.session_name, &state.session_id);
    let projection_paths =
        crate::rub_paths::RubPaths::new(&state.rub_home).session(&state.session_name);
    let actual_socket = runtime_paths.socket_path();
    #[cfg(unix)]
    {
        let canonical_socket = projection_paths.canonical_socket_path();
        atomic_replace_symlink(&actual_socket, &canonical_socket)?;
    }

    Ok(())
}

fn restore_socket_projection(home: &Path, entry: &RegistryEntry) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let canonical_socket = crate::rub_paths::RubPaths::new(home)
            .session(&entry.session_name)
            .canonical_socket_path();
        atomic_replace_symlink(Path::new(&entry.socket_path), &canonical_socket)?;
    }

    Ok(())
}

fn atomic_write_durable_bytes(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    let outcome = atomic_write_bytes(path, contents, mode)?;
    require_durable_projection_commit(path, outcome)
}

fn require_durable_projection_commit(
    path: &Path,
    outcome: FileCommitOutcome,
) -> std::io::Result<()> {
    if outcome.durability_confirmed() {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "Projection commit for {} was published but durability was not confirmed",
        path.display()
    )))
}

#[cfg(unix)]
fn atomic_replace_symlink(target: &Path, symlink_path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = symlink_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_symlink = symlink_path.with_extension(format!("tmp-link-{}", Uuid::now_v7()));
    let _ = std::fs::remove_file(&temp_symlink);
    symlink(target, &temp_symlink)?;
    if let Err(error) = std::fs::rename(&temp_symlink, symlink_path) {
        let _ = std::fs::remove_file(&temp_symlink);
        return Err(error);
    }
    sync_parent_dir(symlink_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        StartupCommitGuard, protocol_read_failure_response, publish_pid_projection,
        publish_socket_projection, publish_startup_commit_marker, read_failure_envelope,
        signal_ready, wait_for_transaction_drain, wait_for_transaction_drain_with_timeout,
        wait_for_worker_shutdown_with_timeout,
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
