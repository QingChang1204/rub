mod capture;
mod projection;
mod registry;

#[cfg(test)]
pub(crate) use self::capture::build_binding_record_from_candidate;
pub(crate) use self::capture::{BindingWriteMode, capture_binding_alias};
pub(crate) use self::projection::{
    binding_alias_not_found_error, load_binding_resolution_state, load_live_registry_snapshot,
    project_binding_inspect, project_binding_list, project_live_status, resolve_binding_target,
    resolve_binding_target_from_state,
};
pub(crate) use self::registry::{
    normalize_binding_alias, read_binding_registry, write_binding_registry,
};

use crate::commands::{BindingSubcommand, EffectiveCli};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{CommandResult, PathReferenceState};
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub(crate) async fn handle_binding_command(
    cli: &EffectiveCli,
    subcommand: &BindingSubcommand,
) -> Result<(), RubError> {
    let data = match subcommand {
        BindingSubcommand::List => project_binding_list(&cli.rub_home)?,
        BindingSubcommand::Aliases => {
            crate::binding_memory_ctl::project_remembered_alias_list(&cli.rub_home)?
        }
        BindingSubcommand::Capture { alias, auth_input } => {
            capture_binding_alias(
                cli,
                alias,
                BindingWriteMode::Capture {
                    auth_input: *auth_input,
                },
            )
            .await?
        }
        BindingSubcommand::BindCurrent { alias } => {
            capture_binding_alias(cli, alias, BindingWriteMode::BindCurrent).await?
        }
        BindingSubcommand::Inspect { alias } => project_binding_inspect(&cli.rub_home, alias)?,
        BindingSubcommand::Rename { alias, new_alias } => {
            rename_binding_alias(&cli.rub_home, alias, new_alias)?
        }
        BindingSubcommand::Remove { alias } => remove_binding_alias(&cli.rub_home, alias)?,
        BindingSubcommand::Remember {
            alias,
            binding_alias,
            kind,
        } => crate::binding_memory_ctl::remember_binding_alias(
            &cli.rub_home,
            alias,
            binding_alias,
            *kind,
        )?,
        BindingSubcommand::Resolve { alias } => {
            crate::binding_memory_ctl::resolve_remembered_alias(&cli.rub_home, alias)?
        }
        BindingSubcommand::Rebind {
            alias,
            binding_alias,
            kind,
        } => crate::binding_memory_ctl::rebind_remembered_alias(
            &cli.rub_home,
            alias,
            binding_alias,
            *kind,
        )?,
        BindingSubcommand::Forget { alias } => {
            crate::binding_memory_ctl::forget_remembered_alias(&cli.rub_home, alias)?
        }
    };

    let result = CommandResult::success("binding", &cli.session, Uuid::now_v7().to_string(), data);
    let output = if cli.json_pretty {
        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    };
    println!("{output}");
    Ok(())
}

pub(crate) fn binding_registry_subject(rub_home: &Path) -> Value {
    let paths = RubPaths::new(rub_home);
    json!({
        "kind": "binding_registry",
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": binding_path_state(
            "cli.binding.subject.rub_home",
            "cli_binding_registry",
            "rub_home_directory",
        ),
        "registry_path": paths.bindings_path().display().to_string(),
        "registry_path_state": binding_path_state(
            "cli.binding.subject.registry_path",
            "cli_binding_registry",
            "binding_registry_file",
        ),
        "lock_path": paths.bindings_lock_path().display().to_string(),
        "lock_path_state": binding_path_state(
            "cli.binding.subject.lock_path",
            "cli_binding_registry",
            "binding_registry_lock",
        ),
    })
}

pub(crate) fn binding_alias_subject(rub_home: &Path, alias: &str) -> Value {
    json!({
        "kind": "binding_alias",
        "alias": alias,
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": binding_path_state(
            "cli.binding.subject.rub_home",
            "cli_binding_registry",
            "rub_home_directory",
        ),
    })
}

pub(crate) fn binding_path_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_product_state_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

fn rename_binding_alias(rub_home: &Path, alias: &str, new_alias: &str) -> Result<Value, RubError> {
    let alias = normalize_binding_alias(alias)?;
    let new_alias = normalize_binding_alias(new_alias)?;
    let dependent_aliases =
        crate::binding_memory_ctl::remembered_aliases_referencing_binding(rub_home, &alias)?;
    if !dependent_aliases.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Binding alias '{alias}' is referenced by remembered aliases and cannot be renamed"
            ),
            json!({
                "alias": alias,
                "remembered_aliases": dependent_aliases,
                "reason": "binding_alias_referenced_by_remembered_aliases",
            }),
        ));
    }
    let mut registry = read_binding_registry(rub_home)?;

    if registry
        .bindings
        .iter()
        .any(|binding| binding.alias == new_alias)
    {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Binding alias already exists: {new_alias}"),
            json!({
                "alias": alias,
                "new_alias": new_alias,
                "reason": "binding_alias_already_exists",
            }),
        ));
    }

    let binding = registry
        .bindings
        .iter_mut()
        .find(|binding| binding.alias == alias)
        .ok_or_else(|| binding_alias_not_found_error(rub_home, &alias))?;
    binding.alias = new_alias.clone();
    write_binding_registry(rub_home, &registry)?;

    Ok(json!({
        "subject": binding_alias_subject(rub_home, &new_alias),
        "result": {
            "previous_alias": alias,
            "alias": new_alias,
        }
    }))
}

fn remove_binding_alias(rub_home: &Path, alias: &str) -> Result<Value, RubError> {
    let normalized = normalize_binding_alias(alias)?;
    let dependent_aliases =
        crate::binding_memory_ctl::remembered_aliases_referencing_binding(rub_home, &normalized)?;
    if !dependent_aliases.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Binding alias '{normalized}' is referenced by remembered aliases and cannot be removed"
            ),
            json!({
                "alias": normalized,
                "remembered_aliases": dependent_aliases,
                "reason": "binding_alias_referenced_by_remembered_aliases",
            }),
        ));
    }
    let mut registry = read_binding_registry(rub_home)?;
    let original_len = registry.bindings.len();
    registry
        .bindings
        .retain(|binding| binding.alias != normalized);
    if registry.bindings.len() == original_len {
        return Err(binding_alias_not_found_error(rub_home, &normalized));
    }
    write_binding_registry(rub_home, &registry)?;

    Ok(json!({
        "subject": binding_alias_subject(rub_home, &normalized),
        "result": {
            "removed_alias": normalized,
        }
    }))
}

#[cfg(test)]
mod tests;
