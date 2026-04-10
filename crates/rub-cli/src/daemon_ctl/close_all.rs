use crate::timeout_budget::helpers::mutating_request;
use rub_core::error::RubError;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    BatchCloseResult, ShutdownFenceStatus, cleanup_stale, registry_authority_snapshot,
    remaining_budget_ms, send_existing_request_with_replay_recovery,
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
    pub(crate) stale_entries: Vec<rub_daemon::session::RegistryEntry>,
    pub(crate) has_uncertain_entries: bool,
}

pub(crate) async fn close_all_sessions(
    rub_home: &Path,
    timeout: u64,
) -> Result<BatchCloseResult, RubError> {
    let command_deadline = Instant::now() + Duration::from_millis(timeout.max(1));
    if !rub_home.exists() {
        return Ok(BatchCloseResult {
            closed: Vec::new(),
            cleaned_stale: Vec::new(),
            failed: Vec::new(),
        });
    }

    let snapshot = registry_authority_snapshot(rub_home)?;
    if snapshot.sessions.is_empty() {
        return Ok(BatchCloseResult {
            closed: Vec::new(),
            cleaned_stale: Vec::new(),
            failed: Vec::new(),
        });
    }

    let mut closed = Vec::new();
    let mut cleaned_stale = Vec::new();
    let mut failed = Vec::new();

    for target in close_all_session_targets(&snapshot) {
        if remaining_budget_ms(command_deadline) == 0 {
            failed.push(target.session_name);
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

        let session_paths =
            RubPaths::new(rub_home).session_runtime(&entry.session_name, &entry.session_id);
        let mut graceful_close = false;

        if let Some(socket_path) = session_paths.existing_socket_paths().into_iter().next()
            && let Ok(mut client) = IpcClient::connect(&socket_path).await
        {
            let request = mutating_request(
                "close",
                serde_json::json!({}),
                remaining_budget_ms(command_deadline).max(1),
            );
            graceful_close = matches!(
                send_existing_request_with_replay_recovery(
                    &mut client,
                    &request,
                    command_deadline,
                    rub_home,
                    &target.session_name,
                    Some(entry.session_id.as_str()),
                )
                .await,
                Ok(response) if response.status == rub_ipc::protocol::ResponseStatus::Success
            );
        }

        let termination_requested = terminate_registry_entry_process(rub_home, entry).is_ok();
        let shutdown = wait_for_shutdown_until(rub_home, entry, command_deadline).await;

        let still_running = rub_core::process::is_process_alive(entry.pid)
            || !session_paths.existing_socket_paths().is_empty();
        match classify_close_all_result(
            graceful_close,
            termination_requested,
            shutdown,
            still_running,
        ) {
            CloseAllDisposition::Closed => {
                cleanup_stale(rub_home, entry);
                let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
                closed.push(target.session_name);
            }
            CloseAllDisposition::CleanedStale => {
                cleanup_stale(rub_home, entry);
                let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
                cleaned_stale.push(target.session_name);
            }
            CloseAllDisposition::Failed => {
                failed.push(target.session_name);
            }
        }
    }

    Ok(BatchCloseResult {
        closed,
        cleaned_stale,
        failed,
    })
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
            stale_entries: session.stale_entries(),
            has_uncertain_entries: session.has_uncertain_entries(),
        })
        .collect()
}

pub(crate) fn classify_close_all_result(
    graceful_close: bool,
    termination_requested: bool,
    shutdown: ShutdownFenceStatus,
    still_running: bool,
) -> CloseAllDisposition {
    if shutdown.fully_released() {
        if graceful_close {
            CloseAllDisposition::Closed
        } else {
            CloseAllDisposition::CleanedStale
        }
    } else if graceful_close
        || termination_requested
        || still_running
        || shutdown.lifecycle_committed()
    {
        CloseAllDisposition::Failed
    } else {
        CloseAllDisposition::CleanedStale
    }
}
