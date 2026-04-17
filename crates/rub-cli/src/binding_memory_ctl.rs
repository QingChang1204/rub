use crate::binding_ctl::{
    binding_alias_not_found_error, binding_path_state, load_binding_resolution_state,
    normalize_binding_alias, read_binding_registry, resolve_binding_target,
    resolve_binding_target_from_state,
};
use crate::commands::RememberedBindingAliasKindArg;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{RememberedBindingAliasKind, RememberedBindingAliasRecord};
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::Path;

mod registry;

use self::registry::{
    read_remembered_alias_registry, rfc3339_now, write_remembered_alias_registry,
};

pub(crate) fn project_remembered_alias_list(rub_home: &Path) -> Result<Value, RubError> {
    let registry = read_remembered_alias_registry(rub_home)?;
    let binding_state = load_binding_resolution_state(rub_home)?;
    let items = registry
        .aliases
        .iter()
        .map(|alias| {
            Ok(json!({
                "remembered_alias": alias,
                "target": resolve_binding_target_from_state(&alias.binding_alias, &binding_state)?,
            }))
        })
        .collect::<Result<Vec<_>, RubError>>()?;

    Ok(json!({
        "subject": remembered_alias_registry_subject(rub_home),
        "result": {
            "schema_version": registry.schema_version,
            "items": items,
        }
    }))
}

pub(crate) fn remember_binding_alias(
    rub_home: &Path,
    alias: &str,
    binding_alias: &str,
    kind: RememberedBindingAliasKindArg,
) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    let binding_alias = normalize_binding_alias(binding_alias)?;
    ensure_binding_target_exists(rub_home, &binding_alias)?;

    let mut registry = read_remembered_alias_registry(rub_home)?;
    if registry.aliases.iter().any(|record| record.alias == alias) {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Remembered alias already exists: {alias}"),
            json!({
                "alias": alias,
                "reason": "remembered_alias_already_exists",
            }),
        ));
    }

    let now = rfc3339_now();
    let record = RememberedBindingAliasRecord {
        alias: alias.clone(),
        kind: remembered_alias_kind(kind),
        binding_alias: binding_alias.clone(),
        created_at: now.clone(),
        updated_at: now,
    };
    registry.aliases.push(record.clone());
    write_remembered_alias_registry(rub_home, &registry)?;

    Ok(json!({
        "subject": remembered_alias_subject(rub_home, &alias),
        "result": {
            "action": "remember",
            "remembered_alias": record,
            "target": resolve_binding_target(rub_home, &binding_alias)?,
        }
    }))
}

pub(crate) fn resolve_remembered_alias(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let (record, target) = resolve_remembered_alias_target(rub_home, alias)?;

    Ok(json!({
        "subject": remembered_alias_subject(rub_home, &record.alias),
        "result": {
            "remembered_alias": record.clone(),
            "target": target,
        }
    }))
}

pub(crate) fn resolve_remembered_alias_target(
    rub_home: &Path,
    alias: &str,
) -> Result<
    (
        RememberedBindingAliasRecord,
        rub_core::model::RememberedBindingAliasTarget,
    ),
    RubError,
> {
    let normalized = normalize_binding_alias(alias)?;
    let registry = read_remembered_alias_registry(rub_home)?;
    let binding_state = load_binding_resolution_state(rub_home)?;
    let record = registry
        .aliases
        .iter()
        .find(|record| record.alias == normalized)
        .cloned()
        .ok_or_else(|| remembered_alias_not_found_error(rub_home, &normalized))?;
    let target = resolve_binding_target_from_state(&record.binding_alias, &binding_state)?;
    Ok((record, target))
}

pub(crate) fn rebind_remembered_alias(
    rub_home: &Path,
    alias: &str,
    binding_alias: &str,
    kind: Option<RememberedBindingAliasKindArg>,
) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    let binding_alias = normalize_binding_alias(binding_alias)?;
    ensure_binding_target_exists(rub_home, &binding_alias)?;

    let mut registry = read_remembered_alias_registry(rub_home)?;
    let record = registry
        .aliases
        .iter_mut()
        .find(|record| record.alias == alias)
        .ok_or_else(|| remembered_alias_not_found_error(rub_home, &alias))?;
    let previous_binding_alias = record.binding_alias.clone();
    record.binding_alias = binding_alias.clone();
    if let Some(kind) = kind {
        record.kind = remembered_alias_kind(kind);
    }
    record.updated_at = rfc3339_now();
    let updated = record.clone();
    write_remembered_alias_registry(rub_home, &registry)?;

    Ok(json!({
        "subject": remembered_alias_subject(rub_home, &alias),
        "result": {
            "action": "rebind",
            "previous_binding_alias": previous_binding_alias,
            "remembered_alias": updated,
            "target": resolve_binding_target(rub_home, &binding_alias)?,
        }
    }))
}

