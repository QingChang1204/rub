use std::path::Path;
use std::path::PathBuf;

use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::RubPaths;
use serde_json::json;

use super::process_identity::{
    process_matches_daemon_identity, process_matches_registry_entry_for_termination,
};
use super::process_lifecycle::force_kill_process;
use super::{DaemonCtlPathContext, daemon_ctl_path_error};

pub(crate) fn cleanup_stale(rub_home: &Path, entry: &rub_daemon::session::RegistryEntry) {
    let _ = cleanup_stale_checked(rub_home, entry);
}

pub(crate) fn cleanup_stale_checked(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> Result<(), RubError> {
    if rub_daemon::session::hard_cut_release_pending_blocks_entry(rub_home, entry) {
        return Ok(());
    }
    rub_daemon::session::cleanup_projections(rub_home, entry);
    let residues = stale_projection_residues(rub_home, entry);
    if residues.is_empty() {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::IoError,
        format!(
            "Failed to confirm stale projection cleanup for session '{}' ({})",
            entry.session_name, entry.session_id
        ),
        json!({
            "reason": "stale_projection_cleanup_fence_unconfirmed",
            "session_name": entry.session_name,
            "session_id": entry.session_id,
            "rub_home": rub_home.display().to_string(),
            "residual_projections": residues,
        }),
    ))
}

fn stale_projection_residues(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> Vec<serde_json::Value> {
    let paths = RubPaths::new(rub_home);
    let runtime = paths.session_runtime(&entry.session_name, &entry.session_id);
    let projection = paths.session(&entry.session_name);
    let runtime_socket_path = runtime.socket_path();
    let entry_socket_path = PathBuf::from(&entry.socket_path);
    let mut residues = Vec::new();

    if runtime.session_dir().exists() {
        residues.push(stale_projection_residue(
            "session_runtime_dir",
            &runtime.session_dir(),
            "session_runtime_directory_still_present",
        ));
    }

    if runtime_socket_path.exists() {
        residues.push(stale_projection_residue(
            "runtime_socket",
            &runtime_socket_path,
            "runtime_socket_still_present",
        ));
    }

    if entry_socket_path == runtime_socket_path && entry_socket_path.exists() {
        residues.push(stale_projection_residue(
            "registry_socket",
            &entry_socket_path,
            "registry_socket_still_present",
        ));
    }

    for actual_socket in [&runtime_socket_path, &entry_socket_path] {
        if projection_socket_points_to(&projection.canonical_socket_path(), actual_socket) {
            residues.push(stale_projection_residue(
                "canonical_socket",
                &projection.canonical_socket_path(),
                "canonical_socket_still_points_to_stale_entry",
            ));
        }
    }

    if projection_pid_matches(&projection.canonical_pid_path(), entry.pid) {
        residues.push(stale_projection_residue(
            "canonical_pid",
            &projection.canonical_pid_path(),
            "canonical_pid_still_points_to_stale_entry",
        ));
    }

    if file_contains_session_id(
        &projection.hard_cut_release_pending_path(),
        &entry.session_id,
    ) {
        residues.push(stale_projection_residue(
            "hard_cut_release_pending",
            &projection.hard_cut_release_pending_path(),
            "hard_cut_release_pending_still_points_to_stale_entry",
        ));
    }

    if file_contains_session_id(&projection.startup_committed_path(), &entry.session_id) {
        residues.push(stale_projection_residue(
            "startup_committed",
            &projection.startup_committed_path(),
            "startup_committed_still_points_to_stale_entry",
        ));
    }

    residues
}

fn stale_projection_residue(kind: &str, path: &Path, reason: &str) -> serde_json::Value {
    json!({
        "kind": kind,
        "path": path.display().to_string(),
        "reason": reason,
    })
}

fn projection_socket_points_to(path: &Path, actual_socket: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        metadata.file_type().is_symlink()
            && std::fs::read_link(path).ok().as_deref() == Some(actual_socket)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        let _ = actual_socket;
        false
    }
}

fn projection_pid_matches(path: &Path, pid: u32) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .is_some_and(|contents| contents.trim() == pid.to_string())
}

fn file_contains_session_id(path: &Path, session_id: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .is_some_and(|contents| contents.contains(session_id))
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
        .and_then(|session| {
            session
                .entries
                .iter()
                .rev()
                .find(|entry| entry.is_definitely_stale())
        })
        .map(|entry| entry.entry.clone()))
}

pub(crate) fn terminate_registry_entry_process(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<()> {
    if !process_matches_registry_entry_for_termination(rub_home, entry)? {
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

pub(crate) fn force_kill_registry_entry_process(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<()> {
    if !process_matches_daemon_identity(
        rub_home,
        &entry.session_name,
        Some(entry.session_id.as_str()),
        entry.pid,
    )? || !runtime_commit_matches_registry_entry(rub_home, entry)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Refused to force kill pid {} because it no longer matches daemon authority for session '{}' under {}",
                entry.pid,
                entry.session_name,
                rub_home.display()
            ),
        ));
    }
    force_kill_process(entry.pid)
}

fn runtime_commit_matches_registry_entry(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> bool {
    let runtime = RubPaths::new(rub_home).session_runtime(&entry.session_name, &entry.session_id);
    let pid_matches = std::fs::read_to_string(runtime.pid_path())
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        == Some(entry.pid);
    let committed_matches = std::fs::read_to_string(runtime.startup_committed_path())
        .ok()
        .is_some_and(|raw| raw.trim() == entry.session_id);
    let socket_matches = Path::new(&entry.socket_path) == runtime.socket_path();
    pid_matches && committed_matches && socket_matches
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
