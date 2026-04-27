use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use rub_core::error::ErrorCode;
use rub_core::error::RubError;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use serde_json::json;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use super::connect::{TransientSocketPolicy, detect_or_connect_hardened_until};
use super::process_identity::process_matches_failed_startup_identity;
use super::registry::cleanup_stale;
use super::startup::{
    AuthoritativeStartupInputs, StartupCleanupAuthorityKind, StartupCleanupProof,
    StartupSignalFiles, acquire_startup_lock_until, clear_startup_cleanup_proof,
    read_startup_cleanup_proof, start_daemon, upgrade_startup_lock_to_canonical_attachment_until,
    wait_for_ready_until,
};
use super::{
    DaemonConnection, force_kill_process, remaining_budget_duration, terminate_spawned_daemon,
    wait_for_process_exit,
};

pub struct StartupAuthorityRequest<'a> {
    pub connection_request: &'a crate::session_policy::ConnectionRequest,
    pub attachment_identity: Option<&'a str>,
}

pub struct BootstrapClient {
    pub client: IpcClient,
    pub connected_to_existing_daemon: bool,
    pub daemon_session_id: Option<String>,
    pub authority_socket_path: PathBuf,
}

struct BootstrapResolution {
    client: IpcClient,
    connected_to_existing_daemon: bool,
    daemon_session_id: Option<String>,
    authority_socket_path: PathBuf,
}

impl BootstrapResolution {
    fn connected(
        client: IpcClient,
        daemon_session_id: Option<String>,
        authority_socket_path: PathBuf,
    ) -> Self {
        Self {
            client,
            connected_to_existing_daemon: true,
            daemon_session_id,
            authority_socket_path,
        }
    }

    fn started(
        client: IpcClient,
        daemon_session_id: String,
        authority_socket_path: PathBuf,
    ) -> Self {
        Self {
            client,
            connected_to_existing_daemon: false,
            daemon_session_id: Some(daemon_session_id),
            authority_socket_path,
        }
    }
}

#[derive(Default)]
struct FailedStartupCleanupSummary {
    browser_cleanup_attempted: bool,
    browser_cleanup_succeeded: bool,
    browser_cleanup_error: Option<String>,
    browser_cleanup_authority: Option<StartupCleanupProof>,
    browser_cleanup_proof_retained: bool,
    browser_cleanup_proof_clear_error: Option<String>,
    cleanup_timeout_exhausted: bool,
    cleanup_timeout_phase: Option<&'static str>,
    cleanup_timeout_ms: Option<u64>,
}

#[cfg(test)]
static FORCE_STARTUP_FALLBACK_CLEANUP_FAILURES: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

#[cfg(test)]
pub(crate) async fn cleanup_startup_fallback_browser_authority_for_test(
    path: &Path,
) -> (
    bool,
    bool,
    Option<StartupCleanupProof>,
    Option<String>,
    bool,
    Option<String>,
) {
    let summary = cleanup_startup_fallback_browser_authority(path).await;
    (
        summary.browser_cleanup_attempted,
        summary.browser_cleanup_succeeded,
        summary.browser_cleanup_authority,
        summary.browser_cleanup_error,
        summary.browser_cleanup_proof_retained,
        summary.browser_cleanup_proof_clear_error,
    )
}

#[cfg(test)]
pub(crate) async fn cleanup_startup_fallback_browser_authority_until_for_test(
    path: &Path,
    deadline: Instant,
    timeout_ms: u64,
) -> (
    bool,
    bool,
    Option<StartupCleanupProof>,
    Option<String>,
    bool,
    Option<String>,
    bool,
    Option<&'static str>,
    Option<u64>,
) {
    let summary =
        cleanup_startup_fallback_browser_authority_until(path, deadline, timeout_ms).await;
    (
        summary.browser_cleanup_attempted,
        summary.browser_cleanup_succeeded,
        summary.browser_cleanup_authority,
        summary.browser_cleanup_error,
        summary.browser_cleanup_proof_retained,
        summary.browser_cleanup_proof_clear_error,
        summary.cleanup_timeout_exhausted,
        summary.cleanup_timeout_phase,
        summary.cleanup_timeout_ms,
    )
}

