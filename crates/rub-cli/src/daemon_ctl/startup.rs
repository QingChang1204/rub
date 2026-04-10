use super::{
    DaemonCtlPathContext, authority_bound_deferred_client, connect_ipc_once, daemon_ctl_path_error,
    daemon_ctl_path_state, daemon_ctl_socket_error, fetch_handshake_info_with_timeout,
    handshake_attempt_error, remaining_budget_duration, remaining_budget_ms,
};
use crate::connection_hardening::{
    AttemptError, ConnectionFailureClass, RetryAttribution, RetryFailure, RetryPolicy,
    attach_connection_diagnostics, classify_error_code,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::process::is_process_alive;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[cfg(test)]
use super::FORCE_SETSID_FAILURE;

const READY_FILE_ENV: &str = "RUB_DAEMON_READY_FILE";
const ERROR_FILE_ENV: &str = "RUB_DAEMON_ERROR_FILE";
const SESSION_ID_ENV: &str = "RUB_SESSION_ID";

pub struct StartupSignalFiles {
    pub ready_file: std::path::PathBuf,
    pub error_file: std::path::PathBuf,
    pub daemon_pid: u32,
    pub session_id: String,
}

struct StartupSignalCleanup<'a> {
    signals: &'a StartupSignalFiles,
}

impl Drop for StartupSignalCleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.signals.ready_file);
        let _ = std::fs::remove_file(&self.signals.error_file);
    }
}

struct StartupReadyMonitor {
    socket_path: std::path::PathBuf,
    committed_path: std::path::PathBuf,
}

enum StartupReadinessObservation {
    Pending,
    ReadyToHandshake,
    Error(RubError),
    DaemonExitedBeforeCommit {
        ready_written: bool,
        committed_session_id: Option<String>,
    },
}

#[cfg(unix)]
pub(super) fn detach_daemon_session() -> std::io::Result<()> {
    #[cfg(test)]
    if FORCE_SETSID_FAILURE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other("forced setsid failure"));
    }

    if unsafe { libc::setsid() } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Debug)]
pub struct StartupLockGuard {
    files: Vec<std::fs::File>,
}

impl Drop for StartupLockGuard {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = unlock(file);
        }
    }
}

pub fn start_daemon(
    rub_home: &Path,
    session_name: &str,
    session_id: &str,
    extra_args: &[String],
) -> Result<StartupSignalFiles, RubError> {
    let exe = std::env::current_exe().map_err(|e| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Cannot find rub binary: {e}"),
        )
    })?;
    std::fs::create_dir_all(rub_home)?;

    let startup_id = Uuid::now_v7().to_string();
    let session_paths = RubPaths::new(rub_home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(session_paths.session_dir())?;
    let ready_file = session_paths.startup_ready_path(&startup_id);
    let error_file = session_paths.startup_error_path(&startup_id);
    let _ = std::fs::remove_file(&ready_file);
    let _ = std::fs::remove_file(&error_file);

    let mut cmd = Command::new(exe);
    cmd.arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(rub_home.to_string_lossy().as_ref())
        .env(READY_FILE_ENV, &ready_file)
        .env(ERROR_FILE_ENV, &error_file);
    cmd.env(SESSION_ID_ENV, session_id);

    for arg in extra_args {
        cmd.arg(arg);
    }

    #[cfg(unix)]
    {
        use std::process::Stdio;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(detach_daemon_session);
        }
    }

    let child = cmd.spawn().map_err(|e| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Failed to spawn daemon: {e}"),
        )
    })?;

    Ok(StartupSignalFiles {
        ready_file,
        error_file,
        daemon_pid: child.id(),
        session_id: session_id.to_string(),
    })
}

#[cfg(test)]
pub async fn acquire_startup_lock(
    rub_home: &Path,
    session_name: &str,
    attachment_identity: Option<&str>,
    timeout_ms: u64,
) -> Result<StartupLockGuard, RubError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    acquire_startup_lock_until(rub_home, session_name, attachment_identity, deadline).await
}

