use std::path::Path;
use std::time::Instant;

use rub_core::error::ErrorCode;
use rub_core::error::RubError;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use serde_json::json;

use super::connect::{TransientSocketPolicy, detect_or_connect_hardened_until};
use super::process_identity::process_matches_failed_startup_identity;
use super::registry::cleanup_stale;
use super::startup::{
    StartupCleanupAuthorityKind, StartupCleanupProof, StartupSignalFiles,
    acquire_startup_lock_until, clear_startup_cleanup_proof, read_startup_cleanup_proof,
    start_daemon, upgrade_startup_lock_to_canonical_attachment_until, wait_for_ready_until,
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

#[derive(Default)]
struct FailedStartupCleanupSummary {
    browser_cleanup_attempted: bool,
    browser_cleanup_succeeded: bool,
    browser_cleanup_error: Option<String>,
    browser_cleanup_authority: Option<StartupCleanupProof>,
}

#[cfg(test)]
pub(crate) async fn cleanup_precommit_browser_authority_for_test(
    path: &Path,
) -> (bool, bool, Option<StartupCleanupProof>, Option<String>) {
    let summary = cleanup_precommit_browser_authority(path).await;
    (
        summary.browser_cleanup_attempted,
        summary.browser_cleanup_succeeded,
        summary.browser_cleanup_authority,
        summary.browser_cleanup_error,
    )
}

pub async fn bootstrap_client(
    rub_home: &Path,
    session_name: &str,
    expected_daemon_session_id: Option<&str>,
    command_deadline: Instant,
    command_timeout_ms: u64,
    extra_args: &[String],
    attachment_identity: Option<&str>,
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
        } => {
            validate_existing_bootstrap_authority(
                session_name,
                expected_daemon_session_id,
                daemon_session_id.as_deref(),
            )?;
            BootstrapResolution::connected(client, daemon_session_id)
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
    expected_daemon_session_id: Option<&str>,
    command_deadline: Instant,
    command_timeout_ms: u64,
    extra_args: &[String],
    attachment_identity: Option<&str>,
) -> Result<BootstrapResolution, RubError> {
    let startup_session_id = rub_daemon::session::new_session_id();
    let mut startup_lock = acquire_startup_lock_until(
        rub_home,
        session_name,
        attachment_identity,
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
        } => {
            validate_existing_bootstrap_authority(
                session_name,
                expected_daemon_session_id,
                daemon_session_id.as_deref(),
            )?;
            Ok(BootstrapResolution::connected(client, daemon_session_id))
        }
        DaemonConnection::NeedStart => {
            if let Some(expected_daemon_session_id) = expected_daemon_session_id {
                return Err(existing_bootstrap_authority_unavailable(
                    session_name,
                    expected_daemon_session_id,
                ));
            }
            upgrade_startup_lock_to_canonical_attachment_until(
                &mut startup_lock,
                rub_home,
                attachment_identity,
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
                } => {
                    validate_existing_bootstrap_authority(
                        session_name,
                        expected_daemon_session_id,
                        daemon_session_id.as_deref(),
                    )?;
                    Ok(BootstrapResolution::connected(client, daemon_session_id))
                }
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
    command_deadline: Instant,
) -> Result<BootstrapResolution, RubError> {
    let signals = start_daemon(rub_home, session_name, startup_session_id, extra_args)?;
    let ready = wait_for_ready_until(rub_home, session_name, &signals, command_deadline).await;
    match ready {
        Ok((client, daemon_session_id)) => {
            let _ = clear_startup_cleanup_proof(&signals.cleanup_file);
            Ok(BootstrapResolution::started(client, daemon_session_id))
        }
        Err(error) => {
            let cleanup_summary = cleanup_failed_startup(rub_home, session_name, &signals).await;
            Err(annotate_failed_startup_cleanup(error, cleanup_summary))
        }
    }
}

async fn cleanup_failed_startup(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
) -> FailedStartupCleanupSummary {
    let _ = terminate_failed_startup_process(rub_home, session_name, signals).await;
    let summary = cleanup_precommit_browser_authority(&signals.cleanup_file).await;

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
    let _ = std::fs::remove_file(&signals.cleanup_file);
    summary
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
    terminate_spawned_daemon_force(signals.daemon_pid).await
}

async fn cleanup_precommit_browser_authority(path: &Path) -> FailedStartupCleanupSummary {
    let mut summary = FailedStartupCleanupSummary::default();
    if !path.exists() {
        return summary;
    }

    summary.browser_cleanup_attempted = true;
    match read_startup_cleanup_proof(path) {
        Ok(proof) => {
            summary.browser_cleanup_authority = Some(proof.clone());
            let cleanup_result = match proof.kind {
                StartupCleanupAuthorityKind::ManagedBrowserProfile => {
                    rub_cdp::cleanup_managed_profile_authority(
                        &proof.managed_user_data_dir,
                        proof.ephemeral,
                    )
                    .await
                }
            };
            match cleanup_result {
                Ok(()) => {
                    summary.browser_cleanup_succeeded = true;
                }
                Err(error) => {
                    summary.browser_cleanup_error = Some(error.to_string());
                }
            }
        }
        Err(error) => {
            summary.browser_cleanup_error = Some(error.to_string());
        }
    }

    let _ = clear_startup_cleanup_proof(path);
    summary
}

fn annotate_failed_startup_cleanup(
    error: RubError,
    cleanup_summary: FailedStartupCleanupSummary,
) -> RubError {
    if !cleanup_summary.browser_cleanup_attempted {
        return error;
    }

    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert(
        "startup_precommit_browser_cleanup_attempted".to_string(),
        serde_json::json!(true),
    );
    context.insert(
        "startup_precommit_browser_cleanup_succeeded".to_string(),
        serde_json::json!(cleanup_summary.browser_cleanup_succeeded),
    );
    if let Some(proof) = cleanup_summary.browser_cleanup_authority {
        context.insert(
            "startup_precommit_browser_cleanup_authority".to_string(),
            serde_json::to_value(proof).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(error) = cleanup_summary.browser_cleanup_error {
        context.insert(
            "startup_precommit_browser_cleanup_error".to_string(),
            serde_json::json!(error),
        );
    }
    envelope.context = Some(serde_json::Value::Object(context));
    RubError::Domain(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