#[cfg(test)]
pub(crate) fn force_startup_fallback_cleanup_failure_for_test(profile_dir: &Path) {
    let mut forced = FORCE_STARTUP_FALLBACK_CLEANUP_FAILURES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("startup fallback cleanup force registry poisoned");
    forced.push(profile_dir.to_path_buf());
}

#[cfg(test)]
fn take_forced_startup_fallback_cleanup_failure_for_test(profile_dir: &Path) -> bool {
    let mut forced = FORCE_STARTUP_FALLBACK_CLEANUP_FAILURES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("startup fallback cleanup force registry poisoned");
    let Some(index) = forced.iter().position(|path| path == profile_dir) else {
        return false;
    };
    forced.remove(index);
    true
}

pub async fn bootstrap_client(
    rub_home: &Path,
    session_name: &str,
    expected_daemon_session_id: Option<&str>,
    command_deadline: Instant,
    command_timeout_ms: u64,
    extra_args: &[String],
    startup_authority: StartupAuthorityRequest<'_>,
) -> Result<BootstrapClient, RubError> {
    let resolution = match detect_or_connect_hardened_until(
        rub_home,
        session_name,
        TransientSocketPolicy::NeedStartBeforeLock,
        command_deadline,
        command_timeout_ms,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
            authority_socket_path,
        } => {
            validate_existing_bootstrap_authority(
                session_name,
                expected_daemon_session_id,
                daemon_session_id.as_deref(),
            )?;
            BootstrapResolution::connected(client, daemon_session_id, authority_socket_path)
        }
        DaemonConnection::NeedStart => {
            if let Some(expected_daemon_session_id) = expected_daemon_session_id {
                return Err(existing_bootstrap_authority_unavailable(
                    session_name,
                    expected_daemon_session_id,
                ));
            }
            resolve_bootstrap_after_lock(
                rub_home,
                session_name,
                expected_daemon_session_id,
                command_deadline,
                command_timeout_ms,
                extra_args,
                startup_authority,
            )
            .await?
        }
    };

    Ok(BootstrapClient {
        client: resolution.client,
        connected_to_existing_daemon: resolution.connected_to_existing_daemon,
        daemon_session_id: resolution.daemon_session_id,
        authority_socket_path: resolution.authority_socket_path,
    })
}

async fn resolve_bootstrap_after_lock(
    rub_home: &Path,
    session_name: &str,
    expected_daemon_session_id: Option<&str>,
    command_deadline: Instant,
    command_timeout_ms: u64,
    extra_args: &[String],
    startup_authority: StartupAuthorityRequest<'_>,
) -> Result<BootstrapResolution, RubError> {
    let startup_session_id = rub_daemon::session::new_session_id();
    let mut startup_lock = acquire_startup_lock_until(
        rub_home,
        session_name,
        startup_authority.attachment_identity,
        command_deadline,
    )
    .await?;

    let resolution = match detect_or_connect_hardened_until(
        rub_home,
        session_name,
        TransientSocketPolicy::FailAfterLock,
        command_deadline,
        command_timeout_ms,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
            authority_socket_path,
        } => {
            validate_existing_bootstrap_authority(
                session_name,
                expected_daemon_session_id,
                daemon_session_id.as_deref(),
            )?;
            Ok(BootstrapResolution::connected(
                client,
                daemon_session_id,
                authority_socket_path,
            ))
        }
        DaemonConnection::NeedStart => {
            if let Some(expected_daemon_session_id) = expected_daemon_session_id {
                return Err(existing_bootstrap_authority_unavailable(
                    session_name,
                    expected_daemon_session_id,
                ));
            }
            let canonical_attachment_identity = upgrade_startup_lock_to_canonical_attachment_until(
                &mut startup_lock,
                rub_home,
                startup_authority.attachment_identity,
                command_deadline,
            )
            .await?;

            match detect_or_connect_hardened_until(
                rub_home,
                session_name,
                TransientSocketPolicy::FailAfterLock,
                command_deadline,
                command_timeout_ms,
            )
            .await?
            {
                DaemonConnection::Connected {
                    client,
                    daemon_session_id,
                    authority_socket_path,
                } => {
                    validate_existing_bootstrap_authority(
                        session_name,
                        expected_daemon_session_id,
                        daemon_session_id.as_deref(),
                    )?;
                    Ok(BootstrapResolution::connected(
                        client,
                        daemon_session_id,
                        authority_socket_path,
                    ))
                }
                DaemonConnection::NeedStart => {
                    start_new_daemon_bootstrap(
                        rub_home,
                        session_name,
                        &startup_session_id,
                        extra_args,
                        &AuthoritativeStartupInputs {
                            connection_request: startup_authority.connection_request.clone(),
                            attachment_identity: canonical_attachment_identity.or_else(|| {
                                startup_authority.attachment_identity.map(str::to_string)
                            }),
                        },
                        command_deadline,
                        command_timeout_ms,
                    )
                    .await
                }
            }
        }
    };

    drop(startup_lock);
    resolution
}

