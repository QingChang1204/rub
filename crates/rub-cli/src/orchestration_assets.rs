use crate::commands::{Commands, EffectiveCli, OrchestrationSubcommand};
use crate::local_asset_writes::{
    LocalAssetWriteConfig, commit_local_asset_writes, commit_local_asset_writes_until,
    pending_local_asset_write,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::PathReferenceState;
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;

const ORCHESTRATION_EXPORT_PERSISTENCE_PHASE: &str = "post_commit_orchestration_export_persistence";

const ORCHESTRATION_ASSET_WRITE_CONFIG: LocalAssetWriteConfig = LocalAssetWriteConfig {
    persistence_authority: "cli.orchestration_export_asset_persistence",
    duplicate_path_label: "orchestration export path",
    write_path_label: "orchestration asset",
    resolution_path_label: "orchestration export path",
    duplicate_reason: "orchestration_asset_duplicate_export_path",
    write_failed_reason: "orchestration_asset_write_failed",
    unreadable_reason: "orchestration_asset_unreadable_for_rollback",
    path_resolution_reason: "orchestration_asset_path_resolution_failed",
    path_authority: "cli.orchestration_assets.write.path",
    path_kind: "orchestration_asset_reference",
    path_state: local_orchestration_asset_path_state,
    include_duplicate_authority_path: false,
};

pub(crate) use rub_daemon::orchestration_assets::{
    normalize_orchestration_name, resolve_named_orchestration_path,
};

fn local_orchestration_asset_path_state(
    path_authority: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_asset_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: "cli_orchestration_asset_registry".to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub fn list_orchestrations(rub_home: &Path) -> Result<Value, RubError> {
    let paths = RubPaths::new(rub_home);
    let directory = paths.orchestrations_dir();
    let mut orchestrations = Vec::new();

    if directory.exists() {
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            orchestration_listing_directory_error(
                ErrorCode::InvalidInput,
                format!(
                    "Failed to read orchestration directory {}: {error}",
                    directory.display()
                ),
                &directory,
                "orchestration_directory_read_failed",
            )
        })?;

        for entry in entries {
            let entry = entry.map_err(|error| {
                orchestration_listing_directory_error(
                    ErrorCode::InvalidInput,
                    format!(
                        "Failed to enumerate orchestration directory {}: {error}",
                        directory.display()
                    ),
                    &directory,
                    "orchestration_directory_enumeration_failed",
                )
            })?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let metadata = entry.metadata().map_err(|error| {
                orchestration_listing_path_error(
                    ErrorCode::InvalidInput,
                    format!(
                        "Failed to stat orchestration asset {}: {error}",
                        path.display()
                    ),
                    &path,
                    "orchestration_asset_stat_failed",
                )
            })?;
            let Some(name) = asset_name_from_path(&path) else {
                continue;
            };
            orchestrations.push(json!({
                "name": name,
                "path": path.display().to_string(),
                "path_state": local_orchestration_asset_path_state(
                    "cli.orchestration_assets.item.path",
                    "orchestration_asset_reference",
                ),
                "size_bytes": metadata.len(),
            }));
        }
    }

    orchestrations.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["name"].as_str().unwrap_or_default())
    });

    Ok(json!({
        "subject": {
            "kind": "orchestration_asset_registry",
            "directory": directory.display().to_string(),
            "directory_state": local_orchestration_asset_path_state(
                "cli.orchestration_assets.directory",
                "orchestration_asset_directory",
            ),
        },
        "result": {
            "items": orchestrations,
        }
    }))
}

fn orchestration_listing_directory_error(
    code: ErrorCode,
    message: String,
    directory: &Path,
    reason: &str,
) -> RubError {
    RubError::domain_with_context(
        code,
        message,
        json!({
            "directory": directory.display().to_string(),
            "directory_state": local_orchestration_asset_path_state(
                "cli.orchestration_assets.directory",
                "orchestration_asset_registry_directory",
            ),
            "reason": reason,
        }),
    )
}

fn orchestration_listing_path_error(
    code: ErrorCode,
    message: String,
    path: &Path,
    reason: &str,
) -> RubError {
    RubError::domain_with_context(
        code,
        message,
        json!({
            "path": path.display().to_string(),
            "path_state": local_orchestration_asset_path_state(
                "cli.orchestration_assets.item.path",
                "orchestration_asset_reference",
            ),
            "reason": reason,
        }),
    )
}

