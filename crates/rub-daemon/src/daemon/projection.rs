use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::session::{
    RegistryEntry, SessionState, cleanup_projections, load_registry_for_home,
    registry_entry_has_runtime_authority_for_home, registry_entry_is_live_for_home,
    registry_entry_is_pending_startup_for_home, store_registry_for_home,
    validate_registry_entry_for_home, with_registry_lock,
};
use rub_core::fs::{FileCommitOutcome, atomic_write_bytes, sync_parent_dir};

pub(super) fn signal_ready() -> std::io::Result<()> {
    if let Some(path) = std::env::var_os("RUB_DAEMON_READY_FILE") {
        atomic_write_durable_bytes(Path::new(&path), b"ready", 0o600)?;
    }
    Ok(())
}

pub(super) enum RestorePreviousAuthorityOutcome {
    Restored,
    SkippedNotLive,
    SkippedSuperseded,
}

pub(super) fn restore_previous_authority_if_live(
    home: &Path,
    failed_entry: &RegistryEntry,
    entry: &RegistryEntry,
) -> std::io::Result<RestorePreviousAuthorityOutcome> {
    with_registry_lock(home, true, |path| {
        let mut data = load_registry_for_home(home, path)?;
        validate_registry_entry_for_home(home, entry)?;

        let superseded_by_other_candidate = data
            .sessions
            .iter()
            .filter(|existing| existing.session_name == entry.session_name)
            .filter(|existing| existing.session_id != failed_entry.session_id)
            .filter(|existing| existing.session_id != entry.session_id)
            .any(|candidate| {
                registry_entry_is_live_for_home(home, candidate)
                    || registry_entry_is_pending_startup_for_home(home, candidate)
            });
        if superseded_by_other_candidate {
            return Ok(RestorePreviousAuthorityOutcome::SkippedSuperseded);
        }

        if !registry_entry_has_runtime_authority_for_home(home, entry) {
            data.sessions
                .retain(|existing| existing.session_id != entry.session_id);
            store_registry_for_home(home, path, &data)?;
            cleanup_projections(home, entry);
            return Ok(RestorePreviousAuthorityOutcome::SkippedNotLive);
        }

        if !data
            .sessions
            .iter()
            .any(|existing| existing.session_id == entry.session_id)
        {
            data.sessions.push(entry.clone());
            store_registry_for_home(home, path, &data)?;
        }

        clear_startup_commit_marker_if_matches(home, &entry.session_name, &entry.session_id);
        restore_socket_projection(home, entry)?;
        restore_pid_projection(home, entry)?;
        restore_startup_commit_marker(home, entry)?;

        if !registry_entry_has_runtime_authority_for_home(home, entry) {
            data.sessions
                .retain(|existing| existing.session_id != entry.session_id);
            store_registry_for_home(home, path, &data)?;
            cleanup_projections(home, entry);
            return Ok(RestorePreviousAuthorityOutcome::SkippedNotLive);
        }

        Ok(RestorePreviousAuthorityOutcome::Restored)
    })
}

pub(super) fn startup_ready_marker_path() -> Option<PathBuf> {
    std::env::var_os("RUB_DAEMON_READY_FILE").map(PathBuf::from)
}

pub(super) fn publish_pid_projection(state: &SessionState, pid: u32) -> std::io::Result<()> {
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

pub(super) fn publish_startup_commit_marker(state: &SessionState) -> std::io::Result<()> {
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

fn clear_startup_commit_marker_if_matches(home: &Path, session_name: &str, session_id: &str) {
    let path = crate::rub_paths::RubPaths::new(home)
        .session(session_name)
        .startup_committed_path();
    let matches_entry = std::fs::read_to_string(&path)
        .ok()
        .is_some_and(|current| current.trim() == session_id);
    if matches_entry {
        let _ = std::fs::remove_file(path);
    }
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

pub(super) fn publish_socket_projection(state: &SessionState) -> std::io::Result<()> {
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
