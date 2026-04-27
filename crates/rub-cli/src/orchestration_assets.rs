use crate::commands::{Commands, EffectiveCli, OrchestrationSubcommand};
use crate::local_asset_paths::LocalAssetPathIdentity;
use crate::persisted_artifacts::annotate_local_persisted_artifact;
use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::{FileCommitOutcome, atomic_write_bytes, atomic_write_bytes_until};
use rub_core::model::PathReferenceState;
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Instant;

const ORCHESTRATION_EXPORT_PERSISTENCE_PHASE: &str = "post_commit_orchestration_export_persistence";

struct PendingAssetWrite {
    path: PathBuf,
    authority: LocalAssetPathIdentity,
    contents: Vec<u8>,
    artifact: Value,
}

struct CommittedAssetWrite {
    path: PathBuf,
    authority: LocalAssetPathIdentity,
    previous_state: PreviousAssetState,
    committed_contents: Vec<u8>,
}

enum PreviousAssetState {
    Absent,
    Readable(Vec<u8>),
}

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
        pending_writes.push(pending_asset_write(
            path.clone(),
            serialized.clone(),
            artifact,
        )?);
    }

    if let Some(output_path) = output {
        let path = std::path::Path::new(output_path).to_path_buf();
        let mut artifact = json!({
            "kind": "orchestration_export_file",
            "role": "output",
            "path": path.display().to_string(),
            "format": "orchestration",
        });
        if let Some(identity) = rule_identity_projection {
            artifact["source_rule_identity"] = identity;
        }
        pending_writes.push(pending_asset_write(path.clone(), serialized, artifact)?);
    }

    if !pending_writes.is_empty() {
        let committed = match deadline {
            Some((deadline, timeout_ms)) => commit_asset_writes_until(
                pending_writes,
                deadline,
                timeout_ms,
                ORCHESTRATION_EXPORT_PERSISTENCE_PHASE,
            )?,
            None => commit_asset_writes(pending_writes)?,
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

fn commit_asset_writes(writes: Vec<PendingAssetWrite>) -> Result<Vec<Value>, RubError> {
    commit_asset_writes_with_deadline(writes, None)
}

fn commit_asset_writes_until(
    writes: Vec<PendingAssetWrite>,
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<Vec<Value>, RubError> {
    commit_asset_writes_with_deadline(writes, Some((deadline, timeout_ms, phase)))
}

fn commit_asset_writes_with_deadline(
    writes: Vec<PendingAssetWrite>,
    deadline: Option<(Instant, u64, &'static str)>,
) -> Result<Vec<Value>, RubError> {
    let mut committed = Vec::new();
    let mut artifacts = Vec::new();

    for write in writes {
        if committed.iter().any(|existing: &CommittedAssetWrite| {
            existing.authority.conflicts_with(&write.authority)
        }) {
            return Err(asset_write_error_at_path(
                format!(
                    "Duplicate orchestration export path {}",
                    write.path.display()
                ),
                &write.path,
                Some(write.authority.authority_path()),
                "orchestration_asset_duplicate_export_path",
                rollback_asset_writes(&committed).err(),
            ));
        }

        let authority_path = write.authority.authority_path();
        let previous_state = match read_previous_asset_state(authority_path) {
            Ok(state) => state,
            Err(error) => {
                return Err(asset_write_error_from_source(
                    error,
                    rollback_asset_writes(&committed).err(),
                ));
            }
        };
        let commit_outcome =
            match commit_asset_write(authority_path, &write.contents, deadline.as_ref()) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let rollback_errors = rollback_asset_writes(&committed).err();
                    if is_timeout_error(&error) {
                        return Err(asset_write_error_from_source(error, rollback_errors));
                    }
                    return Err(asset_write_error_at_path(
                        format!(
                            "Failed to write orchestration asset {}: {error}",
                            authority_path.display()
                        ),
                        &write.path,
                        Some(authority_path),
                        "orchestration_asset_write_failed",
                        rollback_errors,
                    ));
                }
            };
        let mut artifact = write.artifact;
        annotate_local_persisted_artifact(
            &mut artifact,
            "cli.orchestration_export_asset_persistence",
            commit_outcome,
        );

        committed.push(CommittedAssetWrite {
            path: write.path,
            authority: write.authority,
            previous_state,
            committed_contents: write.contents,
        });
        artifacts.push(artifact);
    }

    Ok(artifacts)
}

fn commit_asset_write(
    authority_path: &Path,
    contents: &[u8],
    deadline: Option<&(Instant, u64, &'static str)>,
) -> Result<FileCommitOutcome, RubError> {
    match deadline {
        Some((deadline, timeout_ms, phase)) => {
            crate::timeout_budget::ensure_remaining_budget(*deadline, *timeout_ms, phase)?;
            atomic_write_bytes_until(authority_path, contents, 0o600, *deadline)
                .map_err(|error| timed_asset_write_error(error, *timeout_ms, phase))
        }
        None => atomic_write_bytes(authority_path, contents, 0o600).map_err(RubError::from),
    }
}

fn timed_asset_write_error(
    error: std::io::Error,
    timeout_ms: u64,
    phase: &'static str,
) -> RubError {
    if error.kind() == std::io::ErrorKind::TimedOut {
        crate::main_support::command_timeout_error(timeout_ms, phase)
    } else {
        RubError::from(error)
    }
}

fn is_timeout_error(error: &RubError) -> bool {
    matches!(error, RubError::Domain(envelope) if envelope.code == ErrorCode::IpcTimeout)
}

fn read_previous_asset_state(path: &Path) -> Result<PreviousAssetState, RubError> {
    match std::fs::read(path) {
        Ok(previous) => Ok(PreviousAssetState::Readable(previous)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PreviousAssetState::Absent)
        }
        Err(error) => Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Cannot safely overwrite orchestration asset {} because the existing file is not readable for rollback: {error}",
                path.display()
            ),
            json!({
                "path": path.display().to_string(),
                "path_state": local_orchestration_asset_path_state(
                    "cli.orchestration_assets.write.path",
                    "orchestration_asset_reference",
                ),
                "reason": "orchestration_asset_unreadable_for_rollback",
            }),
        )),
    }
}

