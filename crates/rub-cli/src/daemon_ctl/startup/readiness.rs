#[cfg(test)]
use super::super::FORCE_SETSID_FAILURE;
use super::super::{
    AuthorityBoundConnectSpec, authority_bound_connected_client, connect_ipc_once,
    daemon_ctl_path_error, daemon_ctl_path_state, daemon_ctl_socket_error,
    fetch_handshake_info_with_timeout, handshake_attempt_error, remaining_budget_duration,
    remaining_budget_ms,
};
use super::{
    AuthoritativeStartupInputs, CLEANUP_FILE_ENV, DaemonCtlPathContext, ERROR_FILE_ENV,
    READY_FILE_ENV, SESSION_ID_ENV, STARTUP_INPUTS_ENV, STDERR_FILE_ENV, StartupSignalFiles,
    startup_lock_scope_keys, try_lock_exclusive, unlock,
};
use crate::connection_hardening::{
    AttemptError, ConnectionFailureClass, RetryAttribution, RetryFailure, RetryPolicy,
    attach_connection_diagnostics, classify_error_code,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::process::is_process_alive;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use std::collections::BTreeSet;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use uuid::Uuid;

struct StartupSignalCleanup<'a> {
    signals: &'a StartupSignalFiles,
}

impl Drop for StartupSignalCleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.signals.ready_file);
        let _ = std::fs::remove_file(&self.signals.error_file);
        let _ = std::fs::remove_file(&self.signals.stderr_file);
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
pub(crate) fn detach_daemon_session() -> std::io::Result<()> {
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
    scope_keys: BTreeSet<String>,
}

impl Drop for StartupLockGuard {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = unlock(file);
        }
    }
}

impl StartupLockGuard {
    async fn acquire_scope_until(
        &mut self,
        rub_home: &Path,
        scope_key: String,
        deadline: Instant,
    ) -> Result<(), RubError> {
        if !self.scope_keys.insert(scope_key.clone()) {
            return Ok(());
        }

        let file = acquire_startup_lock_file_until(rub_home, &scope_key, deadline).await?;
        self.files.push(file);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn holds_scope_key_for_test(&self, scope_key: &str) -> bool {
        self.scope_keys.contains(scope_key)
    }
}

pub fn start_daemon(
    rub_home: &Path,
    session_name: &str,
    session_id: &str,
    extra_args: &[String],
    authoritative_startup_inputs: Option<&AuthoritativeStartupInputs>,
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
    let stderr_file = session_paths.startup_stderr_path(&startup_id);
    let cleanup_file = session_paths.startup_cleanup_path(&startup_id);
    let _ = std::fs::remove_file(&ready_file);
    let _ = std::fs::remove_file(&error_file);
    let _ = std::fs::remove_file(&stderr_file);
    let _ = std::fs::remove_file(&cleanup_file);

    let mut cmd = Command::new(exe);
    cmd.arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(rub_home.to_string_lossy().as_ref())
        .env(READY_FILE_ENV, &ready_file)
        .env(ERROR_FILE_ENV, &error_file)
        .env(CLEANUP_FILE_ENV, &cleanup_file);
    cmd.env(STDERR_FILE_ENV, &stderr_file);
    cmd.env(SESSION_ID_ENV, session_id);
    apply_authoritative_startup_inputs_env(&mut cmd, authoritative_startup_inputs)?;

    for arg in extra_args {
        cmd.arg(arg);
    }

    #[cfg(unix)]
    {
        use std::process::Stdio;
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_file)
            .map_err(|error| {
                daemon_ctl_path_error(
                    ErrorCode::DaemonStartFailed,
                    format!(
                        "Failed to open daemon startup stderr file {}: {error}",
                        stderr_file.display()
                    ),
                    DaemonCtlPathContext {
                        path_key: "stderr_file",
                        path: &stderr_file,
                        path_authority: "daemon_ctl.startup.stderr_file",
                        upstream_truth: "startup_stderr_signal_file",
                        path_kind: "startup_stderr_file",
                        reason: "startup_stderr_file_open_failed",
                    },
                )
            })?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr));
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
        stderr_file,
        cleanup_file,
        daemon_pid: child.id(),
        session_id: session_id.to_string(),
    })
}

