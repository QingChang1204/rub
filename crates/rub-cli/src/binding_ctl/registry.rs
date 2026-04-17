use crate::local_registry::{
    ensure_directory, load_json_file_with_create, with_file_lock, write_pretty_json_file,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{BindingRecord, BindingRegistryData};
use rub_daemon::rub_paths::{RubPaths, validate_session_id_component, validate_session_name};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

use super::binding_path_state;

pub(crate) fn read_binding_registry(rub_home: &Path) -> Result<BindingRegistryData, RubError> {
    with_bindings_lock(rub_home, false, |path| {
        let registry = load_json_file_with_create(
            path,
            |path, reason, error| {
                binding_registry_io_error(
                    rub_home,
                    path,
                    match reason {
                        "open_failed" => "binding_registry_open_failed",
                        "read_failed" => "binding_registry_read_failed",
                        _ => "binding_registry_io_failed",
                    },
                    error,
                )
            },
            |path, error| {
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
            },
        )?;
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
        write_pretty_json_file(path, &normalized, 0o600, |path, reason, error| {
            binding_registry_io_error(
                rub_home,
                path,
                match reason {
                    "serialize_failed" => "binding_registry_serialize_failed",
                    "write_failed" => "binding_registry_write_failed",
                    _ => "binding_registry_io_failed",
                },
                error,
            )
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
    ensure_directory(rub_home).map_err(|error| {
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
    with_file_lock(
        &lock_path,
        exclusive,
        "binding_registry_lock_open_failed",
        "binding_registry_lock_failed",
        "binding_registry_unlock_failed",
        |path, reason, error| binding_registry_io_error(rub_home, path, reason, error),
        || f(&registry_path),
    )
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