fn validate_existing_bootstrap_authority(
    session_name: &str,
    expected_daemon_session_id: Option<&str>,
    actual_daemon_session_id: Option<&str>,
) -> Result<(), RubError> {
    let Some(expected_daemon_session_id) = expected_daemon_session_id else {
        return Ok(());
    };

    match actual_daemon_session_id {
        Some(actual_daemon_session_id)
            if actual_daemon_session_id == expected_daemon_session_id =>
        {
            Ok(())
        }
        Some(actual_daemon_session_id) => Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Session '{}' resolved to daemon '{}' but bootstrap connected to '{}'",
                session_name, expected_daemon_session_id, actual_daemon_session_id
            ),
            json!({
                "reason": "existing_session_bootstrap_authority_mismatch",
                "expected_daemon_session_id": expected_daemon_session_id,
                "actual_daemon_session_id": actual_daemon_session_id,
            }),
        )),
        None => Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Session '{}' resolved to daemon '{}' but bootstrap could not confirm the connected daemon identity",
                session_name, expected_daemon_session_id
            ),
            json!({
                "reason": "existing_session_bootstrap_authority_missing",
                "expected_daemon_session_id": expected_daemon_session_id,
            }),
        )),
    }
}

fn existing_bootstrap_authority_unavailable(
    session_name: &str,
    expected_daemon_session_id: &str,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::DaemonNotRunning,
        format!(
            "Session '{}' previously resolved to daemon '{}' but that live daemon is no longer available",
            session_name, expected_daemon_session_id
        ),
        json!({
            "reason": "existing_session_bootstrap_authority_unavailable",
            "expected_daemon_session_id": expected_daemon_session_id,
        }),
    )
}

async fn start_new_daemon_bootstrap(
    rub_home: &Path,
    session_name: &str,
    startup_session_id: &str,
    extra_args: &[String],
    authoritative_startup_inputs: &AuthoritativeStartupInputs,
    command_deadline: Instant,
    command_timeout_ms: u64,
) -> Result<BootstrapResolution, RubError> {
    let signals = start_daemon(
        rub_home,
        session_name,
        startup_session_id,
        extra_args,
        Some(authoritative_startup_inputs),
    )?;
    let ready = wait_for_ready_until(
        rub_home,
        session_name,
        &signals,
        command_deadline,
        authoritative_startup_inputs.attachment_identity.as_deref(),
    )
    .await;
    match ready {
        Ok((client, daemon_session_id)) => {
            clear_committed_startup_cleanup_proof(
                &signals.cleanup_file,
                daemon_session_id.as_str(),
            )?;
            let authority_socket_path = RubPaths::new(rub_home)
                .session_runtime(session_name, &daemon_session_id)
                .socket_path();
            Ok(BootstrapResolution::started(
                client,
                daemon_session_id,
                authority_socket_path,
            ))
        }
        Err(error) => {
            let cleanup_summary = cleanup_failed_startup(
                rub_home,
                session_name,
                &signals,
                command_deadline,
                command_timeout_ms,
            )
            .await;
            Err(annotate_failed_startup_cleanup(error, cleanup_summary))
        }
    }
}

