use crate::rub_paths::RubPaths;
use rub_core::model::ConnectionTarget;
use serde::{Deserialize, Serialize};
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

pub use self::liveness::{
    registry_entry_is_live_for_home, registry_entry_is_pending_startup_for_home,
};
pub use self::mutation::{
    check_profile_in_use, deregister_session, promote_session_authority, register_pending_session,
    register_session, register_session_with_displaced,
};
pub use self::projection_cleanup::cleanup_projections;
pub use self::snapshot::{
    RegistryAuthoritySnapshot, RegistryEntryLiveness, RegistryEntrySnapshot,
    RegistrySessionSnapshot, active_registry_entries, authoritative_entry_by_session_name,
    latest_entry_by_session_name, registry_authority_snapshot,
};

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