fn apply_authoritative_startup_inputs_env(
    cmd: &mut Command,
    authoritative_startup_inputs: Option<&AuthoritativeStartupInputs>,
) -> Result<(), RubError> {
    if let Some(authoritative_startup_inputs) = authoritative_startup_inputs {
        let raw = serde_json::to_string(authoritative_startup_inputs).map_err(|error| {
            RubError::domain(
                ErrorCode::DaemonStartFailed,
                format!("Failed to serialize authoritative startup inputs: {error}"),
            )
        })?;
        cmd.env(STARTUP_INPUTS_ENV, raw);
    } else {
        cmd.env_remove(STARTUP_INPUTS_ENV);
    }
    Ok(())
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
    let mut guard = StartupLockGuard {
        files: Vec::new(),
        scope_keys: BTreeSet::new(),
    };
    for scope_key in startup_lock_scope_keys(session_name, attachment_identity) {
        guard
            .acquire_scope_until(rub_home, scope_key, deadline)
            .await?;
    }

    Ok(guard)
}

pub async fn upgrade_startup_lock_to_canonical_attachment_until(
    guard: &mut StartupLockGuard,
    rub_home: &Path,
    attachment_identity: Option<&str>,
    deadline: Instant,
) -> Result<Option<String>, RubError> {
    let Some(canonical_identity) =
        canonical_startup_attachment_identity(attachment_identity, deadline).await?
    else {
        return Ok(None);
    };
    guard
        .acquire_scope_until(
            rub_home,
            format!("attachment-{canonical_identity}"),
            deadline,
        )
        .await?;
    Ok(Some(canonical_identity))
}

async fn acquire_startup_lock_file_until(
    rub_home: &Path,
    scope_key: &str,
    deadline: Instant,
) -> Result<std::fs::File, RubError> {
    let paths = RubPaths::new(rub_home);
    std::fs::create_dir_all(paths.startup_locks_dir())?;
    let lock_path = paths.startup_lock_path(scope_key);
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
            Ok(()) => return Ok(file),
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

async fn canonical_startup_attachment_identity(
    attachment_identity: Option<&str>,
    deadline: Instant,
) -> Result<Option<String>, RubError> {
    let Some(identity) = attachment_identity else {
        return Ok(None);
    };
    let Some((kind, value)) = identity.split_once(':') else {
        return Ok(Some(identity.to_string()));
    };
    match kind {
        "cdp" => Ok(Some(format!(
            "cdp:{}",
            rub_cdp::attachment::canonical_external_browser_identity_until(value, deadline.into())
                .await?
        ))),
        "auto_discover" if value == "local_cdp" => {
            let candidate =
                rub_cdp::attachment::resolve_unique_local_cdp_candidate_until(deadline.into())
                    .await?;
            Ok(Some(format!(
                "cdp:{}",
                rub_cdp::attachment::canonical_external_browser_identity_until(
                    &candidate.ws_url,
                    deadline.into(),
                )
                .await?
            )))
        }
        _ => Ok(Some(identity.to_string())),
    }
}

#[cfg(test)]
pub async fn wait_for_ready(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    timeout_ms: u64,
) -> Result<(IpcClient, String), RubError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    wait_for_ready_until(rub_home, session_name, signals, deadline, None).await
}

