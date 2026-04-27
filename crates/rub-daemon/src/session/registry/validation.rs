use super::RegistryData;
use super::RegistryEntry;
use super::snapshot;
use crate::rub_paths::{RubPaths, validate_session_id_component, validate_session_name};
use std::path::Path;
use time::format_description::well_known::Rfc3339;

pub(super) fn validate_registry_data_for_home(
    home: &Path,
    data: &RegistryData,
) -> std::io::Result<()> {
    for entry in &data.sessions {
        validate_registry_entry_for_home(home, entry)?;
    }
    Ok(())
}

pub(crate) fn validate_registry_entry_for_home(
    home: &Path,
    entry: &RegistryEntry,
) -> std::io::Result<()> {
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
    let parsed_created_at =
        snapshot::parsed_registry_created_at(&entry.created_at).map_err(|error| {
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
    let recorded_socket_path = Path::new(&entry.socket_path);
    if recorded_socket_path != expected_socket_path
        && !is_legacy_runtime_socket_path(recorded_socket_path)
    {
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

pub(super) fn is_legacy_runtime_socket_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut components = path.components();
    let (
        Some(std::path::Component::RootDir),
        Some(std::path::Component::Normal(tmp)),
        Some(std::path::Component::Normal(parent)),
        Some(std::path::Component::Normal(file)),
        None,
    ) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    )
    else {
        return false;
    };
    if tmp != "tmp" {
        return false;
    }
    let Some(parent_name) = parent.to_str() else {
        return false;
    };
    let Some(legacy_tag) = parent_name.strip_prefix("rub-sock-") else {
        return false;
    };
    if legacy_tag.is_empty()
        || !legacy_tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return false;
    }
    let Some(file_name) = file.to_str() else {
        return false;
    };
    let Some(hex) = file_name.strip_suffix(".sock") else {
        return false;
    };
    hex.len() == 16 && hex.chars().all(|ch| ch.is_ascii_hexdigit())
}
