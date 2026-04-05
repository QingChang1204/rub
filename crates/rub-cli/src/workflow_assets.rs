use crate::commands::{Commands, EffectiveCli};
use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes;
use rub_daemon::rub_paths::RubPaths;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

struct PendingAssetWrite {
    path: PathBuf,
    contents: Vec<u8>,
    artifact: Value,
}

struct CommittedAssetWrite {
    path: PathBuf,
    previous_state: PreviousAssetState,
    committed_contents: Vec<u8>,
}

enum PreviousAssetState {
    Absent,
    Readable(Vec<u8>),
}

pub(crate) use rub_daemon::workflow_assets::{
    normalize_workflow_name, resolve_named_workflow_path,
};

pub fn list_workflows(rub_home: &Path) -> Result<Value, RubError> {
    let paths = RubPaths::new(rub_home);
    let directory = paths.workflows_dir();
    let mut workflows = Vec::new();

    if directory.exists() {
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Failed to read workflow directory {}: {error}",
                    directory.display()
                ),
            )
        })?;

        for entry in entries {
            let entry = entry.map_err(|error| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!(
                        "Failed to enumerate workflow directory {}: {error}",
                        directory.display()
                    ),
                )
            })?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let metadata = entry.metadata().map_err(|error| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Failed to stat workflow file {}: {error}", path.display()),
                )
            })?;
            let Some(name) = workflow_name_from_path(&path) else {
                continue;
            };
            workflows.push(json!({
                "name": name,
                "path": path.display().to_string(),
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
        },
        "result": {
            "items": workflows,
        }
    }))
}

pub fn persist_history_export_asset(cli: &EffectiveCli, data: &mut Value) -> Result<(), RubError> {
    let Commands::History {
        export_pipe,
        export_script,
        save_as,
        output,
        ..
    } = &cli.command
    else {
        return Ok(());
    };

    if !(*export_pipe || *export_script) {
        return Ok(());
    }
    if save_as.is_none() && output.is_none() {
        return Ok(());
    }

    let object = data.as_object_mut().ok_or_else(|| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            "history export response must be a JSON object",
        )
    })?;
    let result = object
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::IpcProtocolError,
                "history export response missing result object",
            )
        })?;
    let format = result
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RubError::domain(ErrorCode::IpcProtocolError, "history export missing format")
        })?;
    let mut persisted_artifacts = result
        .get("persisted_artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pending_writes = Vec::new();

    if let Some(name) = save_as {
        let path = resolve_named_workflow_path(&cli.rub_home, name)?;
        let serialized = render_export_asset(result, true)?;
        pending_writes.push(PendingAssetWrite {
            path: path.clone(),
            contents: serialized,
            artifact: json!({
            "kind": "workflow_asset",
            "role": "output",
            "path": path.display().to_string(),
            "workflow_name": normalize_workflow_name(name)?,
            }),
        });
    }

    if let Some(output_path) = output {
        let path = resolve_cli_path(output_path);
        let serialized = render_export_asset(result, false)?;
        pending_writes.push(PendingAssetWrite {
            path: path.clone(),
            contents: serialized,
            artifact: json!({
            "kind": "history_export_file",
            "role": "output",
            "path": path.display().to_string(),
            "format": format,
            }),
        });
    }

    if !pending_writes.is_empty() {
        persisted_artifacts.extend(commit_asset_writes(pending_writes)?);
    }

    if !persisted_artifacts.is_empty() {
        result.insert(
            "persisted_artifacts".to_string(),
            Value::Array(persisted_artifacts),
        );
    }

    Ok(())
}

fn render_export_asset(
    result: &serde_json::Map<String, Value>,
    for_named_workflow: bool,
) -> Result<Vec<u8>, RubError> {
    let format = result
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RubError::domain(ErrorCode::IpcProtocolError, "history export missing format")
        })?;
    match format {
        "pipe" => {
            let steps = result.get("entries").cloned().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::IpcProtocolError,
                    "history export pipe response missing entries",
                )
            })?;
            serde_json::to_vec_pretty(&json!({ "steps": steps })).map_err(RubError::from)
        }
        "script" => {
            if for_named_workflow {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    "--save-as is only supported with --export-pipe",
                ));
            }
            let script = result
                .get("export")
                .and_then(Value::as_object)
                .and_then(|export| export.get("content"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::IpcProtocolError,
                        "history export script response missing export.content",
                    )
                })?;
            Ok(script.as_bytes().to_vec())
        }
        other => Err(RubError::domain(
            ErrorCode::IpcProtocolError,
            format!("unknown history export format '{other}'"),
        )),
    }
}