pub async fn wait_for_ready_until(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    deadline: Instant,
    expected_attachment_identity: Option<&str>,
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
                let stderr_excerpt = read_startup_stderr_excerpt(&signals.stderr_file);
                return Err(RubError::domain_with_context(
                    ErrorCode::DaemonStartFailed,
                    stderr_excerpt
                        .as_deref()
                        .unwrap_or(
                            "Daemon exited before startup authority committed and did not publish a structured startup error",
                        )
                        .to_string(),
                    serde_json::json!({
                        "reason": "daemon_exited_before_startup_commit",
                        "session": session_name,
                        "session_id": signals.session_id,
                        "daemon_pid": signals.daemon_pid,
                        "ready_marker_present": ready_written,
                        "commit_marker_value": committed_session_id,
                        "startup_stderr_file": signals.stderr_file.display().to_string(),
                        "startup_stderr_state": daemon_ctl_path_state(
                            "daemon_ctl.startup.stderr_file",
                            "startup_stderr_signal_file",
                            "startup_stderr_file",
                        ),
                        "startup_stderr_excerpt": stderr_excerpt,
                    }),
                ));
            }
            StartupReadinessObservation::ReadyToHandshake => {
                match connect_ready_client(
                    &monitor.socket_path,
                    &signals.session_id,
                    expected_attachment_identity,
                    deadline,
                )
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

    let mut error = RubError::domain(
        ErrorCode::DaemonStartFailed,
        format!("Daemon did not become ready within {}ms", timeout_ms),
    );
    if let Some(stderr_excerpt) = read_startup_stderr_excerpt(&signals.stderr_file) {
        error = RubError::domain_with_context(
            ErrorCode::DaemonStartFailed,
            format!("Daemon did not become ready within {}ms", timeout_ms),
            serde_json::json!({
                "reason": "startup_timeout_with_stderr_excerpt",
                "session": session_name,
                "session_id": signals.session_id,
                "daemon_pid": signals.daemon_pid,
                "startup_stderr_file": signals.stderr_file.display().to_string(),
                "startup_stderr_state": daemon_ctl_path_state(
                    "daemon_ctl.startup.stderr_file",
                    "startup_stderr_signal_file",
                    "startup_stderr_file",
                ),
                "startup_stderr_excerpt": stderr_excerpt,
            }),
        );
    }
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

fn read_startup_stderr_excerpt(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
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
    expected_attachment_identity: Option<&str>,
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
        let handshake_socket_identity = match super::super::current_socket_path_identity(
            &socket_path,
            "daemon_ctl.startup.handshake.socket_path",
            "startup_ready_monitor.socket_path",
            ErrorCode::DaemonStartFailed,
            "verified_daemon_authority_socket_identity_read_failed",
        ) {
            Ok(identity) => identity,
            Err(error) => {
                return Err(RetryFailure {
                    error,
                    attribution,
                    final_failure_class: ConnectionFailureClass::ProtocolMismatch,
                });
            }
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
            super::super::verify_socket_path_identity(
                &socket_path,
                handshake_socket_identity,
                &AuthorityBoundConnectSpec {
                    phase: "startup_handshake",
                    error_code: ErrorCode::DaemonStartFailed,
                    message_prefix:
                        "Failed to connect to the daemon socket while confirming startup readiness",
                    path_authority: "daemon_ctl.startup.handshake.socket_path",
                    upstream_truth: "startup_ready_monitor.socket_path",
                },
            )
            .map_err(|error| {
                AttemptError::terminal(error, ConnectionFailureClass::ProtocolMismatch)
            })?;

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
            super::super::validate_handshake_attachment_identity(
                "startup",
                expected_attachment_identity,
                handshake.attachment_identity.as_deref(),
                &socket_path,
                "daemon_ctl.startup.handshake.socket_path",
                "startup_ready_monitor.socket_path",
            )
            .map_err(|error| AttemptError::terminal(error, ConnectionFailureClass::ProtocolMismatch))?;

            let client = authority_bound_connected_client(
                &socket_path,
                &handshake.daemon_session_id,
                handshake_socket_identity,
                Some(super::super::AttachBudget {
                    deadline,
                    timeout_ms: remaining_budget_ms(deadline).max(1),
                }),
                AuthorityBoundConnectSpec {
                    phase: "startup_handshake_bound_authority_connect",
                    error_code: ErrorCode::DaemonStartFailed,
                    message_prefix: "Failed to connect the verified startup daemon authority",
                    path_authority: "daemon_ctl.startup.handshake.socket_path",
                    upstream_truth: "startup_ready_monitor.socket_path",
                },
            )
            .await
            .map_err(|error| {
                AttemptError::terminal(error, ConnectionFailureClass::ProtocolMismatch)
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

pub(crate) fn startup_ready_retry_timeout_failure(
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

pub(crate) fn read_startup_error(path: &Path) -> Result<rub_core::error::ErrorEnvelope, RubError> {
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

#[cfg(test)]
mod tests {
    use super::{STARTUP_INPUTS_ENV, apply_authoritative_startup_inputs_env};
    use crate::daemon_ctl::AuthoritativeStartupInputs;
    use crate::session_policy::ConnectionRequest;
    use std::process::Command;

    #[test]
    fn startup_inputs_env_is_explicitly_removed_when_parent_has_no_authority() {
        let mut cmd = Command::new("rub");
        cmd.env(STARTUP_INPUTS_ENV, "stale-parent-authority");

        apply_authoritative_startup_inputs_env(&mut cmd, None)
            .expect("env removal should not fail");

        let (_, value) = cmd
            .get_envs()
            .find(|(key, _)| *key == STARTUP_INPUTS_ENV)
            .expect("env_remove should be recorded on command");
        assert!(value.is_none());
    }

    #[test]
    fn startup_inputs_env_is_set_only_from_authoritative_parent_payload() {
        let mut cmd = Command::new("rub");
        let inputs = AuthoritativeStartupInputs {
            connection_request: ConnectionRequest::None,
            attachment_identity: None,
        };

        apply_authoritative_startup_inputs_env(&mut cmd, Some(&inputs))
            .expect("env serialization should succeed");

        let (_, value) = cmd
            .get_envs()
            .find(|(key, _)| *key == STARTUP_INPUTS_ENV)
            .expect("startup inputs env should be set");
        assert!(
            value
                .and_then(|value| value.to_str())
                .is_some_and(|raw| raw.contains("\"connection_request\""))
        );
    }
}
