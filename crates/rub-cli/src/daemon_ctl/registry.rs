use std::path::Path;

use rub_core::error::{ErrorCode, RubError};

use super::process_identity::{process_matches_daemon_identity, process_matches_registry_entry};
use super::{DaemonCtlPathContext, daemon_ctl_path_error};

pub(crate) fn cleanup_stale(rub_home: &Path, entry: &rub_daemon::session::RegistryEntry) {
    rub_daemon::session::cleanup_projections(rub_home, entry);
}

pub(crate) fn registry_entry_by_name(
    rub_home: &Path,
    session_name: &str,
) -> Result<Option<rub_daemon::session::RegistryEntry>, RubError> {
    Ok(registry_authority_snapshot(rub_home)?
        .session(session_name)
        .and_then(|session| {
            session
                .authoritative_entry()
                .map(|entry| entry.entry.clone())
        }))
}

pub(crate) fn latest_registry_entry_by_name(
    rub_home: &Path,
    session_name: &str,
) -> Result<Option<rub_daemon::session::RegistryEntry>, RubError> {
    Ok(registry_authority_snapshot(rub_home)?
        .session(session_name)
        .and_then(|session| session.latest_entry().map(|entry| entry.entry.clone())))
}

pub(crate) fn latest_definitely_stale_entry_by_name(
    rub_home: &Path,
    session_name: &str,
) -> Result<Option<rub_daemon::session::RegistryEntry>, RubError> {
    Ok(registry_authority_snapshot(rub_home)?
        .session(session_name)
        .and_then(|session| session.latest_entry())
        .filter(|entry| entry.is_definitely_stale())
        .map(|entry| entry.entry.clone()))
}

pub(crate) fn terminate_registry_entry_process(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<()> {
    if !process_matches_registry_entry(rub_home, entry)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Refused to kill pid {} because it no longer matches daemon authority for session '{}' under {}",
                entry.pid,
                entry.session_name,
                rub_home.display()
            ),
        ));
    }
    let result = unsafe { libc::kill(entry.pid as i32, libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn registry_authority_snapshot(
    rub_home: &Path,
) -> Result<rub_daemon::session::RegistryAuthoritySnapshot, RubError> {
    rub_daemon::session::registry_authority_snapshot(rub_home).map_err(|e| {
        daemon_ctl_path_error(
            ErrorCode::DaemonStartFailed,
            format!("Failed to resolve registry authority: {e}"),
            DaemonCtlPathContext {
                path_key: "rub_home",
                path: rub_home,
                path_authority: "daemon_ctl.registry_authority.rub_home",
                upstream_truth: "cli_rub_home",
                path_kind: "rub_home_directory",
                reason: "registry_authority_resolution_failed",
            },
        )
    })
}

pub(crate) fn process_matches_failed_startup_identity(
    rub_home: &Path,
    session_name: &str,
    session_id: &str,
    daemon_pid: u32,
) -> std::io::Result<bool> {
    process_matches_daemon_identity(rub_home, session_name, Some(session_id), daemon_pid)
}