fn clear_committed_startup_cleanup_proof(
    path: &Path,
    daemon_session_id: &str,
) -> Result<(), RubError> {
    clear_startup_cleanup_proof(path).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::DaemonStartFailed,
            format!(
                "Daemon reported ready for session '{daemon_session_id}' but startup cleanup proof {} could not be cleared: {error}",
                path.display()
            ),
            json!({
                "reason": "startup_cleanup_proof_clear_failed_after_ready",
                "daemon_session_id": daemon_session_id,
                "cleanup_file": path.display().to_string(),
                "startup_fallback_browser_cleanup_proof_retained": path.exists(),
            }),
        )
    })
}

async fn cleanup_failed_startup(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    command_deadline: Instant,
    command_timeout_ms: u64,
) -> FailedStartupCleanupSummary {
    let mut summary = FailedStartupCleanupSummary::default();
    let Some(remaining) = remaining_budget_duration(command_deadline) else {
        note_failed_startup_cleanup_timeout(
            &mut summary,
            "failed_startup_process_cleanup",
            command_timeout_ms,
            &signals.cleanup_file,
        );
        cleanup_failed_startup_signal_files(signals, summary.browser_cleanup_proof_retained);
        return summary;
    };
    if tokio::time::timeout(
        remaining,
        terminate_failed_startup_process(rub_home, session_name, signals),
    )
    .await
    .is_err()
    {
        note_failed_startup_cleanup_timeout(
            &mut summary,
            "failed_startup_process_cleanup",
            command_timeout_ms,
            &signals.cleanup_file,
        );
        cleanup_failed_startup_signal_files(signals, summary.browser_cleanup_proof_retained);
        return summary;
    }

    summary = cleanup_startup_fallback_browser_authority_until(
        &signals.cleanup_file,
        command_deadline,
        command_timeout_ms,
    )
    .await;
    if summary.cleanup_timeout_exhausted {
        cleanup_failed_startup_signal_files(signals, summary.browser_cleanup_proof_retained);
        return summary;
    }

    let runtime_paths = RubPaths::new(rub_home).session_runtime(session_name, &signals.session_id);
    let socket_paths = runtime_paths.actual_socket_paths();
    if !wait_for_failed_startup_runtime_release_until(
        signals.daemon_pid,
        &socket_paths,
        command_deadline,
    )
    .await
    {
        note_failed_startup_cleanup_timeout(
            &mut summary,
            "failed_startup_runtime_release",
            command_timeout_ms,
            &signals.cleanup_file,
        );
        cleanup_failed_startup_signal_files(signals, summary.browser_cleanup_proof_retained);
        return summary;
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

    cleanup_failed_startup_signal_files(signals, summary.browser_cleanup_proof_retained);
    summary
}

fn cleanup_failed_startup_signal_files(signals: &StartupSignalFiles, retain_cleanup_proof: bool) {
    let _ = std::fs::remove_file(&signals.ready_file);
    let _ = std::fs::remove_file(&signals.error_file);
    if !retain_cleanup_proof {
        let _ = std::fs::remove_file(&signals.cleanup_file);
    }
}

#[cfg(test)]
pub(crate) fn cleanup_failed_startup_signal_files_for_test(
    signals: &StartupSignalFiles,
    retain_cleanup_proof: bool,
) {
    cleanup_failed_startup_signal_files(signals, retain_cleanup_proof);
}

async fn terminate_failed_startup_process(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
) -> std::io::Result<()> {
    let runtime_paths = RubPaths::new(rub_home).session_runtime(session_name, &signals.session_id);
    if !process_matches_failed_startup_identity(
        rub_home,
        session_name,
        signals.session_id.as_str(),
        &runtime_paths.socket_path(),
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
    let _ = terminate_spawned_daemon(signals.daemon_pid);
    if wait_for_process_exit(signals.daemon_pid, std::time::Duration::from_secs(2)).await {
        return Ok(());
    }
    if !process_matches_failed_startup_identity(
        rub_home,
        session_name,
        signals.session_id.as_str(),
        &runtime_paths.socket_path(),
        signals.daemon_pid,
    )? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Refused to force kill pid {} because failed-startup daemon authority changed before escalation for session '{}' under {}",
                signals.daemon_pid,
                session_name,
                rub_home.display()
            ),
        ));
    }
    force_kill_process(signals.daemon_pid)
}

