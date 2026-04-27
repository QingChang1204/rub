use super::CleanupResult;
use super::projection::{CleanupPathContext, cleanup_path_error};
use super::upgrade_probe::fetch_upgrade_status_for_session_with_deadline;
use crate::daemon_ctl::compatibility_degraded_owned_from_snapshot;
use std::path::Path;
use std::time::Instant;

use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::RubPaths;

pub(super) async fn cleanup_current_home_stale(
    rub_home: &Path,
    deadline: Instant,
    timeout_ms: u64,
    result: &mut CleanupResult,
) -> Result<(), RubError> {
    let snapshot = match rub_daemon::session::registry_authority_snapshot(rub_home) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(cleanup_path_error(
                ErrorCode::DaemonStartFailed,
                format!("Failed to read registry for cleanup: {error}"),
                CleanupPathContext {
                    path_key: "rub_home",
                    path: rub_home,
                    path_authority: "cli.cleanup.subject.rub_home",
                    upstream_truth: "cli_rub_home",
                    path_kind: "cleanup_home_directory",
                    reason: "cleanup_registry_read_failed",
                },
            ));
        }
    };

    for session in snapshot.sessions {
        for entry_snapshot in session.entries {
            if let Some(compatibility_degraded_owned) =
                compatibility_degraded_owned_from_snapshot(&entry_snapshot)
            {
                result
                    .compatibility_degraded_owned_sessions
                    .push(compatibility_degraded_owned);
                continue;
            }
            let pending_startup = entry_snapshot.is_pending_startup();
            let live_authority = entry_snapshot.is_live_authority();
            let entry = entry_snapshot.entry;
            let session_name = entry.session_name.clone();
            let session_paths =
                RubPaths::new(rub_home).session_runtime(&session_name, &entry.session_id);
            match fetch_upgrade_status_for_session_with_deadline(
                &session_paths,
                deadline,
                timeout_ms,
                "cleanup_current_home_upgrade_check",
            )
            .await
            {
                Ok(Some(_)) => {
                    result.kept_active_sessions.push(session_name);
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    if matches!(&error, RubError::Domain(envelope) if envelope.code == ErrorCode::IpcTimeout)
                    {
                        return Err(error);
                    }
                    if pending_startup || live_authority {
                        result.skipped_unreachable_sessions.push(session_name);
                        continue;
                    }
                }
            }

            if pending_startup || live_authority {
                result.skipped_unreachable_sessions.push(session_name);
                continue;
            }

            rub_daemon::session::cleanup_projections(rub_home, &entry);
            let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
            result.cleaned_stale_sessions.push(entry.session_name);
        }
    }

    Ok(())
}
