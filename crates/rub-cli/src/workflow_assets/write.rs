use std::path::{Path, PathBuf};
use std::time::Instant;

use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::{FileCommitOutcome, atomic_write_bytes, atomic_write_bytes_until};
use serde_json::{Value, json};

use crate::local_asset_paths::LocalAssetPathIdentity;
use crate::persisted_artifacts::annotate_local_persisted_artifact;

use super::local_workflow_asset_path_state;

pub(super) struct PendingAssetWrite {
    pub(super) path: PathBuf,
    authority: LocalAssetPathIdentity,
    pub(super) contents: Vec<u8>,
    pub(super) artifact: Value,
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

pub(super) fn commit_asset_writes(writes: Vec<PendingAssetWrite>) -> Result<Vec<Value>, RubError> {
    commit_asset_writes_with_deadline(writes, None)
}

pub(super) fn commit_asset_writes_until(
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
                format!("Duplicate workflow export path {}", write.path.display()),
                &write.path,
                Some(write.authority.authority_path()),
                "workflow_asset_duplicate_export_path",
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
                            "Failed to write workflow asset {}: {error}",
                            authority_path.display()
                        ),
                        &write.path,
                        Some(authority_path),
                        "workflow_asset_write_failed",
                        rollback_errors,
                    ));
                }
            };
        let mut artifact = write.artifact;
        annotate_local_persisted_artifact(
            &mut artifact,
            "cli.history_export_asset_persistence",
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
                "Cannot safely overwrite workflow asset {} because the existing file is not readable for rollback: {error}",
                path.display()
            ),
            json!({
                "path": path.display().to_string(),
                "path_state": local_workflow_asset_path_state(
                    "cli.workflow_assets.write.path",
                    "workflow_asset_reference",
                ),
                "reason": "workflow_asset_unreadable_for_rollback",
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
                "Failed to roll back workflow asset {}: {error}",
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

#[cfg(test)]
pub(super) fn remove_newly_created_asset_if_matches(
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

#[cfg(not(test))]
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
            json!(local_workflow_asset_path_state(
                "cli.workflow_assets.write.path",
                "workflow_asset_reference",
            )),
        ),
        ("reason".to_string(), json!(reason)),
    ]);
    if let Some(authority_path) = authority_path
        && (authority_path != path || reason == "workflow_asset_duplicate_export_path")
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

pub(super) fn pending_asset_write(
    path: PathBuf,
    contents: Vec<u8>,
    mut artifact: Value,
) -> Result<PendingAssetWrite, RubError> {
    let authority = LocalAssetPathIdentity::resolve(&path).map_err(|error| {
        asset_write_error_at_path(
            format!(
                "Failed to resolve workflow export path {} to a stable local asset authority: {error}",
                path.display()
            ),
            &path,
            None,
            "workflow_asset_path_resolution_failed",
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