fn rollback_asset_writes(committed: &[CommittedAssetWrite]) -> Result<(), Vec<String>> {
    let mut rollback_errors = Vec::new();
    for write in committed.iter().rev() {
        let rollback_result = match &write.previous_state {
            PreviousAssetState::Readable(previous) => {
                atomic_write_bytes(write.authority.authority_path(), previous, 0o600).map(|_| ())
            }
            PreviousAssetState::Absent => remove_newly_created_asset_if_matches(
                write.authority.authority_path(),
                &write.committed_contents,
            ),
        };
        if let Err(error) = rollback_result {
            rollback_errors.push(format!(
                "Failed to roll back orchestration asset {}: {error}",
                write.path.display()
            ));
        }
    }
    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(rollback_errors)
    }
}

fn remove_newly_created_asset_if_matches(
    path: &Path,
    expected_contents: &[u8],
) -> std::io::Result<()> {
    match std::fs::read(path) {
        Ok(current) if current == expected_contents => std::fs::remove_file(path),
        Ok(_) => Err(std::io::Error::other(format!(
            "rollback target {} no longer matches the file published by this export attempt",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn asset_write_error_at_path(
    message: String,
    path: &Path,
    authority_path: Option<&Path>,
    reason: &str,
    rollback_errors: Option<Vec<String>>,
) -> RubError {
    let mut context = serde_json::Map::from_iter([
        ("path".to_string(), json!(path.display().to_string())),
        (
            "path_state".to_string(),
            json!(local_orchestration_asset_path_state(
                "cli.orchestration_assets.write.path",
                "orchestration_asset_reference",
            )),
        ),
        ("reason".to_string(), json!(reason)),
    ]);
    if let Some(authority_path) = authority_path
        && authority_path != path
    {
        context.insert(
            "authority_path".to_string(),
            json!(authority_path.display().to_string()),
        );
    }
    if let Some(errors) = rollback_errors {
        context.insert("rollback_failed".to_string(), json!(true));
        context.insert("rollback_errors".to_string(), json!(errors));
    }
    RubError::domain_with_context(ErrorCode::InvalidInput, message, Value::Object(context))
}

fn asset_write_error_from_source(
    error: RubError,
    rollback_errors: Option<Vec<String>>,
) -> RubError {
    let envelope = error.into_envelope();
    let mut context = envelope
        .context
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(errors) = rollback_errors {
        context.insert("rollback_failed".to_string(), json!(true));
        context.insert("rollback_errors".to_string(), json!(errors));
    }
    if context.is_empty() {
        RubError::domain(envelope.code, envelope.message)
    } else {
        RubError::domain_with_context(envelope.code, envelope.message, Value::Object(context))
    }
}

fn asset_name_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_string)
}

fn pending_asset_write(
    path: PathBuf,
    contents: Vec<u8>,
    mut artifact: Value,
) -> Result<PendingAssetWrite, RubError> {
    let authority = LocalAssetPathIdentity::resolve(&path).map_err(|error| {
        asset_write_error_at_path(
            format!(
                "Failed to resolve orchestration export path {} to a stable local asset authority: {error}",
                path.display()
            ),
            &path,
            None,
            "orchestration_asset_path_resolution_failed",
            None,
        )
    })?;
    if let Some(object) = artifact.as_object_mut() {
        object.insert(
            "path".to_string(),
            json!(authority.authority_path().display().to_string()),
        );
    }
    Ok(PendingAssetWrite {
        path,
        authority,
        contents,
        artifact,
    })
}

#[cfg(test)]
mod tests;
