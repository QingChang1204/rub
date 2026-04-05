use crate::rub_paths::{RubPaths, validate_session_id_component, validate_session_name};
use rub_core::model::ConnectionTarget;
use rub_core::process::is_process_alive;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufReader, Read, Write};
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryEntryLiveness {
    Live,
    BusyOrUnknown,
    PendingStartup,
    Dead,
}

#[derive(Debug, Clone)]
pub struct RegistryEntrySnapshot {
    pub entry: RegistryEntry,
    pub liveness: RegistryEntryLiveness,
    pub pid_live: bool,
}

impl RegistryEntrySnapshot {
    pub fn is_live_authority(&self) -> bool {
        matches!(
            self.liveness,
            RegistryEntryLiveness::Live | RegistryEntryLiveness::BusyOrUnknown
        )
    }

    pub fn is_pending_startup(&self) -> bool {
        self.liveness == RegistryEntryLiveness::PendingStartup
    }

    pub fn is_definitely_stale(&self) -> bool {
        self.liveness == RegistryEntryLiveness::Dead && !self.pid_live
    }

    pub fn is_uncertain(&self) -> bool {
        self.liveness == RegistryEntryLiveness::Dead && self.pid_live
    }
}

#[derive(Debug, Clone)]
pub struct RegistrySessionSnapshot {
    pub session_name: String,
    pub entries: Vec<RegistryEntrySnapshot>,
}