pub async fn acquire_startup_lock_until(
    rub_home: &Path,
    session_name: &str,
    attachment_identity: Option<&str>,
    deadline: Instant,
) -> Result<StartupLockGuard, RubError> {
    std::fs::create_dir_all(rub_home)?;
    let paths = RubPaths::new(rub_home);
    std::fs::create_dir_all(paths.startup_locks_dir())?;
    let mut files = Vec::new();
    for scope_key in startup_lock_scope_keys(session_name, attachment_identity) {
        let lock_path = paths.startup_lock_path(&scope_key);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                daemon_ctl_path_error(
                    ErrorCode::DaemonStartFailed,
                    format!("Failed to open startup lock {}: {e}", lock_path.display()),
                    DaemonCtlPathContext {
                        path_key: "lock_path",
                        path: &lock_path,
                        path_authority: "daemon_ctl.startup.lock_path",
                        upstream_truth: "startup_lock_scope_key",
                        path_kind: "startup_lock_file",
                        reason: "startup_lock_open_failed",
                    },
                )
            })?;

        loop {
            match try_lock_exclusive(&file) {
                Ok(()) => {
                    files.push(file);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(daemon_ctl_path_error(
                            ErrorCode::DaemonStartFailed,
                            "Timed out waiting for daemon startup lock before the command deadline"
                                .to_string(),
                            DaemonCtlPathContext {
                                path_key: "lock_path",
                                path: &lock_path,
                                path_authority: "daemon_ctl.startup.lock_path",
                                upstream_truth: "startup_lock_scope_key",
                                path_kind: "startup_lock_file",
                                reason: "startup_lock_timeout",
                            },
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(err) => {
                    return Err(daemon_ctl_path_error(
                        ErrorCode::DaemonStartFailed,
                        format!("Failed to acquire daemon startup lock: {err}"),
                        DaemonCtlPathContext {
                            path_key: "lock_path",
                            path: &lock_path,
                            path_authority: "daemon_ctl.startup.lock_path",
                            upstream_truth: "startup_lock_scope_key",
                            path_kind: "startup_lock_file",
                            reason: "startup_lock_acquire_failed",
                        },
                    ));
                }
            }
        }
    }

    Ok(StartupLockGuard { files })
}

#[cfg(test)]
pub async fn wait_for_ready(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    timeout_ms: u64,
) -> Result<(IpcClient, String), RubError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    wait_for_ready_until(rub_home, session_name, signals, deadline).await
}

