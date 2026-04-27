use crate::timeout_budget::deadline_from_start;
use crate::timeout_budget::helpers::mutating_request;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_daemon::rub_paths::RubPaths;
use serde_json::json;
use std::path::Path;
use std::time::Instant;

use super::{
    BatchCloseResult, BatchCloseSessionError, CompatibilityDegradedOwnedSession, DaemonConnection,
    ShutdownFenceStatus, TransientSocketPolicy, cleanup_stale,
    compatibility_degraded_owned_from_snapshot, detect_or_connect_hardened_until,
    registry_authority_snapshot, remaining_budget_ms, send_existing_request_with_replay_recovery,
    terminate_registry_entry_process, wait_for_shutdown_until,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseAllDisposition {
    Closed,
    CleanedStale,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) struct CloseAllSessionTarget {
    pub(crate) session_name: String,
    pub(crate) authority_entry: Option<rub_daemon::session::RegistryEntry>,
    pub(crate) compatibility_degraded_owned: Option<CompatibilityDegradedOwnedSession>,
    pub(crate) stale_entries: Vec<rub_daemon::session::RegistryEntry>,
    pub(crate) has_uncertain_entries: bool,
}

pub(crate) async fn close_all_sessions(
    rub_home: &Path,
    timeout: u64,
) -> Result<BatchCloseResult, RubError> {
    let command_deadline = deadline_from_start(Instant::now(), timeout);
    close_all_sessions_until(rub_home, command_deadline).await
}

pub(crate) async fn close_all_sessions_until(
    rub_home: &Path,
    command_deadline: Instant,
) -> Result<BatchCloseResult, RubError> {
    if !rub_home.exists() {
        return Ok(BatchCloseResult {
            closed: Vec::new(),
            cleaned_stale: Vec::new(),
            compatibility_degraded_owned_sessions: Vec::new(),
            failed: Vec::new(),
            session_error_details: Vec::new(),
        });
    }

    let snapshot = registry_authority_snapshot(rub_home)?;
    if snapshot.sessions.is_empty() {
        return Ok(BatchCloseResult {
            closed: Vec::new(),
            cleaned_stale: Vec::new(),
            compatibility_degraded_owned_sessions: Vec::new(),
            failed: Vec::new(),
            session_error_details: Vec::new(),
        });
    }

    let mut closed = Vec::new();
    let mut cleaned_stale = Vec::new();
    let mut compatibility_degraded_owned_sessions = Vec::new();
    let mut failed = Vec::new();
    let mut session_error_details = Vec::new();

    for target in close_all_session_targets(&snapshot) {
        if remaining_budget_ms(command_deadline) == 0 {
            record_close_all_budget_exhausted(
                target,
                &mut failed,
                &mut compatibility_degraded_owned_sessions,
            );
            continue;
        }
        let mut session_cleaned_stale = false;
        for entry in &target.stale_entries {
            cleanup_stale(rub_home, entry);
            let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
            session_cleaned_stale = true;
        }

        let Some(entry) = target.authority_entry.as_ref() else {
            if target.has_uncertain_entries {
                failed.push(target.session_name);
            } else if session_cleaned_stale {
                cleaned_stale.push(target.session_name);
            }
            continue;
        };

        let mut current_entry = entry.clone();
        let mut graceful_close = false;
        let mut authoritative_attach_confirmed = false;
        let mut attach_error = None;

        match detect_or_connect_hardened_until(
            rub_home,
            &target.session_name,
            TransientSocketPolicy::FailAfterLock,
            command_deadline,
            remaining_budget_ms(command_deadline).max(1),
        )
        .await
        {
            Ok(DaemonConnection::Connected {
                mut client,
                daemon_session_id,
                ..
            }) => {
                authoritative_attach_confirmed = true;
                match resolve_attached_close_all_authority(
                    rub_home,
                    &target.session_name,
                    daemon_session_id.as_deref(),
                )? {
                    Some(attached_entry) => {
                        current_entry = attached_entry;
                        let request = mutating_request(
                            "close",
                            serde_json::json!({}),
                            remaining_budget_ms(command_deadline).max(1),
                        );
                        match send_existing_request_with_replay_recovery(
                            &mut client,
                            &request,
                            command_deadline,
                            rub_home,
                            &target.session_name,
                            daemon_session_id.as_deref(),
                        )
                        .await
                        {
                            Ok(response)
                                if response.status
                                    == rub_ipc::protocol::ResponseStatus::Success =>
                            {
                                graceful_close = true;
                            }
                            Ok(response) => {
                                attach_error = Some(close_all_daemon_error_response_error(
                                    &target.session_name,
                                    response,
                                ));
                            }
                            Err(error) => attach_error = Some(error),
                        }
                    }
                    None => {
                        authoritative_attach_confirmed = false;
                        attach_error = Some(close_all_attached_authority_missing_error(
                            &target.session_name,
                            daemon_session_id.as_deref(),
                        ));
                    }
                }
            }
            Ok(DaemonConnection::NeedStart) => {}
            Err(error) => attach_error = Some(error),
        }

        let session_paths = RubPaths::new(rub_home)
            .session_runtime(&current_entry.session_name, &current_entry.session_id);
        let initial_shutdown = if requires_immediate_batch_shutdown_after_external_close(
            &current_entry,
            graceful_close,
        ) {
            ShutdownFenceStatus {
                daemon_stopped: false,
                profile_released: false,
            }
        } else {
            wait_for_shutdown_until(rub_home, &current_entry, command_deadline).await
        };
        let mut shutdown = initial_shutdown;
        let mut kill_fallback_used = false;
        let mut still_running = rub_core::process::is_process_alive(current_entry.pid)
            || !session_paths.existing_socket_paths().is_empty();

        if authoritative_attach_confirmed
            && should_escalate_close_all_to_kill_fallback(
                shutdown,
                still_running,
                remaining_budget_ms(command_deadline),
            )
            && terminate_registry_entry_process(rub_home, &current_entry).is_ok()
        {
            kill_fallback_used = true;
            shutdown = wait_for_shutdown_until(rub_home, &current_entry, command_deadline).await;
            still_running = rub_core::process::is_process_alive(current_entry.pid)
                || !session_paths.existing_socket_paths().is_empty();
        }

        if let Some(error) = attach_error.take() {
            session_error_details.push(BatchCloseSessionError {
                session: target.session_name.clone(),
                error: error.into_envelope(),
            });
            if let Some(compatibility_degraded_owned) = target.compatibility_degraded_owned {
                compatibility_degraded_owned_sessions.push(compatibility_degraded_owned);
            } else {
                failed.push(target.session_name);
            }
            continue;
        }

        let disposition =
            classify_close_all_result(graceful_close, kill_fallback_used, shutdown, still_running);
        let uncertain_siblings_remain = registry_authority_snapshot(rub_home)?
            .session(&target.session_name)
            .is_some_and(|session| session.has_uncertain_entries());

        if uncertain_siblings_remain {
            if matches!(
                disposition,
                CloseAllDisposition::Closed | CloseAllDisposition::CleanedStale
            ) {
                cleanup_stale(rub_home, &current_entry);
                let _ =
                    rub_daemon::session::deregister_session(rub_home, &current_entry.session_id);
            }
            failed.push(target.session_name);
            continue;
        }

        match disposition {
            CloseAllDisposition::Closed => {
                cleanup_stale(rub_home, &current_entry);
                let _ =
                    rub_daemon::session::deregister_session(rub_home, &current_entry.session_id);
                closed.push(target.session_name);
            }
            CloseAllDisposition::CleanedStale => {
                cleanup_stale(rub_home, &current_entry);
                let _ =
                    rub_daemon::session::deregister_session(rub_home, &current_entry.session_id);
                cleaned_stale.push(target.session_name);
            }
            CloseAllDisposition::Failed => {
                if let Some(compatibility_degraded_owned) = target.compatibility_degraded_owned {
                    compatibility_degraded_owned_sessions.push(compatibility_degraded_owned);
                } else {
                    failed.push(target.session_name);
                }
            }
        }
    }

    compatibility_degraded_owned_sessions.sort();
    compatibility_degraded_owned_sessions.dedup();

    Ok(BatchCloseResult {
        closed,
        cleaned_stale,
        compatibility_degraded_owned_sessions,
        failed,
        session_error_details,
    })
}

fn resolve_attached_close_all_authority(
    rub_home: &Path,
    session_name: &str,
    daemon_session_id: Option<&str>,
) -> Result<Option<rub_daemon::session::RegistryEntry>, RubError> {
    let Some(daemon_session_id) = daemon_session_id else {
        return Ok(None);
    };
    Ok(registry_authority_snapshot(rub_home)?
        .session(session_name)
        .and_then(|session| session.authoritative_entry())
        .filter(|entry| entry.entry.session_id == daemon_session_id)
        .map(|entry| entry.entry.clone()))
}

fn close_all_attached_authority_missing_error(
    session_name: &str,
    daemon_session_id: Option<&str>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::SessionBusy,
        format!(
            "Session '{session_name}' changed authority during batch close attachment; refusing to clean stale projections against an unverified entry"
        ),
        json!({
            "reason": "close_all_authority_shifted_after_attach",
            "session_name": session_name,
            "daemon_session_id": daemon_session_id,
        }),
    )
}