#[cfg(test)]
async fn cleanup_startup_fallback_browser_authority(path: &Path) -> FailedStartupCleanupSummary {
    cleanup_startup_fallback_browser_authority_until(
        path,
        Instant::now() + std::time::Duration::from_secs(5),
        5_000,
    )
    .await
}

async fn cleanup_startup_fallback_browser_authority_until(
    path: &Path,
    deadline: Instant,
    timeout_ms: u64,
) -> FailedStartupCleanupSummary {
    let mut summary = FailedStartupCleanupSummary::default();
    if !path.exists() {
        return summary;
    }

    summary.browser_cleanup_attempted = true;
    match read_startup_cleanup_proof(path) {
        Ok(proof) => {
            summary.browser_cleanup_authority = Some(proof.clone());
            let Some(remaining) = remaining_budget_duration(deadline) else {
                note_failed_startup_cleanup_timeout(
                    &mut summary,
                    "startup_fallback_browser_cleanup",
                    timeout_ms,
                    path,
                );
                return summary;
            };
            let cleanup_result =
                tokio::time::timeout(remaining, run_startup_fallback_browser_cleanup(&proof)).await;
            match cleanup_result {
                Ok(Ok(())) => {
                    summary.browser_cleanup_succeeded = true;
                    if let Err(error) = clear_startup_cleanup_proof(path) {
                        summary.browser_cleanup_proof_retained = path.exists();
                        summary.browser_cleanup_proof_clear_error = Some(error.to_string());
                    }
                }
                Ok(Err(error)) => {
                    summary.browser_cleanup_error = Some(error.to_string());
                    summary.browser_cleanup_proof_retained = path.exists();
                }
                Err(_) => {
                    note_failed_startup_cleanup_timeout(
                        &mut summary,
                        "startup_fallback_browser_cleanup",
                        timeout_ms,
                        path,
                    );
                }
            }
        }
        Err(error) => {
            summary.browser_cleanup_error = Some(error.to_string());
            summary.browser_cleanup_proof_retained = path.exists();
        }
    }
    summary
}

fn note_failed_startup_cleanup_timeout(
    summary: &mut FailedStartupCleanupSummary,
    phase: &'static str,
    timeout_ms: u64,
    cleanup_file: &Path,
) {
    summary.cleanup_timeout_exhausted = true;
    summary.cleanup_timeout_phase = Some(phase);
    summary.cleanup_timeout_ms = Some(timeout_ms);
    summary.browser_cleanup_proof_retained |= cleanup_file.exists();
}

async fn wait_for_failed_startup_runtime_release_until(
    daemon_pid: u32,
    socket_paths: &[PathBuf],
    deadline: Instant,
) -> bool {
    loop {
        if !rub_core::process::is_process_alive(daemon_pid)
            && socket_paths.iter().all(|path| !path.exists())
        {
            return true;
        }

        let Some(remaining) = remaining_budget_duration(deadline) else {
            return false;
        };
        let sleep_for = std::cmp::min(std::time::Duration::from_millis(100), remaining);
        tokio::time::sleep(sleep_for).await;
    }
}

async fn run_startup_fallback_browser_cleanup(proof: &StartupCleanupProof) -> Result<(), RubError> {
    #[cfg(test)]
    if take_forced_startup_fallback_cleanup_failure_for_test(Path::new(
        &proof.managed_user_data_dir,
    )) {
        return Err(RubError::domain(
            ErrorCode::ProfileInUse,
            "forced startup fallback cleanup failure".to_string(),
        ));
    }

    match proof.kind {
        StartupCleanupAuthorityKind::ManagedBrowserProfileFallback => {
            rub_cdp::cleanup_managed_profile_authority(
                &proof.managed_user_data_dir,
                proof.managed_profile_directory.as_deref(),
                proof.ephemeral,
            )
            .await
        }
    }
}