fn commit_asset_writes(writes: Vec<PendingAssetWrite>) -> Result<Vec<Value>, RubError> {
    let mut committed = Vec::new();
    let mut artifacts = Vec::new();

    for write in writes {
        if committed
            .iter()
            .any(|existing: &CommittedAssetWrite| existing.path == write.path)
        {
            return Err(asset_write_error(
                format!("Duplicate workflow export path {}", write.path.display()),
                rollback_asset_writes(&committed).err(),
            ));
        }

        let previous_state = match read_previous_asset_state(&write.path) {
            Ok(state) => state,
            Err(error) => {
                return Err(asset_write_error(
                    error.to_string(),
                    rollback_asset_writes(&committed).err(),
                ));
            }
        };
        let commit_outcome = match atomic_write_bytes(&write.path, &write.contents, 0o600) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(asset_write_error(
                    format!(
                        "Failed to write workflow asset {}: {error}",
                        write.path.display()
                    ),
                    rollback_asset_writes(&committed).err(),
                ));
            }
        };
        let mut artifact = write.artifact;
        if !commit_outcome.durability_confirmed()
            && let Some(object) = artifact.as_object_mut()
        {
            object.insert("durability_confirmed".to_string(), Value::Bool(false));
        }

        committed.push(CommittedAssetWrite {
            path: write.path,
            previous_state,
            committed_contents: write.contents,
        });
        artifacts.push(artifact);
    }

    Ok(artifacts)
}

fn read_previous_asset_state(path: &Path) -> Result<PreviousAssetState, RubError> {
    match std::fs::read(path) {
        Ok(previous) => Ok(PreviousAssetState::Readable(previous)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PreviousAssetState::Absent)
        }
        Err(error) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Cannot safely overwrite workflow asset {} because the existing file is not readable for rollback: {error}",
                path.display()
            ),
        )),
    }
}