pub(crate) fn forget_remembered_alias(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    let mut registry = read_remembered_alias_registry(rub_home)?;
    let original_len = registry.aliases.len();
    registry.aliases.retain(|record| record.alias != alias);
    if registry.aliases.len() == original_len {
        return Err(remembered_alias_not_found_error(rub_home, &alias));
    }
    write_remembered_alias_registry(rub_home, &registry)?;

    Ok(json!({
        "subject": remembered_alias_subject(rub_home, &alias),
        "result": {
            "removed_alias": alias,
        }
    }))
}

pub(crate) fn remembered_aliases_referencing_binding(
    rub_home: &Path,
    binding_alias: &str,
) -> Result<Vec<String>, RubError> {
    let normalized = normalize_binding_alias(binding_alias)?;
    let registry = read_remembered_alias_registry(rub_home)?;
    Ok(registry
        .aliases
        .into_iter()
        .filter(|record| record.binding_alias == normalized)
        .map(|record| record.alias)
        .collect())
}

pub(crate) fn remembered_alias_registry_subject(rub_home: &Path) -> Value {
    let paths = RubPaths::new(rub_home);
    json!({
        "kind": "remembered_binding_alias_registry",
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": binding_path_state(
            "cli.remembered_binding.subject.rub_home",
            "cli_remembered_binding_registry",
            "rub_home_directory",
        ),
        "registry_path": paths.remembered_bindings_path().display().to_string(),
        "registry_path_state": binding_path_state(
            "cli.remembered_binding.subject.registry_path",
            "cli_remembered_binding_registry",
            "remembered_binding_registry_file",
        ),
        "lock_path": paths.remembered_bindings_lock_path().display().to_string(),
        "lock_path_state": binding_path_state(
            "cli.remembered_binding.subject.lock_path",
            "cli_remembered_binding_registry",
            "remembered_binding_registry_lock",
        ),
    })
}

pub(crate) fn remembered_alias_subject(rub_home: &Path, alias: &str) -> Value {
    json!({
        "kind": "remembered_binding_alias",
        "alias": alias,
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": binding_path_state(
            "cli.remembered_binding.subject.rub_home",
            "cli_remembered_binding_registry",
            "rub_home_directory",
        ),
    })
}

fn ensure_binding_target_exists(rub_home: &Path, binding_alias: &str) -> Result<(), RubError> {
    let normalized = normalize_binding_alias(binding_alias)?;
    let registry = read_binding_registry(rub_home)?;
    if registry
        .bindings
        .iter()
        .any(|binding| binding.alias == normalized)
    {
        Ok(())
    } else {
        Err(binding_alias_not_found_error(rub_home, &normalized))
    }
}

fn remembered_alias_not_found_error(rub_home: &Path, alias: &str) -> RubError {
    let paths = RubPaths::new(rub_home);
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Remembered alias not found: {alias}"),
        json!({
            "alias": alias,
            "registry_path": paths.remembered_bindings_path().display().to_string(),
            "registry_path_state": binding_path_state(
                "cli.remembered_binding.subject.registry_path",
                "cli_remembered_binding_registry",
                "remembered_binding_registry_file",
            ),
            "reason": "remembered_alias_not_found",
        }),
    )
}

fn remembered_alias_kind(kind: RememberedBindingAliasKindArg) -> RememberedBindingAliasKind {
    match kind {
        RememberedBindingAliasKindArg::Binding => RememberedBindingAliasKind::Binding,
        RememberedBindingAliasKindArg::Account => RememberedBindingAliasKind::Account,
        RememberedBindingAliasKindArg::Workspace => RememberedBindingAliasKind::Workspace,
    }
}

pub(super) fn remembered_binding_registry_io_error(
    rub_home: &Path,
    path: &Path,
    reason: &str,
    error: std::io::Error,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IoError,
        format!(
            "Remembered binding registry path {} failed: {error}",
            path.display()
        ),
        json!({
            "rub_home": rub_home.display().to_string(),
            "path": path.display().to_string(),
            "path_state": binding_path_state(
                "cli.remembered_binding.registry.path",
                "cli_remembered_binding_registry",
                "remembered_binding_registry_path",
            ),
            "reason": reason,
        }),
    )
}

#[cfg(test)]
mod tests;
