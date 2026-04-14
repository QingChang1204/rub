use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes;
use rub_core::model::{BindingRecord, BindingRegistryData};
use rub_daemon::rub_paths::{RubPaths, validate_session_id_component, validate_session_name};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::Path;

use super::binding_path_state;

pub(crate) fn read_binding_registry(rub_home: &Path) -> Result<BindingRegistryData, RubError> {
    with_bindings_lock(rub_home, false, |path| {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| {
                binding_registry_io_error(rub_home, path, "binding_registry_open_failed", error)
            })?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|error| {
            binding_registry_io_error(rub_home, path, "binding_registry_read_failed", error)
        })?;
        let registry = if contents.trim().is_empty() {
            BindingRegistryData::default()
        } else {
            serde_json::from_str::<BindingRegistryData>(&contents).map_err(|error| {
                RubError::domain_with_context(
                    ErrorCode::JsonError,
                    format!(
                        "Failed to parse binding registry {}: {error}",
                        path.display()
                    ),
                    json!({
                        "registry_path": path.display().to_string(),
                        "registry_path_state": binding_path_state(
                            "cli.binding.subject.registry_path",
                            "cli_binding_registry",
                            "binding_registry_file",
                        ),
                        "reason": "binding_registry_parse_failed",
                    }),
                )
            })?
        };
        validate_binding_registry(rub_home, &registry)?;
        Ok(registry)
    })
}

pub(crate) fn write_binding_registry(
    rub_home: &Path,
    registry: &BindingRegistryData,
) -> Result<(), RubError> {
    with_bindings_lock(rub_home, true, |path| {
        let mut normalized = registry.clone();
        normalized
            .bindings
            .sort_by(|left, right| left.alias.cmp(&right.alias));
        validate_binding_registry(rub_home, &normalized)?;
        let json = serde_json::to_vec_pretty(&normalized).map_err(RubError::from)?;
        atomic_write_bytes(path, &json, 0o600).map_err(|error| {
            binding_registry_io_error(rub_home, path, "binding_registry_write_failed", error)
        })?;
        Ok(())
    })
}

fn validate_binding_registry(
    rub_home: &Path,
    registry: &BindingRegistryData,
) -> Result<(), RubError> {
    if registry.schema_version != 1 {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Unsupported binding registry schema_version {}; expected 1",
                registry.schema_version
            ),
            json!({
                "schema_version": registry.schema_version,
                "reason": "binding_registry_schema_version_unsupported",
            }),
        ));
    }

    let mut seen = BTreeSet::new();
    for binding in &registry.bindings {
        validate_binding_record(rub_home, binding, &mut seen)?;
    }
    Ok(())
}

fn validate_binding_record(
    rub_home: &Path,
    binding: &BindingRecord,
    seen: &mut BTreeSet<String>,
) -> Result<(), RubError> {
    let normalized = normalize_binding_alias(&binding.alias)?;
    if normalized != binding.alias {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Binding alias '{}' is non-canonical; expected '{}'",
                binding.alias, normalized
            ),
            json!({
                "alias": binding.alias,
                "expected_alias": normalized,
                "reason": "binding_alias_noncanonical",
            }),
        ));
    }
    if !seen.insert(binding.alias.clone()) {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Duplicate binding alias '{}'", binding.alias),
            json!({
                "alias": binding.alias,
                "reason": "binding_alias_duplicate",
            }),
        ));
    }
    if binding.rub_home_reference != rub_home.display().to_string() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Binding alias '{}' points at rub_home '{}' but registry is scoped to '{}'",
                binding.alias,
                binding.rub_home_reference,
                rub_home.display()
            ),
            json!({
                "alias": binding.alias,
                "reason": "binding_rub_home_mismatch",
            }),
        ));
    }
    if let Some(reference) = &binding.session_reference {
        validate_session_id_component(&reference.session_id).map_err(|reason| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Binding alias '{}' has invalid session_id '{}': {reason}",
                    binding.alias, reference.session_id
                ),
                json!({
                    "alias": binding.alias,
                    "reason": "binding_session_id_invalid",
                }),
            )
        })?;
        validate_session_name(&reference.session_name).map_err(|reason| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Binding alias '{}' has invalid session_name '{}': {reason}",
                    binding.alias, reference.session_name
                ),
                json!({
                    "alias": binding.alias,
                    "reason": "binding_session_name_invalid",
                }),
            )
        })?;
    }
    Ok(())
}

fn with_bindings_lock<T>(
    rub_home: &Path,
    exclusive: bool,
    f: impl FnOnce(&Path) -> Result<T, RubError>,
) -> Result<T, RubError> {
    std::fs::create_dir_all(rub_home).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::IoError,
            format!("Failed to create RUB_HOME {}: {error}", rub_home.display()),
            json!({
                "rub_home": rub_home.display().to_string(),
                "rub_home_state": binding_path_state(
                    "cli.binding.subject.rub_home",
                    "cli_binding_registry",
                    "rub_home_directory",
                ),
                "reason": "binding_rub_home_create_failed",
            }),
        )
    })?;

    let paths = RubPaths::new(rub_home);
    let registry_path = paths.bindings_path();
    let lock_path = paths.bindings_lock_path();
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            binding_registry_io_error(
                rub_home,
                &lock_path,
                "binding_registry_lock_open_failed",
                error,
            )
        })?;

    flock(&lock_file, exclusive).map_err(|error| {
        binding_registry_io_error(rub_home, &lock_path, "binding_registry_lock_failed", error)
    })?;
    let result = f(&registry_path);
    let unlock_result = unlock(&lock_file);

    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(binding_registry_io_error(
            rub_home,
            &lock_path,
            "binding_registry_unlock_failed",
            error,
        )),
        (Err(error), Err(_)) => Err(error),
    }
}

fn binding_registry_io_error(
    rub_home: &Path,
    path: &Path,
    reason: &str,
    error: std::io::Error,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IoError,
        format!("Binding registry path {} failed: {error}", path.display()),
        json!({
            "rub_home": rub_home.display().to_string(),
            "path": path.display().to_string(),
            "path_state": binding_path_state(
                "cli.binding.registry.path",
                "cli_binding_registry",
                "binding_registry_path",
            ),
            "reason": reason,
        }),
    )
}

pub(crate) fn normalize_binding_alias(alias: &str) -> Result<String, RubError> {
    let trimmed = alias.trim();
    if trimmed.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Binding alias cannot be empty",
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid binding alias '{alias}'"),
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Invalid binding alias '{alias}'; use letters, digits, underscores, and dashes"
            ),
        ));
    }
    Ok(trimmed.to_string())
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