pub async fn wait_for_ready_until(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    deadline: Instant,
) -> Result<(IpcClient, String), RubError> {
    let monitor = StartupReadyMonitor::new(rub_home, session_name, &signals.session_id);
    let _signal_cleanup = StartupSignalCleanup { signals };
    let timeout_ms = remaining_budget_ms(deadline).max(1);
    let mut last_transport_retry: Option<RetryAttribution> = None;

    loop {
        match monitor.observe(signals)? {
            StartupReadinessObservation::Error(error) => {
                let envelope = error.into_envelope();
                let classified = classify_error_code(envelope.code);
                let error = RubError::Domain(envelope);
                return Err(if let Some(attribution) = last_transport_retry.as_ref() {
                    attach_connection_diagnostics(error, attribution, classified)
                } else {
                    error
                });
            }
            StartupReadinessObservation::DaemonExitedBeforeCommit {
                ready_written,
                committed_session_id,
            } => {
                return Err(RubError::domain_with_context(
                    ErrorCode::DaemonStartFailed,
                    format!(
                        "Daemon for session '{session_name}' exited before startup authority committed"
                    ),
                    serde_json::json!({
                        "reason": "daemon_exited_before_startup_commit",
                        "session": session_name,
                        "session_id": signals.session_id,
                        "daemon_pid": signals.daemon_pid,
                        "ready_marker_present": ready_written,
                        "commit_marker_value": committed_session_id,
                    }),
                ));
            }
            StartupReadinessObservation::ReadyToHandshake => {
                match connect_ready_client(&monitor.socket_path, &signals.session_id, deadline)
                    .await
                {
                    Ok((client, daemon_session_id, _attribution)) => {
                        return Ok((client, daemon_session_id));
                    }
                    Err(failure)
                        if matches!(
                            failure.final_failure_class,
                            ConnectionFailureClass::TransportTransient
                        ) =>
                    {
                        last_transport_retry = Some(failure.attribution);
                    }
                    Err(failure) => return Err(failure.into_error()),
                }
            }
            StartupReadinessObservation::Pending => {}
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let sleep_for = std::cmp::min(
            std::time::Duration::from_millis(100),
            deadline.saturating_duration_since(now),
        );
        tokio::time::sleep(sleep_for).await;
    }

    let error = RubError::domain(
        ErrorCode::DaemonStartFailed,
        format!("Daemon did not become ready within {}ms", timeout_ms),
    );
    Err(if let Some(attribution) = last_transport_retry.as_ref() {
        attach_connection_diagnostics(
            error,
            attribution,
            ConnectionFailureClass::TransportTransient,
        )
    } else {
        error
    })
}

impl StartupReadyMonitor {
    fn new(rub_home: &Path, session_name: &str, session_id: &str) -> Self {
        let session_runtime = RubPaths::new(rub_home).session_runtime(session_name, session_id);
        Self {
            socket_path: session_runtime.socket_path(),
            committed_path: session_runtime.startup_committed_path(),
        }
    }

    fn observe(
        &self,
        signals: &StartupSignalFiles,
    ) -> Result<StartupReadinessObservation, RubError> {
        if signals.error_file.exists() {
            let envelope = read_startup_error(&signals.error_file)?;
            return Ok(StartupReadinessObservation::Error(RubError::Domain(
                envelope,
            )));
        }

        let ready_written = signals.ready_file.exists();
        let committed_session_id = std::fs::read_to_string(&self.committed_path).ok();
        let committed = committed_session_id.as_deref() == Some(signals.session_id.as_str());
        if !committed && !is_process_alive(signals.daemon_pid) {
            return Ok(StartupReadinessObservation::DaemonExitedBeforeCommit {
                ready_written,
                committed_session_id,
            });
        }

        if ready_written && committed {
            Ok(StartupReadinessObservation::ReadyToHandshake)
        } else {
            Ok(StartupReadinessObservation::Pending)
        }
    }
}

async fn connect_ready_client(
    socket_path: &Path,
    expected_session_id: &str,
    deadline: Instant,
) -> Result<(IpcClient, String, RetryAttribution), RetryFailure> {
    let socket_path = socket_path.to_path_buf();
    let expected_session_id = expected_session_id.to_string();
    let policy = RetryPolicy::default();
    let mut attribution = RetryAttribution::default();

    loop {
        let Some(remaining) = remaining_budget_duration(deadline) else {
            return Err(startup_ready_retry_timeout_failure(
                attribution,
                &socket_path,
            ));
        };
        let attempt = tokio::time::timeout(remaining, async {
            let mut handshake_client = connect_ipc_once(
                &socket_path,
                ErrorCode::DaemonStartFailed,
                "Failed to connect to the daemon socket while confirming startup readiness",
                "daemon_ctl.startup.handshake.socket_path",
                "startup_ready_monitor.socket_path",
            )
            .await?;

            let handshake = fetch_handshake_info_with_timeout(
                &mut handshake_client,
                remaining_budget_ms(deadline).max(1),
            )
            .await
            .map_err(handshake_attempt_error)?;
            if handshake.daemon_session_id != expected_session_id {
                return Err(AttemptError::terminal(
                    RubError::domain_with_context(
                        ErrorCode::DaemonStartFailed,
                        "Daemon startup handshake resolved a different daemon authority than the committed session",
                        serde_json::json!({
                            "reason": "startup_handshake_authority_mismatch",
                            "expected_session_id": expected_session_id,
                            "handshake_session_id": handshake.daemon_session_id,
                            "socket_path": socket_path.display().to_string(),
                            "socket_path_state": daemon_ctl_path_state(
                                "daemon_ctl.startup.handshake.socket_path",
                                "startup_ready_monitor.socket_path",
                                "session_socket",
                            ),
                        }),
                    ),
                    ConnectionFailureClass::ProtocolMismatch,
                ));
            }

            let client = authority_bound_deferred_client(&socket_path, &handshake.daemon_session_id)
                .map_err(|error| {
                    AttemptError::terminal(
                        daemon_ctl_socket_error(
                            ErrorCode::DaemonStartFailed,
                            format!(
                                "Failed to bind startup daemon authority after handshake: {error}"
                            ),
                            &socket_path,
                            "daemon_ctl.startup.handshake.socket_path",
                            "startup_ready_monitor.socket_path",
                            "startup_handshake_bind_failed",
                        ),
                        ConnectionFailureClass::ProtocolMismatch,
                    )
                })?;

            Ok((client, handshake.daemon_session_id))
        })
        .await;

        match attempt {
            Ok(Ok((client, daemon_session_id))) => {
                return Ok((client, daemon_session_id, attribution));
            }
            Ok(Err(attempt_error)) => {
                if let Some(reason) = attempt_error.transient_reason.clone()
                    && attribution.retry_count < policy.max_retries
                    && remaining_budget_duration(deadline).is_some()
                {
                    attribution.retry_count += 1;
                    attribution.retry_reason = Some(reason);
                    if let Some(delay) = remaining_budget_duration(deadline) {
                        tokio::time::sleep(delay.min(policy.delay)).await;
                        continue;
                    }
                }

                return Err(RetryFailure {
                    error: attempt_error.error,
                    attribution,
                    final_failure_class: if attempt_error.transient_reason.is_some() {
                        ConnectionFailureClass::TransportTransient
                    } else {
                        attempt_error.final_failure_class
                    },
                });
            }
            Err(_) => {
                return Err(startup_ready_retry_timeout_failure(
                    attribution,
                    &socket_path,
                ));
            }
        }
    }
}

pub(super) fn startup_ready_retry_timeout_failure(
    attribution: RetryAttribution,
    socket_path: &Path,
) -> RetryFailure {
    RetryFailure {
        error: daemon_ctl_socket_error(
            ErrorCode::DaemonStartFailed,
            "Daemon readiness handshake exceeded the declared startup timeout".to_string(),
            socket_path,
            "daemon_ctl.startup.handshake.socket_path",
            "startup_ready_monitor.socket_path",
            "startup_handshake_timeout",
        ),
        attribution,
        final_failure_class: ConnectionFailureClass::TransportTransient,
    }
}

pub(super) fn read_startup_error(path: &Path) -> Result<rub_core::error::ErrorEnvelope, RubError> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        daemon_ctl_path_error(
            ErrorCode::DaemonStartFailed,
            format!(
                "Failed to read startup error file {}: {error}",
                path.display()
            ),
            DaemonCtlPathContext {
                path_key: "error_file",
                path,
                path_authority: "daemon_ctl.startup.error_file",
                upstream_truth: "startup_error_signal_file",
                path_kind: "startup_error_file",
                reason: "startup_error_file_read_failed",
            },
        )
    })?;
    let envelope = serde_json::from_str(&contents).unwrap_or_else(|_| {
        rub_core::error::ErrorEnvelope::new(ErrorCode::DaemonStartFailed, contents)
    });
    Ok(envelope)
}

pub fn startup_signal_paths() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    (
        std::env::var_os(READY_FILE_ENV).map(std::path::PathBuf::from),
        std::env::var_os(ERROR_FILE_ENV).map(std::path::PathBuf::from),
    )
}

pub(super) fn startup_lock_scope_keys(
    session_name: &str,
    attachment_identity: Option<&str>,
) -> Vec<String> {
    let mut keys = vec![format!("session-{session_name}")];
    if let Some(identity) = attachment_identity {
        keys.push(format!("attachment-{identity}"));
    }
    keys
}

pub(super) fn try_lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(super) fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
