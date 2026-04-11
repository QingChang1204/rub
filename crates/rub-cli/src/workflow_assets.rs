mod export;
mod listing;
mod write;

pub use export::persist_history_export_asset;
pub use listing::list_workflows;
pub(crate) use listing::local_workflow_asset_path_state;
pub(crate) use rub_daemon::workflow_assets::{
    normalize_workflow_name, resolve_named_workflow_path, workflow_asset_path_state,
};

#[cfg(test)]
mod tests {
    use super::write::{
        PendingAssetWrite, commit_asset_writes, remove_newly_created_asset_if_matches,
    };
    use super::{list_workflows, persist_history_export_asset, resolve_named_workflow_path};
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
                    {
                        "command": "pipe",
                        "args": {
                            "spec": "[]",
                            "spec_source": {
                                "kind": "workflow",
                                "name": "login_flow",
                                "path": "/tmp/rub-home/workflows/login_flow.json",
                                "path_state": {
                                    "truth_level": "input_path_reference",
                                    "path_authority": "cli.pipe.spec_source.path",
                                    "upstream_truth": "cli_pipe_workflow_option",
                                    "path_kind": "workflow_asset_reference",
                                    "control_role": "display_only"
                                }
                            }
                        },
                        "source": { "sequence": 1 }
                    }
                ],
                "count": 1
            }
        });
        persist_history_export_asset(&cli, &mut data).unwrap();

        let saved = home.join("workflows/login_flow.json");
        let contents = std::fs::read_to_string(&saved).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["steps"][0]["command"], "pipe");
        assert!(parsed["steps"][0].get("source").is_none(), "{parsed}");
        assert_eq!(
            parsed["steps"][0]["args"]["spec_source"]["path_state"]["path_authority"],
            "cli.pipe.spec_source.path"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["path"],
            serde_json::json!(saved.display().to_string())
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["workflow_name"],
            "login_flow"
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
            "cli.history_export_asset_persistence"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["upstream_commit_truth"],
            "daemon_response_committed"
        );
        assert!(data.get("saved_to").is_none(), "{data}");
        assert!(data.get("workflow_name").is_none(), "{data}");
        assert!(data.get("output_path").is_none(), "{data}");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn persist_history_export_asset_preserves_projection_truth_labels() {
        let home =
            std::env::temp_dir().join(format!("rub-workflow-projection-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let output_path = home.join("exports/history.json");
        let cli = cli_with(
            Commands::History {
                last: 10,
                from: None,
                to: None,
                export_pipe: true,
                export_script: false,
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
                "format": "pipe",
                "projection_state": {
                    "surface": "workflow_capture_export",
                    "truth_level": "operator_projection",
                    "projection_kind": "bounded_post_commit_projection",
                    "projection_authority": "session.workflow_capture",
                    "upstream_commit_truth": "daemon_response_committed",
                    "control_role": "display_only",
                    "durability": "best_effort",
                    "lossy": false,
                    "lossy_reasons": []
                },
                "entries": [
                    {
                        "command": "pipe",
                        "args": {
                            "spec": "[]",
                            "spec_source": {
                                "kind": "file",
                                "path": "/tmp/workflow.json",
                                "path_state": {
                                    "truth_level": "input_path_reference",
                                    "path_authority": "cli.pipe.spec_source.path",
                                    "upstream_truth": "cli_pipe_file_option",
                                    "path_kind": "workflow_spec_file",
                                    "control_role": "display_only"
                                }
                            }
                        },
                        "source": { "sequence": 1 }
                    }
                ],
                "count": 1
            }
        });
        persist_history_export_asset(&cli, &mut data).unwrap();

        let contents = std::fs::read_to_string(&output_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["steps"][0]["command"], "pipe");
        assert!(parsed["steps"][0].get("source").is_none(), "{parsed}");
        assert_eq!(
            parsed["steps"][0]["args"]["spec_source"]["path_state"]["upstream_truth"],
            "cli_pipe_file_option"
        );
        assert_eq!(
            data["result"]["projection_state"]["surface"],
            "workflow_capture_export"
        );
        assert_eq!(
            data["result"]["projection_state"]["projection_kind"],
            "bounded_post_commit_projection"
        );
        assert_eq!(
            data["result"]["projection_state"]["truth_level"],
            "operator_projection"
        );
        assert_eq!(
            data["result"]["projection_state"]["upstream_commit_truth"],
            "daemon_response_committed"
        );
        assert_eq!(
            data["result"]["projection_state"]["control_role"],
            "display_only"
        );
        assert_eq!(
            data["result"]["projection_state"]["durability"],
            "best_effort"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["projection_authority"],
            "cli.history_export_asset_persistence"
        );
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["durability"],
            "durable"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn persist_history_export_asset_preserves_redacted_secret_placeholders() {
        let home = std::env::temp_dir().join(format!("rub-workflow-secret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let output_path = home.join("exports/history.json");
        let cli = cli_with(
            Commands::History {
                last: 10,
                from: None,
                to: None,
                export_pipe: true,
                export_script: false,
                include_observation: false,
                save_as: Some("secret_flow".to_string()),
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
                "format": "pipe",
                "entries": [
                    {
                        "command": "pipe",
                        "args": {
                            "spec": [{
                                "command": "fill",
                                "args": {
                                    "selector": "#token",
                                    "value": "$RUB_TOKEN"
                                }
                            }],
                            "headers": {
                                "authorization": "Bearer $RUB_TOKEN"
                            }
                        },
                        "source": { "sequence": 1 }
                    }
                ],
                "count": 1
            }
        });

        persist_history_export_asset(&cli, &mut data).unwrap();

        let named_output = home.join("workflows/secret_flow.json");
        let named_contents = std::fs::read_to_string(&named_output).unwrap();
        let file_contents = std::fs::read_to_string(&output_path).unwrap();

        assert!(named_contents.contains("$RUB_TOKEN"), "{named_contents}");
        assert!(file_contents.contains("$RUB_TOKEN"), "{file_contents}");
        assert!(!named_contents.contains("token-123"), "{named_contents}");
        assert!(!file_contents.contains("token-123"), "{file_contents}");

        let named_parsed: serde_json::Value = serde_json::from_str(&named_contents).unwrap();
        let file_parsed: serde_json::Value = serde_json::from_str(&file_contents).unwrap();
        assert_eq!(
            named_parsed["steps"][0]["args"]["headers"]["authorization"],
            "Bearer $RUB_TOKEN"
        );
        assert_eq!(
            file_parsed["steps"][0]["args"]["spec"][0]["args"]["value"],
            "$RUB_TOKEN"
        );

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
        assert_eq!(
            listed["subject"]["directory_state"]["path_authority"],
            "cli.workflow_assets.directory"
        );
        assert_eq!(
            listed["result"]["items"][0]["path_state"]["path_authority"],
            "cli.workflow_assets.item.path"
        );
        assert_eq!(
            listed["result"]["items"][0]["path_state"]["truth_level"],
            "local_asset_reference"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_workflows_read_failure_preserves_directory_state() {
        let home = std::env::temp_dir().join(format!(
            "rub-workflow-list-failure-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create rub_home");
        let workflows_dir = rub_daemon::rub_paths::RubPaths::new(&home).workflows_dir();
        std::fs::write(&workflows_dir, b"not-a-directory").expect("seed blocking file");

        let envelope = list_workflows(&home)
            .expect_err("workflow directory read should fail")
            .into_envelope();
        let context = envelope.context.expect("workflow listing context");
        assert_eq!(context["reason"], "workflow_directory_read_failed");
        assert_eq!(
            context["directory_state"]["path_authority"],
            "cli.workflow_assets.directory"
        );

        let _ = std::fs::remove_file(&workflows_dir);
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
        assert_eq!(
            data["result"]["persisted_artifacts"][0]["projection_state"]["projection_kind"],
            "cli_persisted_artifact"
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
        let context = envelope.context.expect("workflow asset error context");
        assert_eq!(context["reason"], "workflow_asset_unreadable_for_rollback");
        assert_eq!(
            context["path_state"]["path_authority"],
            "cli.workflow_assets.write.path"
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

    #[test]
    fn commit_asset_writes_rejects_duplicate_export_path_with_path_state() {
        let root = std::env::temp_dir().join(format!(
            "rub-workflow-assets-duplicate-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let duplicate = root.join("workflow.json");

        let error = commit_asset_writes(vec![
            PendingAssetWrite {
                path: duplicate.clone(),
                contents: br#"{"steps":[1]}"#.to_vec(),
                artifact: serde_json::json!({
                    "path": duplicate.display().to_string(),
                }),
            },
            PendingAssetWrite {
                path: duplicate.clone(),
                contents: br#"{"steps":[2]}"#.to_vec(),
                artifact: serde_json::json!({
                    "path": duplicate.display().to_string(),
                }),
            },
        ])
        .expect_err("duplicate export path should be rejected");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("duplicate path context");
        assert_eq!(context["reason"], "workflow_asset_duplicate_export_path");
        assert_eq!(
            context["path_state"]["path_authority"],
            "cli.workflow_assets.write.path"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn commit_asset_writes_preserves_path_state_on_write_failure() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "rub-workflow-assets-write-failure-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let locked_parent = root.join("locked-parent");
        std::fs::create_dir_all(&locked_parent).expect("create locked parent");
        std::fs::set_permissions(&locked_parent, std::fs::Permissions::from_mode(0o500))
            .expect("lock parent permissions");
        let target = locked_parent.join("workflow.json");

        let error = commit_asset_writes(vec![PendingAssetWrite {
            path: target.clone(),
            contents: br#"{"steps":[]}"#.to_vec(),
            artifact: serde_json::json!({
                "path": target.display().to_string(),
            }),
        }])
        .expect_err("write into locked parent should fail");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("write failure context");
        assert_eq!(context["reason"], "workflow_asset_write_failed");
        assert_eq!(
            context["path_state"]["path_authority"],
            "cli.workflow_assets.write.path"
        );

        std::fs::set_permissions(&locked_parent, std::fs::Permissions::from_mode(0o700))
            .expect("restore parent permissions");
        let _ = std::fs::remove_dir_all(root);
    }
}
