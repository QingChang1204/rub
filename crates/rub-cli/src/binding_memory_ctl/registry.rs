use super::{binding_path_state, normalize_binding_alias, remembered_binding_registry_io_error};
use crate::local_registry::{
    ensure_directory, load_json_file_with_create, with_file_lock, write_pretty_json_file,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{BindingRegistryData, RememberedBindingAliasRegistryData};
use rub_daemon::rub_paths::RubPaths;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[cfg(test)]
pub(super) fn read_remembered_alias_registry(
    rub_home: &Path,
) -> Result<RememberedBindingAliasRegistryData, RubError> {
    with_remembered_bindings_lock(rub_home, false, |path| {
        load_remembered_alias_registry_unlocked(rub_home, path)
    })
}

pub(crate) fn read_binding_and_remembered_alias_registries(
    rub_home: &Path,
) -> Result<(BindingRegistryData, RememberedBindingAliasRegistryData), RubError> {
    crate::binding_ctl::registry::with_bindings_lock(rub_home, false, |binding_path| {
        with_remembered_bindings_lock(rub_home, false, |remembered_path| {
            let binding_registry = crate::binding_ctl::registry::load_binding_registry_unlocked(
                rub_home,
                binding_path,
            )?;
            let remembered_registry =
                load_remembered_alias_registry_unlocked(rub_home, remembered_path)?;
            Ok((binding_registry, remembered_registry))
        })
    })
}

#[cfg(test)]
pub(super) fn write_remembered_alias_registry(
    rub_home: &Path,
    registry: &RememberedBindingAliasRegistryData,
) -> Result<(), RubError> {
    with_remembered_bindings_lock(rub_home, true, |path| {
        write_remembered_alias_registry_unlocked(rub_home, path, registry)
    })
}

pub(super) fn mutate_remembered_alias_registry<T>(
    rub_home: &Path,
    mutation: impl FnOnce(&mut RememberedBindingAliasRegistryData) -> Result<T, RubError>,
) -> Result<T, RubError> {
    with_remembered_bindings_lock(rub_home, true, |path| {
        let mut registry = load_remembered_alias_registry_unlocked(rub_home, path)?;
        let result = mutation(&mut registry)?;
        write_remembered_alias_registry_unlocked(rub_home, path, &registry)?;
        Ok(result)
    })
}

pub(crate) fn mutate_binding_and_remembered_alias_registries<T>(
    rub_home: &Path,
    mutation: impl FnOnce(
        &mut BindingRegistryData,
        &mut RememberedBindingAliasRegistryData,
    ) -> Result<T, RubError>,
) -> Result<T, RubError> {
    crate::binding_ctl::registry::with_bindings_lock(rub_home, true, |binding_path| {
        with_remembered_bindings_lock(rub_home, true, |remembered_path| {
            let mut binding_registry =
                crate::binding_ctl::registry::load_binding_registry_unlocked(
                    rub_home,
                    binding_path,
                )?;
            let mut remembered_registry =
                load_remembered_alias_registry_unlocked(rub_home, remembered_path)?;
            let original_binding_registry = binding_registry.clone();
            let original_remembered_registry = remembered_registry.clone();
            let result = mutation(&mut binding_registry, &mut remembered_registry)?;
            let binding_changed = binding_registry != original_binding_registry;
            let remembered_changed = remembered_registry != original_remembered_registry;
            if binding_changed && remembered_changed {
                return Err(RubError::domain_with_context(
                    rub_core::error::ErrorCode::InvalidInput,
                    "Binding registry and remembered alias registry cannot be mutated in the same local transaction",
                    serde_json::json!({
                        "reason": "cross_registry_atomic_mutation_unsupported",
                        "binding_registry_path": binding_path.display().to_string(),
                        "remembered_alias_registry_path": remembered_path.display().to_string(),
                    }),
                ));
            }
            if binding_changed {
                crate::binding_ctl::registry::write_binding_registry_unlocked(
                    rub_home,
                    binding_path,
                    &binding_registry,
                )?;
            }
            if remembered_changed {
                write_remembered_alias_registry_unlocked(
                    rub_home,
                    remembered_path,
                    &remembered_registry,
                )?;
            }
            Ok(result)
        })
    })
}

pub(super) fn load_remembered_alias_registry_unlocked(
    rub_home: &Path,
    path: &Path,
) -> Result<RememberedBindingAliasRegistryData, RubError> {
    let registry = load_json_file_with_create(
        path,
        |path, reason, error| {
            remembered_binding_registry_io_error(
                rub_home,
                path,
                match reason {
                    "open_failed" => "remembered_binding_registry_open_failed",
                    "read_failed" => "remembered_binding_registry_read_failed",
                    _ => "remembered_binding_registry_io_failed",
                },
                error,
            )
        },
        |path, error| {
            RubError::domain_with_context(
                ErrorCode::JsonError,
                format!(
                    "Failed to parse remembered binding registry {}: {error}",
                    path.display()
                ),
                json!({
                    "registry_path": path.display().to_string(),
                    "registry_path_state": binding_path_state(
                        "cli.remembered_binding.subject.registry_path",
                        "cli_remembered_binding_registry",
                        "remembered_binding_registry_file",
                    ),
                    "reason": "remembered_binding_registry_parse_failed",
                }),
            )
        },
    )?;
    validate_remembered_alias_registry(&registry)?;
    Ok(registry)
}

pub(super) fn write_remembered_alias_registry_unlocked(
    rub_home: &Path,
    path: &Path,
    registry: &RememberedBindingAliasRegistryData,
) -> Result<(), RubError> {
    let mut normalized = registry.clone();
    normalized
        .aliases
        .sort_by(|left, right| left.alias.cmp(&right.alias));
    validate_remembered_alias_registry(&normalized)?;
    write_pretty_json_file(path, &normalized, 0o600, |path, reason, error| {
        remembered_binding_registry_io_error(
            rub_home,
            path,
            match reason {
                "serialize_failed" => "remembered_binding_registry_serialize_failed",
                "write_failed" => "remembered_binding_registry_write_failed",
                _ => "remembered_binding_registry_io_failed",
            },
            error,
        )
    })?;
    Ok(())
}

pub(super) fn rfc3339_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

fn validate_remembered_alias_registry(
    registry: &RememberedBindingAliasRegistryData,
) -> Result<(), RubError> {
    if registry.schema_version != 1 {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Unsupported remembered binding registry schema_version {}; expected 1",
                registry.schema_version
            ),
            json!({
                "schema_version": registry.schema_version,
                "reason": "remembered_binding_registry_schema_version_unsupported",
            }),
        ));
    }

    let mut seen = BTreeSet::new();
    for record in &registry.aliases {
        let normalized_alias = normalize_binding_alias(&record.alias)?;
        if normalized_alias != record.alias {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Remembered alias '{}' is non-canonical; expected '{}'",
                    record.alias, normalized_alias
                ),
                json!({
                    "alias": record.alias,
                    "expected_alias": normalized_alias,
                    "reason": "remembered_alias_noncanonical",
                }),
            ));
        }
        if !seen.insert(record.alias.clone()) {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("Duplicate remembered alias '{}'", record.alias),
                json!({
                    "alias": record.alias,
                    "reason": "remembered_alias_duplicate",
                }),
            ));
        }
        let normalized_binding_alias = normalize_binding_alias(&record.binding_alias)?;
        if normalized_binding_alias != record.binding_alias {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Remembered alias '{}' points at non-canonical binding alias '{}'",
                    record.alias, record.binding_alias
                ),
                json!({
                    "alias": record.alias,
                    "binding_alias": record.binding_alias,
                    "expected_binding_alias": normalized_binding_alias,
                    "reason": "remembered_alias_binding_noncanonical",
                }),
            ));
        }
    }

    Ok(())
}

fn with_remembered_bindings_lock<T>(
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
                    "cli.remembered_binding.subject.rub_home",
                    "cli_remembered_binding_registry",
                    "rub_home_directory",
                ),
                "reason": "remembered_binding_rub_home_create_failed",
            }),
        )
    })?;

    let paths = RubPaths::new(rub_home);
    let registry_path = paths.remembered_bindings_path();
    let lock_path = paths.remembered_bindings_lock_path();
    with_file_lock(
        &lock_path,
        exclusive,
        "remembered_binding_registry_lock_open_failed",
        "remembered_binding_registry_lock_failed",
        "remembered_binding_registry_unlock_failed",
        |path, reason, error| remembered_binding_registry_io_error(rub_home, path, reason, error),
        || f(&registry_path),
    )
}
