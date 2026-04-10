use std::path::Path;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::PathReferenceState;
use rub_daemon::rub_paths::RubPaths;
use serde_json::json;

pub(crate) fn local_workflow_asset_path_state(
    path_authority: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_asset_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: "cli_workflow_asset_registry".to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub fn list_workflows(rub_home: &Path) -> Result<serde_json::Value, RubError> {
    let paths = RubPaths::new(rub_home);
    let directory = paths.workflows_dir();
    let mut workflows = Vec::new();

    if directory.exists() {
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            workflow_listing_directory_error(
                ErrorCode::InvalidInput,
                format!(
                    "Failed to read workflow directory {}: {error}",
                    directory.display()
                ),
                &directory,
                "workflow_directory_read_failed",
            )
        })?;

        for entry in entries {
            let entry = entry.map_err(|error| {
                workflow_listing_directory_error(
                    ErrorCode::InvalidInput,
                    format!(
                        "Failed to enumerate workflow directory {}: {error}",
                        directory.display()
                    ),
                    &directory,
                    "workflow_directory_enumeration_failed",
                )
            })?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let metadata = entry.metadata().map_err(|error| {
                workflow_listing_path_error(
                    ErrorCode::InvalidInput,
                    format!("Failed to stat workflow file {}: {error}", path.display()),
                    &path,
                    "workflow_asset_stat_failed",
                )
            })?;
            let Some(name) = workflow_name_from_path(&path) else {
                continue;
            };
            workflows.push(json!({
                "name": name,
                "path": path.display().to_string(),
                "path_state": local_workflow_asset_path_state(
                    "cli.workflow_assets.item.path",
                    "workflow_asset_reference",
                ),
                "size_bytes": metadata.len(),
            }));
        }
    }

    workflows.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["name"].as_str().unwrap_or_default())
    });

    Ok(json!({
        "subject": {
            "kind": "workflow_asset_registry",
            "directory": directory.display().to_string(),
            "directory_state": local_workflow_asset_path_state(
                "cli.workflow_assets.directory",
                "workflow_asset_directory",
            ),
        },
        "result": {
            "items": workflows,
        }
    }))
}

fn workflow_listing_directory_error(
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
            "directory_state": local_workflow_asset_path_state(
                "cli.workflow_assets.directory",
                "workflow_asset_registry_directory",
            ),
            "reason": reason,
        }),
    )
}

fn workflow_listing_path_error(
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
            "path_state": local_workflow_asset_path_state(
                "cli.workflow_assets.item.path",
                "workflow_asset_reference",
            ),
            "reason": reason,
        }),
    )
}

pub(super) fn workflow_name_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_string)
}