impl RegistrySessionSnapshot {
    pub fn authoritative_entry(&self) -> Option<&RegistryEntrySnapshot> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.is_live_authority())
    }

    pub fn latest_entry(&self) -> Option<&RegistryEntrySnapshot> {
        self.entries.last()
    }

    pub fn stale_entries(&self) -> Vec<RegistryEntry> {
        let authoritative_session_id = self
            .authoritative_entry()
            .map(|entry| entry.entry.session_id.as_str());
        self.entries
            .iter()
            .filter(|entry| authoritative_session_id != Some(entry.entry.session_id.as_str()))
            .filter(|entry| entry.is_definitely_stale())
            .map(|entry| entry.entry.clone())
            .collect()
    }

    pub fn has_uncertain_entries(&self) -> bool {
        let authoritative_session_id = self
            .authoritative_entry()
            .map(|entry| entry.entry.session_id.as_str());
        self.entries
            .iter()
            .filter(|entry| authoritative_session_id != Some(entry.entry.session_id.as_str()))
            .any(RegistryEntrySnapshot::is_uncertain)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegistryAuthoritySnapshot {
    pub sessions: Vec<RegistrySessionSnapshot>,
}

impl RegistryAuthoritySnapshot {
    pub fn session(&self, session_name: &str) -> Option<&RegistrySessionSnapshot> {
        self.sessions
            .iter()
            .find(|session| session.session_name == session_name)
    }

    pub fn active_entries(&self) -> Vec<RegistryEntry> {
        let mut entries = self
            .sessions
            .iter()
            .filter_map(|session| {
                session
                    .authoritative_entry()
                    .map(|entry| entry.entry.clone())
            })
            .collect::<Vec<_>>();
        entries.sort_by(compare_registry_entry_created_at);
        entries
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrySocketProbe {
    Live,
    BusyOrUnknown,
    Dead,
}

#[derive(Debug, serde::Deserialize)]
struct RegistryHandshakePayload {
    daemon_session_id: String,
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

pub fn ensure_rub_home(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)
}

pub fn read_registry(home: &Path) -> std::io::Result<RegistryData> {
    with_registry_lock(home, false, |path| load_registry_for_home(home, path))
}

pub fn write_registry(home: &Path, data: &RegistryData) -> std::io::Result<()> {
    with_registry_lock(home, true, |path| store_registry_for_home(home, path, data))
}

pub fn register_session(home: &Path, entry: RegistryEntry) -> std::io::Result<()> {
    register_session_with_displaced(home, entry).map(|_| ())
}

pub fn register_pending_session(home: &Path, entry: RegistryEntry) -> std::io::Result<()> {
    with_registry_lock(home, true, |path| {
        let mut data = load_registry_for_home(home, path)?;
        validate_registry_entry_for_home(home, &entry)?;
        data.sessions
            .retain(|existing| existing.session_id != entry.session_id);
        data.sessions.push(entry);
        store_registry_for_home(home, path, &data)
    })
}

pub fn register_session_with_displaced(
    home: &Path,
    entry: RegistryEntry,
) -> std::io::Result<Option<RegistryEntry>> {
    with_registry_lock(home, true, |path| {
        let mut data = load_registry_for_home(home, path)?;
        let entry = entry;
        validate_registry_entry_for_home(home, &entry)?;
        let displaced = data
            .sessions
            .iter()
            .filter(|existing| {
                existing.session_id != entry.session_id
                    && existing.session_name == entry.session_name
            })
            .cloned()
            .max_by(compare_registry_entry_created_at);
        data.sessions.retain(|existing| {
            existing.session_id != entry.session_id && existing.session_name != entry.session_name
        });
        data.sessions.push(entry);
        store_registry_for_home(home, path, &data)?;
        Ok(displaced)
    })
}

pub fn promote_session_authority(
    home: &Path,
    session_name: &str,
    session_id: &str,
) -> std::io::Result<()> {
    with_registry_lock(home, true, |path| {
        let mut data = load_registry_for_home(home, path)?;
        data.sessions.retain(|existing| {
            existing.session_id == session_id || existing.session_name != session_name
        });
        store_registry_for_home(home, path, &data)
    })
}

pub fn deregister_session(home: &Path, session_id: &str) -> std::io::Result<()> {
    with_registry_lock(home, true, |path| {
        let mut data = load_registry_for_home(home, path)?;
        data.sessions.retain(|e| e.session_id != session_id);
        store_registry_for_home(home, path, &data)
    })
}

pub fn check_profile_in_use(
    home: &Path,
    attachment_identity: &str,
    exclude_session_id: Option<&str>,
) -> std::io::Result<Option<String>> {
    let data = read_registry(home)?;
    for entry in &data.sessions {
        if exclude_session_id.is_some_and(|session_id| entry.session_id == session_id) {
            continue;
        }
        if entry.attachment_identity.as_deref() == Some(attachment_identity)
            && registry_entry_is_live_for_home(home, entry)
        {
            return Ok(Some(entry.session_name.clone()));
        }
    }
    Ok(None)
}

pub fn cleanup_projections(home: &Path, entry: &RegistryEntry) {
    let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
    let projection = RubPaths::new(home).session(&entry.session_name);

    for path in [
        runtime.socket_path(),
        runtime.pid_path(),
        runtime.lock_path(),
    ] {
        let _ = std::fs::remove_file(path);
    }

    if let Ok(entries) = std::fs::read_dir(runtime.session_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("startup.")
                && (name.ends_with(".ready") || name.ends_with(".error"))
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let _ = std::fs::remove_dir(runtime.session_dir());

    cleanup_socket_projection_if_matches(
        &projection.canonical_socket_path(),
        &runtime.socket_path(),
    );
    cleanup_pid_projection_if_matches(&projection.canonical_pid_path(), entry.pid);
    cleanup_startup_commit_marker_if_matches(
        &projection.startup_committed_path(),
        &entry.session_id,
    );

    if projection.projection_dir() != runtime.session_dir() {
        let _ = std::fs::remove_dir(projection.projection_dir());
    }
}

pub fn authoritative_entry_by_session_name(
    home: &Path,
    session_name: &str,
) -> std::io::Result<Option<RegistryEntry>> {
    Ok(registry_authority_snapshot(home)?
        .session(session_name)
        .and_then(|session| {
            session
                .authoritative_entry()
                .map(|entry| entry.entry.clone())
        }))
}

pub fn latest_entry_by_session_name(
    home: &Path,
    session_name: &str,
) -> std::io::Result<Option<RegistryEntry>> {
    Ok(registry_authority_snapshot(home)?
        .session(session_name)
        .and_then(|session| session.latest_entry().map(|entry| entry.entry.clone())))
}

pub fn active_registry_entries(home: &Path) -> std::io::Result<Vec<RegistryEntry>> {
    Ok(registry_authority_snapshot(home)?.active_entries())
}

pub fn registry_authority_snapshot(home: &Path) -> std::io::Result<RegistryAuthoritySnapshot> {
    let data = read_registry(home)?;
    Ok(build_registry_authority_snapshot(home, &data))
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

fn registry_path(home: &Path) -> PathBuf {
    RubPaths::new(home).registry_path()
}

fn registry_lock_path(home: &Path) -> PathBuf {
    RubPaths::new(home).registry_lock_path()
}

fn with_registry_lock<T>(
    home: &Path,
    exclusive: bool,
    f: impl FnOnce(&Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    ensure_rub_home(home)?;
    let registry_path = registry_path(home);
    let lock_path = registry_lock_path(home);
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;

    flock(&lock_file, exclusive)?;
    let result = f(&registry_path);
    let unlock_result = unlock(&lock_file);

    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(err),
        (Err(err), Err(_)) => Err(err),
    }
}

fn load_registry_for_home(home: &Path, path: &Path) -> std::io::Result<RegistryData> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if contents.trim().is_empty() {
        return Ok(RegistryData::default());
    }

    let data = serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    validate_registry_data_for_home(home, &data)?;
    Ok(data)
}

fn store_registry_for_home(home: &Path, path: &Path, data: &RegistryData) -> std::io::Result<()> {
    validate_registry_data_for_home(home, data)?;
    let json = serde_json::to_string_pretty(&data).map_err(std::io::Error::other)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(format!(".registry.{}.tmp", Uuid::now_v7()));
    {
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        temp.write_all(json.as_bytes())?;
        temp.sync_all()?;
    }
    std::fs::rename(&temp_path, path)?;
    if let Ok(parent_dir) = std::fs::File::open(parent) {
        let _ = parent_dir.sync_all();
    }
    Ok(())
}

fn build_registry_authority_snapshot(
    home: &Path,
    data: &RegistryData,
) -> RegistryAuthoritySnapshot {
    let mut sessions = BTreeMap::<String, Vec<RegistryEntrySnapshot>>::new();
    for entry in &data.sessions {
        let snapshot = registry_entry_snapshot_for_home(home, entry);
        sessions
            .entry(snapshot.entry.session_name.clone())
            .or_default()
            .push(snapshot);
    }

    let sessions = sessions
        .into_iter()
        .map(|(session_name, mut entries)| {
            entries.sort_by(compare_registry_entry_snapshot_created_at);
            RegistrySessionSnapshot {
                session_name,
                entries,
            }
        })
        .collect();
    RegistryAuthoritySnapshot { sessions }
}

fn compare_registry_entry_created_at(left: &RegistryEntry, right: &RegistryEntry) -> Ordering {
    parsed_registry_created_at(&left.created_at)
        .expect("registry created_at should be validated before ordering")
        .cmp(
            &parsed_registry_created_at(&right.created_at)
                .expect("registry created_at should be validated before ordering"),
        )
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn compare_registry_entry_snapshot_created_at(
    left: &RegistryEntrySnapshot,
    right: &RegistryEntrySnapshot,
) -> Ordering {
    compare_registry_entry_created_at(&left.entry, &right.entry)
}

fn parsed_registry_created_at(created_at: &str) -> std::io::Result<OffsetDateTime> {
    OffsetDateTime::parse(created_at, &Rfc3339).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid canonical RFC3339 timestamp '{created_at}': {error}"),
        )
    })
}

pub fn registry_entry_is_live_for_home(home: &Path, entry: &RegistryEntry) -> bool {
    registry_entry_snapshot_for_home(home, entry).is_live_authority()
}

pub fn registry_entry_is_pending_startup_for_home(home: &Path, entry: &RegistryEntry) -> bool {
    registry_entry_snapshot_for_home(home, entry).is_pending_startup()
}

fn registry_entry_snapshot_for_home(home: &Path, entry: &RegistryEntry) -> RegistryEntrySnapshot {
    let pid_live = is_process_alive(entry.pid);
    RegistryEntrySnapshot {
        entry: entry.clone(),
        liveness: registry_entry_liveness_for_home(home, entry, pid_live),
        pid_live,
    }
}

fn registry_entry_liveness_for_home(
    home: &Path,
    entry: &RegistryEntry,
    pid_live: bool,
) -> RegistryEntryLiveness {
    let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
    let socket_live = Path::new(&entry.socket_path).exists();
    let committed = std::fs::read_to_string(runtime.startup_committed_path())
        .ok()
        .is_some_and(|session_id| session_id == entry.session_id);
    let runtime_pid_live = runtime.pid_path().exists();
    if !committed && socket_live && pid_live && runtime_pid_live {
        return RegistryEntryLiveness::PendingStartup;
    }
    if !(committed && socket_live && pid_live && runtime_pid_live) {
        return RegistryEntryLiveness::Dead;
    }

    match registry_socket_probe(entry) {
        RegistrySocketProbe::Live => RegistryEntryLiveness::Live,
        RegistrySocketProbe::BusyOrUnknown => RegistryEntryLiveness::BusyOrUnknown,
        RegistrySocketProbe::Dead => RegistryEntryLiveness::Dead,
    }
}

#[cfg(unix)]
fn registry_socket_probe(entry: &RegistryEntry) -> RegistrySocketProbe {
    let Ok(mut stream) = UnixStream::connect(&entry.socket_path) else {
        return RegistrySocketProbe::Dead;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));

    let request = IpcRequest::new("_handshake", serde_json::json!({}), 750);
    let Ok(encoded) = NdJsonCodec::encode(&request) else {
        return RegistrySocketProbe::Dead;
    };
    if let Err(error) = stream.write_all(&encoded) {
        return if socket_probe_timeout(&error) {
            RegistrySocketProbe::BusyOrUnknown
        } else {
            RegistrySocketProbe::Dead
        };
    }

    let mut reader = BufReader::new(stream);
    let response = match NdJsonCodec::read_blocking::<IpcResponse, _>(&mut reader) {
        Ok(Some(response)) => response,
        Ok(None) => return RegistrySocketProbe::Dead,
        Err(error) => {
            let timeout = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(socket_probe_timeout);
            return if timeout {
                RegistrySocketProbe::BusyOrUnknown
            } else {
                RegistrySocketProbe::Dead
            };
        }
    };
    if response.ipc_protocol_version != IPC_PROTOCOL_VERSION {
        return RegistrySocketProbe::Dead;
    }
    if response.status != rub_ipc::protocol::ResponseStatus::Success {
        return RegistrySocketProbe::Dead;
    }
    let Ok(payload) =
        response.data.clone().ok_or(()).and_then(|data| {
            serde_json::from_value::<RegistryHandshakePayload>(data).map_err(|_| ())
        })
    else {
        return RegistrySocketProbe::Dead;
    };
    if payload.daemon_session_id != entry.session_id {
        return RegistrySocketProbe::Dead;
    };
    RegistrySocketProbe::Live
}

#[cfg(not(unix))]
fn registry_socket_probe(_entry: &RegistryEntry) -> RegistrySocketProbe {
    RegistrySocketProbe::Dead
}

fn socket_probe_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn cleanup_startup_commit_marker_if_matches(path: &Path, session_id: &str) {
    let matches_entry = std::fs::read_to_string(path)
        .ok()
        .is_some_and(|current| current.trim() == session_id);
    if matches_entry {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
fn is_matching_rub_daemon_command(command: &str, home: &Path, session_name: &str) -> bool {
    if !command.contains("__daemon") {
        return false;
    }
    extract_flag_value(command, "--session").as_deref() == Some(session_name)
        && extract_flag_value(command, "--rub-home")
            .map(PathBuf::from)
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

fn validate_registry_data_for_home(home: &Path, data: &RegistryData) -> std::io::Result<()> {
    for entry in &data.sessions {
        validate_registry_entry_for_home(home, entry)?;
    }
    Ok(())
}

fn validate_registry_entry_for_home(home: &Path, entry: &RegistryEntry) -> std::io::Result<()> {
    if entry.session_id.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' is missing canonical session_id",
                entry.session_name
            ),
        ));
    }
    validate_session_id_component(&entry.session_id).map_err(|reason| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' has invalid canonical session_id '{}': {reason}",
                entry.session_name, entry.session_id
            ),
        )
    })?;
    if entry.session_name.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session_id '{}' is missing canonical session_name",
                entry.session_id
            ),
        ));
    }
    validate_session_name(&entry.session_name).map_err(|reason| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session_id '{}' has invalid canonical session_name '{}': {reason}",
                entry.session_id, entry.session_name
            ),
        )
    })?;
    let parsed_created_at = parsed_registry_created_at(&entry.created_at).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' has invalid created_at '{}': {error}",
                entry.session_name, entry.created_at
            ),
        )
    })?;
    let canonical_created_at = parsed_created_at.format(&Rfc3339).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' could not normalize created_at '{}': {error}",
                entry.session_name, entry.created_at
            ),
        )
    })?;
    if canonical_created_at != entry.created_at {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' has non-canonical created_at '{}'; expected '{}'",
                entry.session_name, entry.created_at, canonical_created_at
            ),
        ));
    }
    if entry.pid == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' has invalid pid 0",
                entry.session_name
            ),
        ));
    }
    let expected_socket_path = RubPaths::new(home)
        .session_runtime(&entry.session_name, &entry.session_id)
        .socket_path();
    if Path::new(&entry.socket_path) != expected_socket_path {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' has non-canonical socket_path '{}'; expected '{}'",
                entry.session_name,
                entry.socket_path,
                expected_socket_path.display()
            ),
        ));
    }
    let protocol = entry.ipc_protocol_version.trim();
    if protocol.is_empty()
        || protocol != entry.ipc_protocol_version
        || protocol
            .split('.')
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Registry entry for session '{}' has invalid canonical ipc_protocol_version '{}'",
                entry.session_name, entry.ipc_protocol_version
            ),
        ));
    }
    Ok(())
}

