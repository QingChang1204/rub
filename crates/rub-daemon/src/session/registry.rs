use crate::rub_paths::RubPaths;
use rub_core::model::ConnectionTarget;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::cell::Cell;
use std::fs;
use std::path::Path;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

mod liveness;
mod lock;
mod mutation;
mod projection_cleanup;
mod snapshot;
mod storage;
mod validation;

pub(crate) use self::liveness::registry_entry_has_runtime_authority_for_home;
#[cfg(test)]
pub(crate) use self::liveness::{
    force_busy_registry_socket_probe_once_for_test, force_dead_registry_socket_probe_once_for_test,
    force_live_registry_socket_probe_once_for_test,
    force_probe_contract_failure_registry_socket_probe_once_for_test,
    force_protocol_incompatible_registry_socket_probe_once_for_test,
};
pub use self::liveness::{
    registry_entry_is_live_for_home, registry_entry_is_pending_startup_for_home,
};
pub use self::mutation::{
    check_profile_in_use, deregister_session, promote_session_authority, register_pending_session,
    register_session, register_session_with_displaced,
};
pub use self::projection_cleanup::cleanup_projections;
pub(crate) use self::snapshot::registry_authority_snapshot_async;
pub use self::snapshot::{
    RegistryAuthoritySnapshot, RegistryEntryLiveness, RegistryEntrySnapshot,
    RegistrySessionSnapshot, active_registry_entries, active_registry_entry_snapshots,
    authoritative_entry_by_session_name, latest_entry_by_session_name, registry_authority_snapshot,
};
pub(crate) use self::storage::{
    load_registry_for_home, store_registry_for_home, with_registry_lock,
};
pub(crate) use self::validation::validate_registry_entry_for_home;

pub fn ensure_rub_home(home: &Path) -> std::io::Result<()> {
    storage::ensure_rub_home(home)
}

pub fn read_registry(home: &Path) -> std::io::Result<RegistryData> {
    storage::read_registry(home)
}

pub fn write_registry(home: &Path, data: &RegistryData) -> std::io::Result<()> {
    storage::write_registry(home, data)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub session_id: String,
    pub session_name: String,
    pub pid: u32,
    pub socket_path: String,
    pub created_at: String,
    pub ipc_protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_target: Option<ConnectionTarget>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryData {
    pub sessions: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardCutReleasePendingProof {
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HardCutReleasePendingAuthority {
    Absent,
    Present,
}

#[cfg(test)]
thread_local! {
    static FORCE_HARD_CUT_RELEASE_PENDING_PROFILE_OBSERVATION_FAILURE: Cell<bool> =
        const { Cell::new(false) };
}

pub fn write_hard_cut_release_pending_proof(
    home: &Path,
    session_name: &str,
    proof: &HardCutReleasePendingProof,
) -> std::io::Result<rub_core::fs::FileCommitOutcome> {
    let path = RubPaths::new(home)
        .session(session_name)
        .hard_cut_release_pending_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(proof)
        .map_err(|error| std::io::Error::other(format!("serialize hard-cut proof: {error}")))?;
    rub_core::fs::atomic_write_bytes(&path, &bytes, 0o600)
}

pub fn read_hard_cut_release_pending_proof(
    home: &Path,
    session_name: &str,
) -> std::io::Result<Option<HardCutReleasePendingProof>> {
    let path = RubPaths::new(home)
        .session(session_name)
        .hard_cut_release_pending_path();
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let proof = serde_json::from_slice::<HardCutReleasePendingProof>(&raw).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("parse hard-cut proof {}: {error}", path.display()),
        )
    })?;
    Ok(Some(proof))
}

pub fn clear_hard_cut_release_pending_proof(
    home: &Path,
    session_name: &str,
) -> std::io::Result<()> {
    let path = RubPaths::new(home)
        .session(session_name)
        .hard_cut_release_pending_path();
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn hard_cut_release_pending_blocks_entry(home: &Path, entry: &RegistryEntry) -> bool {
    matches!(
        hard_cut_release_pending_authority(home, entry),
        HardCutReleasePendingAuthority::Present
    )
}

fn hard_cut_release_pending_authority(
    home: &Path,
    entry: &RegistryEntry,
) -> HardCutReleasePendingAuthority {
    let Some(user_data_dir) = entry.user_data_dir.as_deref() else {
        return HardCutReleasePendingAuthority::Absent;
    };
    let path = RubPaths::new(home)
        .session(&entry.session_name)
        .hard_cut_release_pending_path();
    if !path.exists() {
        return HardCutReleasePendingAuthority::Absent;
    }
    let profile_still_held = match hard_cut_release_pending_profile_still_held(user_data_dir) {
        Ok(profile_still_held) => profile_still_held,
        Err(_) => return HardCutReleasePendingAuthority::Present,
    };
    if !profile_still_held {
        return HardCutReleasePendingAuthority::Absent;
    }
    match read_hard_cut_release_pending_proof(home, &entry.session_name) {
        Ok(Some(proof)) if proof.session_id == entry.session_id => {
            HardCutReleasePendingAuthority::Present
        }
        Ok(Some(_)) | Ok(None) => HardCutReleasePendingAuthority::Absent,
        Err(_) => HardCutReleasePendingAuthority::Present,
    }
}

fn hard_cut_release_pending_profile_still_held(
    user_data_dir: &str,
) -> Result<bool, rub_core::error::RubError> {
    #[cfg(test)]
    if FORCE_HARD_CUT_RELEASE_PENDING_PROFILE_OBSERVATION_FAILURE.with(|force| force.replace(false))
    {
        return Err(rub_core::error::RubError::domain(
            rub_core::error::ErrorCode::BrowserLaunchFailed,
            "forced hard-cut release-pending profile observation failure",
        ));
    }
    rub_cdp::managed_profile_in_use(Path::new(user_data_dir))
}

#[cfg(test)]
pub(crate) fn force_hard_cut_release_pending_profile_observation_failure_for_test() {
    FORCE_HARD_CUT_RELEASE_PENDING_PROFILE_OBSERVATION_FAILURE.with(|force| force.set(true));
}

pub fn rfc3339_now() -> String {
    // Rfc3339 formatting of OffsetDateTime::now_utc() is infallible in
    // practice. Sentinel is non-epoch to make format failures visible
    // rather than silently injecting a valid-looking "1970" timestamp.
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "TIMESTAMP_FORMAT_ERROR".to_string())
}

pub fn new_session_id() -> String {
    Uuid::now_v7().to_string()
}

#[cfg(test)]
fn is_matching_rub_daemon_command(command: &str, home: &Path, session_name: &str) -> bool {
    if !command.contains("__daemon") {
        return false;
    }
    extract_flag_value(command, "--session").as_deref() == Some(session_name)
        && extract_flag_value(command, "--rub-home")
            .map(std::path::PathBuf::from)
            .is_some_and(|path| path == home)
}

#[cfg(test)]
fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
    let parts = tokenize_command(command);
    let mut iter = parts.iter();
    while let Some(part) = iter.next() {
        if part == flag {
            return iter.next().cloned();
        }
        if let Some(value) = part.strip_prefix(&format!("{flag}=")) {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
fn tokenize_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single_quotes = false;
    let mut in_double_quotes = false;
    let mut escaping = false;

    for ch in command.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }

        match ch {
            '\\' if !in_single_quotes => escaping = true,
            '\'' if !in_double_quotes => in_single_quotes = !in_single_quotes,
            '"' if !in_single_quotes => in_double_quotes = !in_double_quotes,
            ch if ch.is_whitespace() && !in_single_quotes && !in_double_quotes => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaping {
        current.push('\\');
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests;