fn close_all_daemon_error_response_error(
    session_name: &str,
    response: rub_ipc::protocol::IpcResponse,
) -> RubError {
    let rub_ipc::protocol::IpcResponse {
        status,
        command_id,
        daemon_session_id,
        request_id,
        error,
        ..
    } = response;
    if let Some(error) = error {
        return RubError::Domain(error);
    }
    RubError::Domain(
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Daemon returned non-success status {status:?} for session '{session_name}' without an error envelope"
            ),
        )
        .with_context(json!({
            "reason": "close_all_daemon_error_response_missing_error_envelope",
            "session_name": session_name,
            "status": status,
            "request_id": request_id,
            "command_id": command_id,
            "daemon_session_id": daemon_session_id,
        })),
    )
}

pub(crate) fn should_escalate_close_all_to_kill_fallback(
    shutdown: ShutdownFenceStatus,
    still_running: bool,
    remaining_budget_ms: u64,
) -> bool {
    !shutdown.fully_released() && still_running && remaining_budget_ms > 0
}

pub(crate) fn requires_immediate_batch_shutdown_after_external_close(
    entry: &rub_daemon::session::RegistryEntry,
    graceful_close: bool,
) -> bool {
    graceful_close
        && matches!(
            entry.connection_target,
            Some(
                rub_core::model::ConnectionTarget::CdpUrl { .. }
                    | rub_core::model::ConnectionTarget::AutoDiscovered { .. }
            )
        )
}