fn flock(file: &std::fs::File, exclusive: bool) -> std::io::Result<()> {
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };

    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn cleanup_socket_projection_if_matches(path: &Path, actual_socket: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    #[cfg(unix)]
    {
        if metadata.file_type().is_symlink()
            && std::fs::read_link(path).ok().as_deref() == Some(actual_socket)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn cleanup_pid_projection_if_matches(path: &Path, pid: u32) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    if contents.trim() == pid.to_string() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RegistryData, RegistryEntry, authoritative_entry_by_session_name, cleanup_projections,
        is_matching_rub_daemon_command, latest_entry_by_session_name, new_session_id,
        promote_session_authority, read_registry, register_pending_session, register_session,
        registry_authority_snapshot, registry_entry_is_live_for_home,
        registry_entry_is_pending_startup_for_home, write_registry,
    };
    use crate::rub_paths::RubPaths;
    use rub_ipc::codec::NdJsonCodec;
    use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};
    use std::io::{BufRead, BufReader, Write};
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    use std::time::Duration;
    use uuid::Uuid;

    fn temp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rub-registry-test-{}", Uuid::now_v7()))
    }

    #[test]
    fn read_registry_rejects_entries_missing_canonical_session_id() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_name": "default",
                    "pid": 1234,
                    "socket_path": runtime.socket_path(),
                    "created_at": "2026-03-31T00:00:00Z",
                    "ipc_protocol_version": "1.0",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("noncanonical schema should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_registry_rejects_entries_with_invalid_canonical_session_id() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_id": "../escape",
                    "session_name": "default",
                    "pid": 1234,
                    "socket_path": runtime.socket_path(),
                    "created_at": "2026-03-31T00:00:00Z",
                    "ipc_protocol_version": "1.0",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("invalid session_id should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_registry_rejects_entries_with_noncanonical_created_at() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_id": "sess-default",
                    "session_name": "default",
                    "pid": 1234,
                    "socket_path": runtime.socket_path(),
                    "created_at": "2026-03-31T00:00:00.0Z",
                    "ipc_protocol_version": "1.0",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("noncanonical created_at should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_registry_rejects_entries_with_invalid_session_name() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("bad/name", "sess-default");
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_id": "sess-default",
                    "session_name": "bad/name",
                    "pid": 1234,
                    "socket_path": runtime.socket_path(),
                    "created_at": "2026-03-31T00:00:00Z",
                    "ipc_protocol_version": "1.0",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("invalid session_name should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_registry_rejects_entries_with_noncanonical_socket_path() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_id": "sess-default",
                    "session_name": "default",
                    "pid": 1234,
                    "socket_path": "/tmp/not-rub.sock",
                    "created_at": "2026-03-31T00:00:00Z",
                    "ipc_protocol_version": "1.0",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("noncanonical socket_path should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_registry_rejects_entries_with_zero_pid() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_id": "sess-default",
                    "session_name": "default",
                    "pid": 0,
                    "socket_path": runtime.socket_path(),
                    "created_at": "2026-03-31T00:00:00Z",
                    "ipc_protocol_version": "1.0",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("zero pid should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_registry_rejects_entries_with_invalid_protocol_version_shape() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::write(
            home.join("registry.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": [{
                    "session_id": "sess-default",
                    "session_name": "default",
                    "pid": 1234,
                    "socket_path": runtime.socket_path(),
                    "created_at": "2026-03-31T00:00:00Z",
                    "ipc_protocol_version": " 1.x ",
                    "user_data_dir": null
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = read_registry(&home).expect_err("invalid protocol version should be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn write_registry_preserves_explicit_session_id() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let session_id = new_session_id();
        let runtime = RubPaths::new(&home).session_runtime("default", &session_id);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: session_id.clone(),
                    session_name: "default".to_string(),
                    pid: 1234,
                    socket_path: runtime.socket_path().display().to_string(),
                    created_at: "2026-03-31T00:00:00Z".to_string(),
                    ipc_protocol_version: "1.0".to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let registry = read_registry(&home).unwrap();
        assert_eq!(registry.sessions[0].session_id, session_id);

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn pending_session_keeps_existing_same_name_authority_until_promoted() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let old_runtime = RubPaths::new(&home).session_runtime("default", "old");
        let new_runtime = RubPaths::new(&home).session_runtime("default", "new");

        register_session(
            &home,
            RegistryEntry {
                session_id: "old".to_string(),
                session_name: "default".to_string(),
                pid: 1234,
                socket_path: old_runtime.socket_path().display().to_string(),
                created_at: "2026-04-01T00:00:00Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            },
        )
        .unwrap();
        register_pending_session(
            &home,
            RegistryEntry {
                session_id: "new".to_string(),
                session_name: "default".to_string(),
                pid: 5678,
                socket_path: new_runtime.socket_path().display().to_string(),
                created_at: "2026-04-01T00:00:01Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            },
        )
        .unwrap();

        let registry = read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 2);
        assert!(
            registry
                .sessions
                .iter()
                .any(|entry| entry.session_id == "old")
        );
        assert!(
            registry
                .sessions
                .iter()
                .any(|entry| entry.session_id == "new")
        );

        promote_session_authority(&home, "default", "new").unwrap();
        let registry = read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);
        assert_eq!(registry.sessions[0].session_id, "new");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn live_registry_identity_requires_matching_session_and_home() {
        let home = temp_home();
        let command = format!(
            r#"/workspace/target/debug/rub __daemon --session default --rub-home "{}""#,
            home.display()
        );
        assert!(is_matching_rub_daemon_command(&command, &home, "default"));
        assert!(!is_matching_rub_daemon_command(&command, &home, "other"));
        assert!(!is_matching_rub_daemon_command(
            &command,
            &home.join("nested"),
            "default"
        ));
    }

    #[test]
    #[cfg(unix)]
    fn live_registry_identity_requires_socket_handshake() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let session_name = "default";
        let session_id = "sess-default";
        let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
        let projection = RubPaths::new(&home).session(session_name);
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(
            projection.canonical_pid_path(),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(projection.startup_committed_path(), session_id).unwrap();
        symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();

        let listener = UnixListener::bind(runtime.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
            assert_eq!(decoded.command, "_handshake");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": "sess-default",
                }),
            );
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        let entry = RegistryEntry {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            pid: std::process::id(),
            socket_path: runtime.socket_path().display().to_string(),
            created_at: "2026-04-02T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        assert!(registry_entry_is_live_for_home(&home, &entry));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    #[cfg(unix)]
    fn live_registry_identity_requires_matching_handshake_session_id() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let session_name = "default";
        let session_id = "sess-default";
        let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
        let projection = RubPaths::new(&home).session(session_name);
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(
            projection.canonical_pid_path(),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(projection.startup_committed_path(), session_id).unwrap();
        symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();

        let listener = UnixListener::bind(runtime.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
            assert_eq!(decoded.command, "_handshake");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": "other-session",
                }),
            );
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        let entry = RegistryEntry {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            pid: std::process::id(),
            socket_path: runtime.socket_path().display().to_string(),
            created_at: "2026-04-02T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        assert!(!registry_entry_is_live_for_home(&home, &entry));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    #[cfg(unix)]
    fn live_registry_identity_requires_matching_protocol_version() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let session_name = "default";
        let session_id = "sess-default";
        let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
        let projection = RubPaths::new(&home).session(session_name);
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(
            projection.canonical_pid_path(),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(projection.startup_committed_path(), session_id).unwrap();
        symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();

        let listener = UnixListener::bind(runtime.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
            assert_eq!(decoded.command, "_handshake");
            let mut response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": "sess-default",
                }),
            );
            response.ipc_protocol_version = "0.9".to_string();
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        let entry = RegistryEntry {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            pid: std::process::id(),
            socket_path: runtime.socket_path().display().to_string(),
            created_at: "2026-04-02T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        assert!(!registry_entry_is_live_for_home(&home, &entry));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    #[cfg(unix)]
    fn slow_handshake_is_treated_as_busy_not_dead() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let session_name = "default";
        let session_id = "sess-slow";
        let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
        let projection = RubPaths::new(&home).session(session_name);
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(
            projection.canonical_pid_path(),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(projection.startup_committed_path(), session_id).unwrap();
        symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();

        let listener = UnixListener::bind(runtime.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(900));
        });

        let entry = RegistryEntry {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            pid: std::process::id(),
            socket_path: runtime.socket_path().display().to_string(),
            created_at: "2026-04-02T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        assert!(registry_entry_is_live_for_home(&home, &entry));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn pending_startup_is_not_live_but_is_explicitly_detectable() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        let session_name = "default";
        let session_id = "sess-pending";
        let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.socket_path(), b"socket").unwrap();

        let entry = RegistryEntry {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            pid: std::process::id(),
            socket_path: runtime.socket_path().display().to_string(),
            created_at: "2026-04-02T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        assert!(!registry_entry_is_live_for_home(&home, &entry));
        assert!(registry_entry_is_pending_startup_for_home(&home, &entry));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn register_session_replaces_same_session_name_authority() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let old_runtime = RubPaths::new(&home).session_runtime("default", "old");
        let new_runtime = RubPaths::new(&home).session_runtime("default", "new");

        register_session(
            &home,
            RegistryEntry {
                session_id: "old".to_string(),
                session_name: "default".to_string(),
                pid: 1234,
                socket_path: old_runtime.socket_path().display().to_string(),
                created_at: "2026-04-01T00:00:00Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            },
        )
        .unwrap();
        register_session(
            &home,
            RegistryEntry {
                session_id: "new".to_string(),
                session_name: "default".to_string(),
                pid: 5678,
                socket_path: new_runtime.socket_path().display().to_string(),
                created_at: "2026-04-01T00:00:01Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            },
        )
        .unwrap();

        let registry = read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);
        assert_eq!(registry.sessions[0].session_id, "new");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn authoritative_entry_by_session_name_falls_back_to_newest_stale_entry() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let older_runtime = RubPaths::new(&home).session_runtime("default", "older");
        let newer_runtime = RubPaths::new(&home).session_runtime("default", "newer");
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![
                    RegistryEntry {
                        session_id: "older".to_string(),
                        session_name: "default".to_string(),
                        pid: 1234,
                        socket_path: older_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-01T00:00:00Z".to_string(),
                        ipc_protocol_version: "1.0".to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "newer".to_string(),
                        session_name: "default".to_string(),
                        pid: 5678,
                        socket_path: newer_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-01T00:00:01Z".to_string(),
                        ipc_protocol_version: "1.0".to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                ],
            },
        )
        .unwrap();

        assert!(
            authoritative_entry_by_session_name(&home, "default")
                .unwrap()
                .is_none()
        );
        let entry = latest_entry_by_session_name(&home, "default")
            .unwrap()
            .expect("latest entry");
        assert_eq!(entry.session_id, "newer");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn registry_authority_snapshot_classifies_stale_and_uncertain_entries_once() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        let live_runtime = RubPaths::new(&home).session_runtime("default", "sess-live");
        std::fs::create_dir_all(live_runtime.session_dir()).unwrap();
        std::fs::write(live_runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::create_dir_all(
            live_runtime
                .startup_committed_path()
                .parent()
                .expect("startup committed parent"),
        )
        .unwrap();
        std::fs::write(live_runtime.startup_committed_path(), "sess-live").unwrap();
        let listener = UnixListener::bind(live_runtime.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: IpcRequest = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(request.command, "_handshake");
            let response = IpcResponse::success(
                request.command.clone(),
                serde_json::json!({
                    "daemon_session_id": "sess-live",
                }),
            );
            let encoded = NdJsonCodec::encode(&response).unwrap();
            reader.get_mut().write_all(&encoded).unwrap();
        });

        let dead_runtime = RubPaths::new(&home).session_runtime("default", "sess-dead");
        let uncertain_runtime = RubPaths::new(&home).session_runtime("default", "sess-uncertain");
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![
                    RegistryEntry {
                        session_id: "sess-dead".to_string(),
                        session_name: "default".to_string(),
                        pid: 999_999,
                        socket_path: dead_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-03T00:00:00Z".to_string(),
                        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "sess-uncertain".to_string(),
                        session_name: "default".to_string(),
                        pid: std::process::id(),
                        socket_path: uncertain_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-03T00:00:01Z".to_string(),
                        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "sess-live".to_string(),
                        session_name: "default".to_string(),
                        pid: std::process::id(),
                        socket_path: live_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-03T00:00:02Z".to_string(),
                        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                ],
            },
        )
        .unwrap();

        let snapshot = registry_authority_snapshot(&home).unwrap();
        let session = snapshot.session("default").expect("session snapshot");
        assert_eq!(
            session
                .authoritative_entry()
                .map(|entry| entry.entry.session_id.as_str()),
            Some("sess-live")
        );
        assert_eq!(
            session
                .stale_entries()
                .into_iter()
                .map(|entry| entry.session_id)
                .collect::<Vec<_>>(),
            vec!["sess-dead".to_string()]
        );
        assert!(session.has_uncertain_entries());

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn latest_entry_by_session_name_orders_by_parsed_timestamp_not_raw_string() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let older_runtime = RubPaths::new(&home).session_runtime("default", "older");
        let newer_runtime = RubPaths::new(&home).session_runtime("default", "newer");
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![
                    RegistryEntry {
                        session_id: "older".to_string(),
                        session_name: "default".to_string(),
                        pid: 1234,
                        socket_path: older_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-01T00:00:00Z".to_string(),
                        ipc_protocol_version: "1.0".to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "newer".to_string(),
                        session_name: "default".to_string(),
                        pid: 5678,
                        socket_path: newer_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-01T00:00:00.9Z".to_string(),
                        ipc_protocol_version: "1.0".to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                ],
            },
        )
        .unwrap();

        let entry = latest_entry_by_session_name(&home, "default")
            .unwrap()
            .expect("latest entry");
        assert_eq!(entry.session_id, "newer");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn cleanup_projections_preserves_foreign_startup_commit_marker() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        let old_entry = RegistryEntry {
            session_id: "old".to_string(),
            session_name: "default".to_string(),
            pid: 1234,
            socket_path: RubPaths::new(&home)
                .session_runtime("default", "old")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let projection = RubPaths::new(&home).session("default");
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(projection.startup_committed_path(), b"new").unwrap();

        cleanup_projections(&home, &old_entry);

        assert_eq!(
            std::fs::read_to_string(projection.startup_committed_path()).unwrap(),
            "new"
        );

        let _ = std::fs::remove_dir_all(home);
    }
}