fn annotate_failed_startup_cleanup(
    error: RubError,
    cleanup_summary: FailedStartupCleanupSummary,
) -> RubError {
    if !cleanup_summary.browser_cleanup_attempted && !cleanup_summary.cleanup_timeout_exhausted {
        return error;
    }

    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert(
        "startup_fallback_browser_cleanup_attempted".to_string(),
        serde_json::json!(true),
    );
    context.insert(
        "startup_fallback_browser_cleanup_succeeded".to_string(),
        serde_json::json!(cleanup_summary.browser_cleanup_succeeded),
    );
    context.insert(
        "startup_fallback_browser_cleanup_proof_retained".to_string(),
        serde_json::json!(cleanup_summary.browser_cleanup_proof_retained),
    );
    context.insert(
        "startup_fallback_cleanup_timed_out".to_string(),
        serde_json::json!(cleanup_summary.cleanup_timeout_exhausted),
    );
    if let Some(proof) = cleanup_summary.browser_cleanup_authority {
        context.insert(
            "startup_fallback_browser_cleanup_authority".to_string(),
            serde_json::to_value(proof).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(error) = cleanup_summary.browser_cleanup_error {
        context.insert(
            "startup_fallback_browser_cleanup_error".to_string(),
            serde_json::json!(error),
        );
    }
    if let Some(error) = cleanup_summary.browser_cleanup_proof_clear_error {
        context.insert(
            "startup_fallback_browser_cleanup_proof_clear_error".to_string(),
            serde_json::json!(error),
        );
    }
    if let Some(phase) = cleanup_summary.cleanup_timeout_phase {
        context.insert(
            "startup_fallback_cleanup_timeout_phase".to_string(),
            serde_json::json!(phase),
        );
    }
    if let Some(timeout_ms) = cleanup_summary.cleanup_timeout_ms {
        context.insert(
            "startup_fallback_cleanup_timeout_ms".to_string(),
            serde_json::json!(timeout_ms),
        );
    }
    envelope.context = Some(serde_json::Value::Object(context));
    RubError::Domain(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn validate_existing_bootstrap_authority_rejects_connected_daemon_session_mismatch() {
        let error = validate_existing_bootstrap_authority(
            "default",
            Some("sess-expected"),
            Some("sess-live"),
        )
        .expect_err("mismatched daemon session authority must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("existing_session_bootstrap_authority_mismatch")
        );
    }

    #[test]
    fn validate_existing_bootstrap_authority_rejects_missing_connected_daemon_session() {
        let error = validate_existing_bootstrap_authority("default", Some("sess-expected"), None)
            .expect_err("missing daemon session authority must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("existing_session_bootstrap_authority_missing")
        );
    }

    #[test]
    fn committed_startup_cleanup_clear_failure_is_not_silent() {
        let cleanup_path = temp_path("rub-startup-cleanup-proof-dir");
        std::fs::create_dir_all(&cleanup_path).expect("create cleanup dir");

        let error = clear_committed_startup_cleanup_proof(&cleanup_path, "sess-ready")
            .expect_err("retained startup cleanup proof must be caller-visible after ready");
        let envelope = error.into_envelope();

        assert_eq!(envelope.code, ErrorCode::DaemonStartFailed);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("startup_cleanup_proof_clear_failed_after_ready")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("daemon_session_id"))
                .and_then(|value| value.as_str()),
            Some("sess-ready")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("startup_fallback_browser_cleanup_proof_retained"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );

        let _ = std::fs::remove_dir_all(cleanup_path);
    }

    #[tokio::test]
    async fn startup_fallback_cleanup_clears_proof_on_success() {
        let cleanup_file = temp_path("rub-startup-cleanup-proof");
        let profile_dir = temp_path("rub-startup-cleanup-profile");
        std::fs::create_dir_all(&profile_dir).expect("create profile dir");
        crate::daemon_ctl::write_startup_cleanup_proof_at(
            &cleanup_file,
            &StartupCleanupProof {
                kind: StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
                managed_user_data_dir: profile_dir.display().to_string(),
                managed_profile_directory: Some("Profile 3".to_string()),
                ephemeral: false,
            },
        )
        .expect("write startup cleanup proof");

        let (attempted, succeeded, _authority, error, retained, clear_error) =
            cleanup_startup_fallback_browser_authority_for_test(&cleanup_file).await;

        assert!(attempted);
        assert!(succeeded);
        assert!(error.is_none());
        assert!(!retained);
        assert!(clear_error.is_none());
        assert!(!cleanup_file.exists());

        let _ = std::fs::remove_dir_all(profile_dir);
    }

    #[tokio::test]
    async fn startup_fallback_cleanup_retains_proof_when_cleanup_fails() {
        let cleanup_file = temp_path("rub-startup-cleanup-proof");
        let profile_dir = temp_path("rub-startup-cleanup-profile");
        crate::daemon_ctl::write_startup_cleanup_proof_at(
            &cleanup_file,
            &StartupCleanupProof {
                kind: StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
                managed_user_data_dir: profile_dir.display().to_string(),
                managed_profile_directory: Some("Profile 3".to_string()),
                ephemeral: false,
            },
        )
        .expect("write startup cleanup proof");
        force_startup_fallback_cleanup_failure_for_test(&profile_dir);

        let (attempted, succeeded, _authority, error, retained, clear_error) =
            cleanup_startup_fallback_browser_authority_for_test(&cleanup_file).await;

        assert!(attempted);
        assert!(!succeeded);
        assert!(error.is_some());
        assert!(retained);
        assert!(clear_error.is_none());
        assert!(cleanup_file.exists());

        let _ = std::fs::remove_file(cleanup_file);
    }

    #[tokio::test]
    async fn startup_fallback_cleanup_retains_proof_when_command_deadline_is_exhausted() {
        let cleanup_file = temp_path("rub-startup-cleanup-proof");
        let profile_dir = temp_path("rub-startup-cleanup-profile");
        crate::daemon_ctl::write_startup_cleanup_proof_at(
            &cleanup_file,
            &StartupCleanupProof {
                kind: StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
                managed_user_data_dir: profile_dir.display().to_string(),
                managed_profile_directory: Some("Profile 3".to_string()),
                ephemeral: false,
            },
        )
        .expect("write startup cleanup proof");

        let (
            attempted,
            succeeded,
            _authority,
            error,
            retained,
            clear_error,
            timed_out,
            timeout_phase,
            timeout_ms,
        ) = cleanup_startup_fallback_browser_authority_until_for_test(
            &cleanup_file,
            Instant::now() - std::time::Duration::from_millis(1),
            25,
        )
        .await;

        assert!(attempted);
        assert!(!succeeded);
        assert!(error.is_none());
        assert!(retained);
        assert!(clear_error.is_none());
        assert!(timed_out);
        assert_eq!(timeout_phase, Some("startup_fallback_browser_cleanup"));
        assert_eq!(timeout_ms, Some(25));
        assert!(cleanup_file.exists());

        let _ = std::fs::remove_file(cleanup_file);
    }

    #[tokio::test]
    async fn failed_startup_runtime_release_wait_honors_expired_deadline() {
        let socket_path = temp_path("rub-startup-runtime-socket");
        std::fs::write(&socket_path, b"live").expect("seed runtime socket");
        let released = wait_for_failed_startup_runtime_release_until(
            std::process::id(),
            std::slice::from_ref(&socket_path),
            Instant::now() - std::time::Duration::from_millis(1),
        )
        .await;
        assert!(
            !released,
            "runtime release fence must fail closed once the shared command deadline is exhausted"
        );
        assert!(socket_path.exists());

        let _ = std::fs::remove_file(socket_path);
    }

    #[test]
    fn failed_startup_signal_cleanup_retains_cleanup_proof_when_requested() {
        let ready_file = temp_path("rub-startup-ready");
        let error_file = temp_path("rub-startup-error");
        let cleanup_file = temp_path("rub-startup-cleanup");
        std::fs::write(&ready_file, b"ready").unwrap();
        std::fs::write(&error_file, b"error").unwrap();
        std::fs::write(&cleanup_file, b"cleanup").unwrap();
        let signals = StartupSignalFiles {
            ready_file: ready_file.clone(),
            error_file: error_file.clone(),
            stderr_file: temp_path("rub-startup-stderr"),
            cleanup_file: cleanup_file.clone(),
            daemon_pid: 1,
            session_id: "sess-1".to_string(),
        };

        cleanup_failed_startup_signal_files_for_test(&signals, true);

        assert!(!ready_file.exists());
        assert!(!error_file.exists());
        assert!(cleanup_file.exists());

        let _ = std::fs::remove_file(cleanup_file);
    }
}