pub(crate) fn close_all_session_targets(
    snapshot: &rub_daemon::session::RegistryAuthoritySnapshot,
) -> Vec<CloseAllSessionTarget> {
    snapshot
        .sessions
        .iter()
        .map(|session| CloseAllSessionTarget {
            session_name: session.session_name.clone(),
            authority_entry: session
                .authoritative_entry()
                .map(|entry| entry.entry.clone()),
            compatibility_degraded_owned: session
                .authoritative_entry()
                .and_then(compatibility_degraded_owned_from_snapshot),
            stale_entries: session.stale_entries(),
            has_uncertain_entries: session.has_uncertain_entries(),
        })
        .collect()
}

pub(crate) fn classify_close_all_result(
    graceful_close: bool,
    kill_fallback_used: bool,
    shutdown: ShutdownFenceStatus,
    still_running: bool,
) -> CloseAllDisposition {
    if shutdown.fully_released() {
        if graceful_close && !kill_fallback_used {
            CloseAllDisposition::Closed
        } else {
            CloseAllDisposition::CleanedStale
        }
    } else if graceful_close
        || kill_fallback_used
        || still_running
        || shutdown.lifecycle_committed()
    {
        CloseAllDisposition::Failed
    } else {
        CloseAllDisposition::CleanedStale
    }
}

pub(crate) fn record_close_all_budget_exhausted(
    target: CloseAllSessionTarget,
    failed: &mut Vec<String>,
    compatibility_degraded_owned_sessions: &mut Vec<CompatibilityDegradedOwnedSession>,
) {
    if let Some(compatibility_degraded_owned) = target.compatibility_degraded_owned {
        compatibility_degraded_owned_sessions.push(compatibility_degraded_owned);
    } else {
        failed.push(target.session_name);
    }
}

#[cfg(test)]
mod tests {
    use super::close_all_daemon_error_response_error;
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_ipc::protocol::IpcResponse;

    #[test]
    fn close_all_preserves_daemon_authoritative_error_response() {
        let response = IpcResponse::error(
            "req-close",
            ErrorEnvelope::new(ErrorCode::AutomationPaused, "automation is paused").with_context(
                serde_json::json!({
                    "reason": "automation_paused_for_handoff",
                    "daemon_truth": true,
                }),
            ),
        )
        .with_command_id("cmd-close")
        .expect("valid command id")
        .with_daemon_session_id("sess-close")
        .expect("valid daemon session id");

        let envelope = close_all_daemon_error_response_error("default", response).into_envelope();
        assert_eq!(envelope.code, ErrorCode::AutomationPaused);
        assert_eq!(
            envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&serde_json::json!("automation_paused_for_handoff"))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("daemon_truth")),
            Some(&serde_json::json!(true))
        );
    }
}
