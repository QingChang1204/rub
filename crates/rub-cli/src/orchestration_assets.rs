use crate::commands::{Commands, EffectiveCli, OrchestrationSubcommand};
use crate::persisted_artifacts::annotate_local_persisted_artifact;
use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes;
use rub_core::model::PathReferenceState;
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

pub fn persist_orchestration_export_asset(
    cli: &EffectiveCli,
    data: &mut Value,
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
        pending_writes.push(PendingAssetWrite {
            path: path.clone(),
            contents: serialized.clone(),
            artifact,
        });
    }

    if let Some(output_path) = output {
        let path = resolve_cli_path(output_path);
        let mut artifact = json!({
            "kind": "orchestration_export_file",
            "role": "output",
            "path": path.display().to_string(),
            "format": "orchestration",
        });
        if let Some(identity) = rule_identity_projection {
            artifact["source_rule_identity"] = identity;
        }
        pending_writes.push(PendingAssetWrite {
            path: path.clone(),
            contents: serialized,
            artifact,
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

fn commit_asset_writes(writes: Vec<PendingAssetWrite>) -> Result<Vec<Value>, RubError> {
    let mut committed = Vec::new();
    let mut artifacts = Vec::new();

    for write in writes {
        if committed
            .iter()
            .any(|existing: &CommittedAssetWrite| existing.path == write.path)
        {
            return Err(asset_write_error_at_path(
                format!(
                    "Duplicate orchestration export path {}",
                    write.path.display()
                ),
                &write.path,
                "orchestration_asset_duplicate_export_path",
                rollback_asset_writes(&committed).err(),
            ));
        }

        let previous_state = match read_previous_asset_state(&write.path) {
            Ok(state) => state,
            Err(error) => {
                return Err(asset_write_error_from_source(
                    error,
                    rollback_asset_writes(&committed).err(),
                ));
            }
        };
        let commit_outcome = match atomic_write_bytes(&write.path, &write.contents, 0o600) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(asset_write_error_at_path(
                    format!(
                        "Failed to write orchestration asset {}: {error}",
                        write.path.display()
                    ),
                    &write.path,
                    "orchestration_asset_write_failed",
                    rollback_asset_writes(&committed).err(),
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
                atomic_write_bytes(&write.path, previous, 0o600).map(|_| ())
            }
            PreviousAssetState::Absent => {
                remove_newly_created_asset_if_matches(&write.path, &write.committed_contents)
            }
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
        PendingAssetWrite, commit_asset_writes, list_orchestrations,
        persist_orchestration_export_asset, remove_newly_created_asset_if_matches,
    };
    use crate::commands::{Commands, EffectiveCli, OrchestrationSubcommand, RequestedLaunchPolicy};
    use rub_core::error::ErrorCode;
    use std::path::PathBuf;

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
            use_alias: None,
            no_stealth: false,
            humanize: false,
            humanize_speed: "normal".to_string(),
            requested_launch_policy: RequestedLaunchPolicy::default(),
            effective_launch_policy: RequestedLaunchPolicy::default(),
        }
    }

    #[test]
    fn list_orchestrations_reads_saved_assets_in_name_order() {
        let rub_home = std::env::temp_dir().join(format!(
            "rub-cli-orchestration-assets-{}",
            std::process::id()
        ));
        let directory = rub_daemon::rub_paths::RubPaths::new(&rub_home).orchestrations_dir();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("b.json"), "{}").unwrap();
        std::fs::write(directory.join("a.json"), "{}").unwrap();

        let listed = list_orchestrations(&rub_home).unwrap();
        assert_eq!(
            listed["result"]["items"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            2
        );
        assert_eq!(listed["result"]["items"][0]["name"], "a");
        assert_eq!(listed["result"]["items"][1]["name"], "b");
        assert_eq!(
            listed["subject"]["directory_state"]["path_authority"],
            "cli.orchestration_assets.directory"
        );
        assert_eq!(
            listed["result"]["items"][0]["path_state"]["path_authority"],
            "cli.orchestration_assets.item.path"
        );
        assert_eq!(
            listed["result"]["items"][0]["path_state"]["truth_level"],
            "local_asset_reference"
        );

        let _ = std::fs::remove_dir_all(rub_home);
    }

    #[test]
    fn list_orchestrations_read_failure_preserves_directory_state() {
        let rub_home = std::env::temp_dir().join(format!(
            "rub-cli-orchestration-list-failure-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&rub_home);
        std::fs::create_dir_all(&rub_home).expect("create rub_home");
        let directory = rub_daemon::rub_paths::RubPaths::new(&rub_home).orchestrations_dir();
        std::fs::write(&directory, b"not-a-directory").expect("seed blocking file");

        let envelope = list_orchestrations(&rub_home)
            .expect_err("orchestration directory read should fail")
            .into_envelope();
        let context = envelope.context.expect("orchestration listing context");
        assert_eq!(context["reason"], "orchestration_directory_read_failed");
        assert_eq!(
            context["directory_state"]["path_authority"],
            "cli.orchestration_assets.directory"
        );

        let _ = std::fs::remove_file(&directory);
        let _ = std::fs::remove_dir_all(&rub_home);
    }

    #[test]
    fn persist_orchestration_export_asset_writes_named_asset_output() {
        let rub_home = std::env::temp_dir().join(format!(
            "rub-cli-orchestration-export-{}",
            std::process::id()
        ));
        let cli = cli_with(
            Commands::Orchestration {
                subcommand: OrchestrationSubcommand::Export {
                    id: 7,
                    save_as: Some("reply_rule".to_string()),
                    output: None,
                },
            },
            rub_home.clone(),
        );
        let mut data = serde_json::json!({
            "subject": {
                "kind": "orchestration_rule",
                "id": 7,
            },
            "result": {
                "format": "orchestration",
                "rule_identity_projection": {
                    "surface": "orchestration_rule_identity",
                    "truth_level": "operator_projection",
                    "projection_kind": "live_rule_identity",
                    "projection_authority": "session.orchestration_runtime.rules",
                    "upstream_truth": "session_orchestration_rule",
                    "control_role": "display_only",
                    "durability": "best_effort",
                    "canonical_spec_kind": "replayable_orchestration_registration_spec",
                    "stripped_from_spec": ["correlation_key", "idempotency_key"],
                    "correlation_key": "corr-7",
                    "idempotency_key": "idem-7"
                },
                "spec": {
                    "source": { "session_id": "source" },
                    "target": { "session_id": "target" },
                    "condition": { "kind": "url_match", "url": "https://example.com" },
                    "actions": [{ "kind": "browser_command", "command": "reload" }]
                }
            }
        });
        persist_orchestration_export_asset(&cli, &mut data).unwrap();
        let saved_to = data["result"]["persisted_artifacts"][0]["path"]
            .as_str()
            .unwrap();
        let saved = std::fs::read_to_string(saved_to).unwrap();
        assert!(saved.contains("\"browser_command\""));
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["asset_name"],
            "reply_rule"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["truth_level"],
            "local_persistence_projection"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["projection_kind"],
            "cli_persisted_artifact"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["projection_authority"],
            "cli.orchestration_export_asset_persistence"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["upstream_commit_truth"],
            "daemon_response_committed"
        );
        assert_eq!(
            data["result"]["rule_identity_projection"]["surface"],
            "orchestration_rule_identity"
        );
        assert_eq!(
            data["result"]["rule_identity_projection"]["truth_level"],
            "operator_projection"
        );
        assert_eq!(
            data["result"]["rule_identity_projection"]["projection_authority"],
            "session.orchestration_runtime.rules"
        );
        assert_eq!(
            data["result"]["rule_identity_projection"]["control_role"],
            "display_only"
        );
        assert_eq!(
            data["result"]["rule_identity_projection"]["durability"],
            "best_effort"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["source_rule_identity"]["correlation_key"],
            "corr-7"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["source_rule_identity"]["idempotency_key"],
            "idem-7"
        );
        assert!(data.get("saved_to").is_none(), "{data}");
        assert!(data.get("asset_name").is_none(), "{data}");
        assert!(data.get("output_path").is_none(), "{data}");

        let _ = std::fs::remove_dir_all(rub_home);
    }

    #[test]
    fn persist_orchestration_export_asset_rolls_back_prior_write_on_second_failure() {
        let rub_home = std::env::temp_dir().join(format!(
            "rub-cli-orchestration-export-rollback-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&rub_home);
        std::fs::create_dir_all(&rub_home).unwrap();
        let blocked = rub_home.join("blocked-parent");
        std::fs::write(&blocked, b"not-a-directory").unwrap();
        let saved = rub_home.join("orchestrations/reply_rule.json");
        let cli = cli_with(
            Commands::Orchestration {
                subcommand: OrchestrationSubcommand::Export {
                    id: 7,
                    save_as: Some("reply_rule".to_string()),
                    output: Some(blocked.join("export.json").display().to_string()),
                },
            },
            rub_home.clone(),
        );
        let mut data = serde_json::json!({
            "subject": { "kind": "orchestration_rule", "id": 7 },
            "result": {
                "format": "orchestration",
                "spec": {
                    "source": { "session_id": "source" },
                    "target": { "session_id": "target" },
                    "condition": { "kind": "url_match", "url_pattern": "https://example.com" },
                    "actions": [{ "kind": "browser_command", "command": "reload" }]
                }
            }
        });

        persist_orchestration_export_asset(&cli, &mut data)
            .expect_err("second write should fail and roll back the first");
        assert!(!saved.exists(), "first output should be rolled back");

        let _ = std::fs::remove_dir_all(rub_home);
    }

    #[test]
    fn commit_asset_writes_rejects_unreadable_existing_targets_before_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "rub-orchestration-assets-unreadable-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let unreadable = root.join("reply_rule.json");
        std::fs::create_dir_all(&unreadable).expect("seed unreadable directory target");

        let error = commit_asset_writes(vec![PendingAssetWrite {
            path: unreadable.clone(),
            contents: br#"{"actions":[]}"#.to_vec(),
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
        let context = envelope.context.expect("orchestration asset error context");
        assert_eq!(
            context["reason"],
            "orchestration_asset_unreadable_for_rollback"
        );
        assert_eq!(
            context["path_state"]["path_authority"],
            "cli.orchestration_assets.write.path"
        );
        assert!(unreadable.is_dir(), "existing target must remain untouched");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_preserves_concurrently_recreated_target() {
        let root = std::env::temp_dir().join(format!(
            "rub-orchestration-assets-rollback-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = root.join("orchestration.json");
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

    #[test]
    fn commit_asset_writes_rejects_duplicate_export_path_with_path_state() {
        let root = std::env::temp_dir().join(format!(
            "rub-orchestration-assets-duplicate-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let duplicate = root.join("reply_rule.json");

        let error = commit_asset_writes(vec![
            PendingAssetWrite {
                path: duplicate.clone(),
                contents: br#"{"actions":[1]}"#.to_vec(),
                artifact: serde_json::json!({
                    "path": duplicate.display().to_string(),
                }),
            },
            PendingAssetWrite {
                path: duplicate.clone(),
                contents: br#"{"actions":[2]}"#.to_vec(),
                artifact: serde_json::json!({
                    "path": duplicate.display().to_string(),
                }),
            },
        ])
        .expect_err("duplicate export path should be rejected");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("duplicate path context");
        assert_eq!(
            context["reason"],
            "orchestration_asset_duplicate_export_path"
        );
        assert_eq!(
            context["path_state"]["path_authority"],
            "cli.orchestration_assets.write.path"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn commit_asset_writes_preserves_path_state_on_write_failure() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "rub-orchestration-assets-write-failure-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let locked_parent = root.join("locked-parent");
        std::fs::create_dir_all(&locked_parent).expect("create locked parent");
        std::fs::set_permissions(&locked_parent, std::fs::Permissions::from_mode(0o500))
            .expect("lock parent permissions");
        let target = locked_parent.join("reply_rule.json");

        let error = commit_asset_writes(vec![PendingAssetWrite {
            path: target.clone(),
            contents: br#"{"actions":[]}"#.to_vec(),
            artifact: serde_json::json!({
                "path": target.display().to_string(),
            }),
        }])
        .expect_err("write into locked parent should fail");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("write failure context");
        assert_eq!(context["reason"], "orchestration_asset_write_failed");
        assert_eq!(
            context["path_state"]["path_authority"],
            "cli.orchestration_assets.write.path"
        );

        std::fs::set_permissions(&locked_parent, std::fs::Permissions::from_mode(0o700))
            .expect("restore parent permissions");
        let _ = std::fs::remove_dir_all(root);
    }
}
