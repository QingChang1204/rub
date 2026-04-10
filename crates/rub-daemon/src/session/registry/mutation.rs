use std::path::Path;

use super::liveness::registry_entry_is_live_for_home;
use super::snapshot::compare_registry_entry_created_at;
use super::storage::{load_registry_for_home, store_registry_for_home, with_registry_lock};
use super::validation::validate_registry_entry_for_home;
use super::{RegistryEntry, read_registry};

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
        data.sessions.retain(|entry| entry.session_id != session_id);
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
