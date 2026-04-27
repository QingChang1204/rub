use crate::binding_ctl::{
    BindingResolutionState, binding_alias_not_found_error, binding_path_state,
    load_binding_resolution_state_from_registry, normalize_binding_alias,
    project_live_registry_error, resolve_binding_target_from_state,
};
use crate::commands::RememberedBindingAliasKindArg;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingRegistryData, RememberedBindingAliasKind, RememberedBindingAliasRecord,
    RememberedBindingAliasRegistryData,
};
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::Path;

pub(crate) mod registry;

pub(crate) use self::registry::mutate_binding_and_remembered_alias_registries;
use self::registry::{
    mutate_remembered_alias_registry, read_binding_and_remembered_alias_registries, rfc3339_now,
};
#[cfg(test)]
use self::registry::{read_remembered_alias_registry, write_remembered_alias_registry};

pub(crate) fn project_remembered_alias_list(rub_home: &Path) -> Result<Value, RubError> {
    let (registry, binding_state) = load_remembered_alias_resolution_state(rub_home)?;
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

    let mut projection = json!({
        "subject": remembered_alias_registry_subject(rub_home),
        "result": {
            "schema_version": registry.schema_version,
            "items": items,
        }
    });
    if let Some(error) = binding_state
        .live_snapshot_error()
        .map(project_live_registry_error)
    {
        projection["result"]["live_registry_error"] = error;
    }
    Ok(projection)
}

pub(crate) fn remember_binding_alias(
    rub_home: &Path,
    alias: &str,
    binding_alias: &str,
    kind: RememberedBindingAliasKindArg,
) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    let binding_alias = normalize_binding_alias(binding_alias)?;

    let now = rfc3339_now();
    let record = RememberedBindingAliasRecord {
        alias: alias.clone(),
        kind: remembered_alias_kind(kind),
        binding_alias: binding_alias.clone(),
        created_at: now.clone(),
        updated_at: now,
    };
    mutate_binding_and_remembered_alias_registries(
        rub_home,
        |binding_registry, remembered_registry| {
            ensure_binding_target_exists_in_registry(rub_home, binding_registry, &binding_alias)?;
            if remembered_registry
                .aliases
                .iter()
                .any(|record| record.alias == alias)
            {
                return Err(RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("Remembered alias already exists: {alias}"),
                    json!({
                        "alias": alias,
                        "reason": "remembered_alias_already_exists",
                    }),
                ));
            }
            remembered_registry.aliases.push(record.clone());
            Ok(())
        },
    )?;

    let (_, binding_state) = load_remembered_alias_resolution_state(rub_home)?;
    let target = resolve_binding_target_from_state(&binding_alias, &binding_state)?;
    let mut projection = json!({
        "subject": remembered_alias_subject(rub_home, &alias),
        "result": {
            "action": "remember",
            "remembered_alias": record,
            "target": target,
        }
    });
    if let Some(error) = binding_state
        .live_snapshot_error()
        .map(project_live_registry_error)
    {
        projection["result"]["live_registry_error"] = error;
    }
    Ok(projection)
}

pub(crate) fn resolve_remembered_alias(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let (record, target, binding_state) = resolve_remembered_alias_target(rub_home, alias)?;

    let mut projection = json!({
        "subject": remembered_alias_subject(rub_home, &record.alias),
        "result": {
            "remembered_alias": record.clone(),
            "target": target,
        }
    });
    if let Some(error) = binding_state
        .live_snapshot_error()
        .map(project_live_registry_error)
    {
        projection["result"]["live_registry_error"] = error;
    }
    Ok(projection)
}

pub(crate) fn resolve_remembered_alias_target(
    rub_home: &Path,
    alias: &str,
) -> Result<
    (
        RememberedBindingAliasRecord,
        rub_core::model::RememberedBindingAliasTarget,
        BindingResolutionState,
    ),
    RubError,
> {
    let normalized = normalize_binding_alias(alias)?;
    let (registry, binding_state) = load_remembered_alias_resolution_state(rub_home)?;
    let (record, target) = resolve_remembered_alias_target_from_binding_state(
        rub_home,
        &normalized,
        registry,
        &binding_state,
    )?;
    Ok((record, target, binding_state))
}

pub(crate) fn resolve_remembered_alias_target_from_binding_state(
    rub_home: &Path,
    normalized_alias: &str,
    registry: RememberedBindingAliasRegistryData,
    binding_state: &crate::binding_ctl::BindingResolutionState,
) -> Result<
    (
        RememberedBindingAliasRecord,
        rub_core::model::RememberedBindingAliasTarget,
    ),
    RubError,
> {
    let record = registry
        .aliases
        .iter()
        .find(|record| record.alias == normalized_alias)
        .cloned()
        .ok_or_else(|| remembered_alias_not_found_error(rub_home, normalized_alias))?;
    let target = resolve_binding_target_from_state(&record.binding_alias, binding_state)?;
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
    let (previous_binding_alias, updated) = mutate_binding_and_remembered_alias_registries(
        rub_home,
        |binding_registry, remembered_registry| {
            ensure_binding_target_exists_in_registry(rub_home, binding_registry, &binding_alias)?;
            let record = remembered_registry
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
            Ok((previous_binding_alias, record.clone()))
        },
    )?;

    let (_, binding_state) = load_remembered_alias_resolution_state(rub_home)?;
    let target = resolve_binding_target_from_state(&binding_alias, &binding_state)?;
    let mut projection = json!({
        "subject": remembered_alias_subject(rub_home, &alias),
        "result": {
            "action": "rebind",
            "previous_binding_alias": previous_binding_alias,
            "remembered_alias": updated,
            "target": target,
        }
    });
    if let Some(error) = binding_state
        .live_snapshot_error()
        .map(project_live_registry_error)
    {
        projection["result"]["live_registry_error"] = error;
    }
    Ok(projection)
}

pub(crate) fn forget_remembered_alias(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    mutate_remembered_alias_registry(rub_home, |registry| {
        let original_len = registry.aliases.len();
        registry.aliases.retain(|record| record.alias != alias);
        if registry.aliases.len() == original_len {
            return Err(remembered_alias_not_found_error(rub_home, &alias));
        }
        Ok(())
    })?;

    Ok(json!({
        "subject": remembered_alias_subject(rub_home, &alias),
        "result": {
            "removed_alias": alias,
        }
    }))
}

fn load_remembered_alias_resolution_state(
    rub_home: &Path,
) -> Result<(RememberedBindingAliasRegistryData, BindingResolutionState), RubError> {
    let (binding_registry, remembered_registry) =
        read_binding_and_remembered_alias_registries(rub_home)?;
    let binding_state = load_binding_resolution_state_from_registry(rub_home, binding_registry)?;
    Ok((remembered_registry, binding_state))
}

#[cfg(test)]
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

fn ensure_binding_target_exists_in_registry(
    rub_home: &Path,
    registry: &BindingRegistryData,
    binding_alias: &str,
) -> Result<(), RubError> {
    let normalized = normalize_binding_alias(binding_alias)?;
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