fn rollback_asset_writes(committed: &[CommittedAssetWrite]) -> Result<(), Vec<String>> {
    let mut rollback_errors = Vec::new();
    for write in committed.iter().rev() {
        let rollback_result = match &write.previous_state {
            PreviousAssetState::Readable(previous) => {
                atomic_write_bytes(&write.path, previous, 0o600).map(|_| ())
            }
            PreviousAssetState::Absent => {
                remove_newly_created_asset_if_matches(&write.path, &write.committed_contents)
            }
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

fn asset_write_error(message: String, rollback_errors: Option<Vec<String>>) -> RubError {
    match rollback_errors {
        Some(errors) => RubError::domain_with_context(
            ErrorCode::InvalidInput,
            message,
            json!({
                "rollback_failed": true,
                "rollback_errors": errors,
            }),
        ),
        None => RubError::domain(ErrorCode::InvalidInput, message),
    }
}

fn workflow_name_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_string)
}

fn resolve_cli_path(path: &str) -> PathBuf {
    let raw = Path::new(path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PendingAssetWrite, commit_asset_writes, list_workflows, persist_history_export_asset,
        remove_newly_created_asset_if_matches, resolve_named_workflow_path,
    };
    use crate::commands::{Commands, EffectiveCli, RequestedLaunchPolicy};
    use rub_core::error::ErrorCode;
    use std::path::{Path, PathBuf};

    fn cli_with(command: Commands, rub_home: PathBuf) -> EffectiveCli {
        EffectiveCli {
            session: "default".to_string(),
            session_id: None,
            rub_home,
            timeout: 30_000,
            headed: false,
            ignore_cert_errors: false,
            user_data_dir: None,
            hide_infobars: true,
            json_pretty: false,
            verbose: false,
            trace: false,
            command,
            cdp_url: None,
            connect: false,
            profile: None,
            no_stealth: false,
            humanize: false,
            humanize_speed: "normal".to_string(),
            requested_launch_policy: RequestedLaunchPolicy::default(),
            effective_launch_policy: RequestedLaunchPolicy::default(),
        }
    }

    #[test]
    fn resolve_named_workflow_path_projects_canonical_asset_location() {
        let path = resolve_named_workflow_path(Path::new("/tmp/rub-home"), "login_flow").unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/rub-home/workflows/login_flow.json")
        );
    }

    #[test]
    fn persist_history_export_asset_writes_replayable_pipe_json() {
        let home = std::env::temp_dir().join(format!("rub-workflow-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let cli = cli_with(
            Commands::History {
                last: 10,
                from: None,
                to: None,
                export_pipe: true,
                export_script: false,
                include_observation: false,
                save_as: Some("login_flow".to_string()),
                output: None,
            },
            home.clone(),
        );

        let mut data = serde_json::json!({
            "subject": {
                "kind": "command_history",
                "selection": { "last": 10 }
            },
            "result": {
                "format": "pipe",
                "entries": [
                    { "command": "open", "args": { "url": "https://example.com" }, "source": { "sequence": 1 } }
                ],
                "count": 1
            }
        });
        persist_history_export_asset(&cli, &mut data).unwrap();

        let saved = home.join("workflows/login_flow.json");
        let contents = std::fs::read_to_string(&saved).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["steps"][0]["command"], "open");
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["path"],
            serde_json::json!(saved.display().to_string())
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["workflow_name"],
            "login_flow"
        );
        assert!(data.get("saved_to").is_none(), "{data}");
        assert!(data.get("workflow_name").is_none(), "{data}");
        assert!(data.get("output_path").is_none(), "{data}");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_workflows_reads_saved_assets_in_name_order() {
        let home = std::env::temp_dir().join(format!("rub-workflow-list-{}", std::process::id()));
        let workflows = home.join("workflows");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("b_flow.json"), "[]").unwrap();
        std::fs::write(workflows.join("a_flow.json"), "[]").unwrap();

        let listed = list_workflows(&home).unwrap();
        assert_eq!(
            listed["result"]["items"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            2
        );
        assert_eq!(listed["result"]["items"][0]["name"], "a_flow");
        assert_eq!(listed["result"]["items"][1]["name"], "b_flow");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn persist_history_export_asset_writes_replayable_script_output() {
        let home = std::env::temp_dir().join(format!("rub-workflow-script-{}", std::process::id()));
        let output_path = home.join("exports/replay.sh");
        let _ = std::fs::remove_dir_all(&home);
        let cli = cli_with(
            Commands::History {
                last: 10,
                from: None,
                to: None,
                export_pipe: false,
                export_script: true,
                include_observation: false,
                save_as: None,
                output: Some(output_path.display().to_string()),
            },
            home.clone(),
        );

        let mut data = serde_json::json!({
            "subject": {
                "kind": "command_history",
                "selection": { "last": 10 }
            },
            "result": {
                "format": "script",
                "export": {
                    "kind": "shell_script",
                    "content": "#!/usr/bin/env bash\nrub pipe --file /tmp/example.json\n"
                },
                "count": 1
            }
        });
        persist_history_export_asset(&cli, &mut data).unwrap();

        let contents = std::fs::read_to_string(&output_path).unwrap();
        assert!(contents.contains("rub pipe --file"));
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["path"],
            serde_json::json!(output_path.display().to_string())
        );
        assert!(data.get("saved_to").is_none(), "{data}");
        assert!(data.get("workflow_name").is_none(), "{data}");
        assert!(data.get("output_path").is_none(), "{data}");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn persist_history_export_asset_rolls_back_prior_write_on_second_failure() {
        let home =
            std::env::temp_dir().join(format!("rub-workflow-rollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let block = home.join("blocked-parent");
        std::fs::write(&block, b"not-a-directory").unwrap();
        let saved = home.join("workflows/login_flow.json");
        let cli = cli_with(
            Commands::History {
                last: 10,
                from: None,
                to: None,
                export_pipe: true,
                export_script: false,
                include_observation: false,
                save_as: Some("login_flow".to_string()),
                output: Some(block.join("export.json").display().to_string()),
            },
            home.clone(),
        );

        let mut data = serde_json::json!({
            "subject": { "kind": "command_history" },
            "result": {
                "format": "pipe",
                "entries": [
                    { "command": "open", "args": { "url": "https://example.com" }, "source": { "sequence": 1 } }
                ]
            }
        });

        persist_history_export_asset(&cli, &mut data)
            .expect_err("second write should fail and roll back the first");
        assert!(!saved.exists(), "first output should be rolled back");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn commit_asset_writes_rejects_unreadable_existing_targets_before_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "rub-workflow-assets-unreadable-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let unreadable = root.join("workflow.json");
        std::fs::create_dir_all(&unreadable).expect("seed unreadable directory target");

        let error = commit_asset_writes(vec![PendingAssetWrite {
            path: unreadable.clone(),
            contents: br#"{"steps":[]}"#.to_vec(),
            artifact: serde_json::json!({
                "path": unreadable.display().to_string(),
            }),
        }])
        .expect_err("directory target should be rejected");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(
            envelope.message.contains("not readable for rollback"),
            "{envelope:?}"
        );
        assert!(unreadable.is_dir(), "existing target must remain untouched");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_preserves_concurrently_recreated_target() {
        let root = std::env::temp_dir().join(format!(
            "rub-workflow-assets-rollback-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = root.join("workflow.json");
        std::fs::write(&path, b"other-writer").expect("seed competing file");

        let error = remove_newly_created_asset_if_matches(&path, br#"{"steps":[]}"#)
            .expect_err("mismatched file authority should not be deleted");
        assert!(error.to_string().contains("no longer matches"));
        assert_eq!(
            std::fs::read(&path).expect("target preserved"),
            b"other-writer"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