#[cfg(test)]
pub fn persist_orchestration_export_asset(
    cli: &EffectiveCli,
    data: &mut Value,
) -> Result<(), RubError> {
    persist_orchestration_export_asset_with_deadline(cli, data, None)
}

pub fn persist_orchestration_export_asset_until(
    cli: &EffectiveCli,
    data: &mut Value,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<(), RubError> {
    persist_orchestration_export_asset_with_deadline(cli, data, Some((deadline, timeout_ms)))
}

fn persist_orchestration_export_asset_with_deadline(
    cli: &EffectiveCli,
    data: &mut Value,
    deadline: Option<(Instant, u64)>,
) -> Result<(), RubError> {
    let Commands::Orchestration { subcommand } = &cli.command else {
        return Ok(());
    };
    let OrchestrationSubcommand::Export {
        save_as, output, ..
    } = subcommand
    else {
        return Ok(());
    };
    if save_as.is_none() && output.is_none() {
        return Ok(());
    }

    let object = data.as_object_mut().ok_or_else(|| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            "orchestration export response must be a JSON object",
        )
    })?;
    let result = object
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::IpcProtocolError,
                "orchestration export response missing result object",
            )
        })?;
    let spec = result.get("spec").cloned().ok_or_else(|| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            "orchestration export response missing canonical spec",
        )
    })?;
    let rule_identity_projection = result.get("rule_identity_projection").cloned();
    let serialized = serde_json::to_vec_pretty(&spec).map_err(RubError::from)?;
    let mut persisted_artifacts = result
        .get("persisted_artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pending_writes = Vec::new();

    if let Some(name) = save_as {
        let path = resolve_named_orchestration_path(&cli.rub_home, name)?;
        let mut artifact = json!({
            "kind": "orchestration_asset",
            "role": "output",
            "path": path.display().to_string(),
            "asset_name": normalize_orchestration_name(name)?,
        });
        if let Some(identity) = rule_identity_projection.clone() {
            artifact["source_rule_identity"] = identity;
        }
        pending_writes.push(pending_local_asset_write(
            path.clone(),
            serialized.clone(),
            artifact,
            &ORCHESTRATION_ASSET_WRITE_CONFIG,
        )?);
    }

    if let Some(output_path) = output {
        let path = Path::new(output_path).to_path_buf();
        let mut artifact = json!({
            "kind": "orchestration_export_file",
            "role": "output",
            "path": path.display().to_string(),
            "format": "orchestration",
        });
        if let Some(identity) = rule_identity_projection {
            artifact["source_rule_identity"] = identity;
        }
        pending_writes.push(pending_local_asset_write(
            path.clone(),
            serialized,
            artifact,
            &ORCHESTRATION_ASSET_WRITE_CONFIG,
        )?);
    }

    if !pending_writes.is_empty() {
        let committed = match deadline {
            Some((deadline, timeout_ms)) => commit_local_asset_writes_until(
                pending_writes,
                &ORCHESTRATION_ASSET_WRITE_CONFIG,
                deadline,
                timeout_ms,
                ORCHESTRATION_EXPORT_PERSISTENCE_PHASE,
            )?,
            None => commit_local_asset_writes(pending_writes, &ORCHESTRATION_ASSET_WRITE_CONFIG)?,
        };
        persisted_artifacts.extend(committed);
    }

    if !persisted_artifacts.is_empty() {
        result.insert(
            "persisted_artifacts".to_string(),
            Value::Array(persisted_artifacts),
        );
    }

    Ok(())
}

fn asset_name_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_string)
}

#[cfg(test)]
fn commit_asset_writes(
    writes: Vec<crate::local_asset_writes::PendingLocalAssetWrite>,
) -> Result<Vec<Value>, RubError> {
    commit_local_asset_writes(writes, &ORCHESTRATION_ASSET_WRITE_CONFIG)
}

#[cfg(test)]
fn pending_asset_write(
    path: std::path::PathBuf,
    contents: Vec<u8>,
    artifact: Value,
) -> Result<crate::local_asset_writes::PendingLocalAssetWrite, RubError> {
    pending_local_asset_write(path, contents, artifact, &ORCHESTRATION_ASSET_WRITE_CONFIG)
}

#[cfg(test)]
use crate::local_asset_writes::remove_newly_created_asset_if_matches;

#[cfg(test)]
mod tests;
