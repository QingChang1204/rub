use super::{binding_path_state, normalize_binding_alias, remembered_binding_registry_io_error};
use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes;
use rub_core::model::RememberedBindingAliasRegistryData;
use rub_daemon::rub_paths::RubPaths;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::Path;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub(super) fn read_remembered_alias_registry(
    rub_home: &Path,
) -> Result<RememberedBindingAliasRegistryData, RubError> {
    with_remembered_bindings_lock(rub_home, false, |path| {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| {
                remembered_binding_registry_io_error(
                    rub_home,
                    path,
                    "remembered_binding_registry_open_failed",
                    error,
                )
            })?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|error| {
            remembered_binding_registry_io_error(
                rub_home,
                path,
                "remembered_binding_registry_read_failed",
                error,
            )
        })?;
        let registry = if contents.trim().is_empty() {
            RememberedBindingAliasRegistryData::default()
        } else {
            serde_json::from_str::<RememberedBindingAliasRegistryData>(&contents).map_err(
                |error| {
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
            )?
        };
        validate_remembered_alias_registry(&registry)?;
        Ok(registry)
    })
}

pub(super) fn write_remembered_alias_registry(
    rub_home: &Path,
    registry: &RememberedBindingAliasRegistryData,
) -> Result<(), RubError> {
    with_remembered_bindings_lock(rub_home, true, |path| {
        let mut normalized = registry.clone();
        normalized
            .aliases
            .sort_by(|left, right| left.alias.cmp(&right.alias));
        validate_remembered_alias_registry(&normalized)?;
        let json = serde_json::to_vec_pretty(&normalized).map_err(RubError::from)?;
        atomic_write_bytes(path, &json, 0o600).map_err(|error| {
            remembered_binding_registry_io_error(
                rub_home,
                path,
                "remembered_binding_registry_write_failed",
                error,
            )
        })?;
        Ok(())
    })
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
    std::fs::create_dir_all(rub_home).map_err(|error| {
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
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            remembered_binding_registry_io_error(
                rub_home,
                &lock_path,
                "remembered_binding_registry_lock_open_failed",
                error,
            )
        })?;

    flock(&lock_file, exclusive).map_err(|error| {
        remembered_binding_registry_io_error(
            rub_home,
            &lock_path,
            "remembered_binding_registry_lock_failed",
            error,
        )
    })?;
    let result = f(&registry_path);
    let unlock_result = unlock(&lock_file);

    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(remembered_binding_registry_io_error(
            rub_home,
            &lock_path,
            "remembered_binding_registry_unlock_failed",
            error,
        )),
        (Err(error), Err(_)) => Err(error),
    }
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
