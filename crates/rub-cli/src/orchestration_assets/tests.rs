use super::{
    commit_asset_writes, list_orchestrations, pending_asset_write,
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

    let error = commit_asset_writes(vec![
        pending_asset_write(
            unreadable.clone(),
            br#"{"actions":[]}"#.to_vec(),
            serde_json::json!({
                "path": unreadable.display().to_string(),
            }),
        )
        .expect("prepare unreadable target"),
    ])
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
        pending_asset_write(
            duplicate.clone(),
            br#"{"actions":[1]}"#.to_vec(),
            serde_json::json!({
                "path": duplicate.display().to_string(),
            }),
        )
        .expect("prepare first duplicate"),
        pending_asset_write(
            duplicate.clone(),
            br#"{"actions":[2]}"#.to_vec(),
            serde_json::json!({
                "path": duplicate.display().to_string(),
            }),
        )
        .expect("prepare second duplicate"),
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

    let error = commit_asset_writes(vec![
        pending_asset_write(
            target.clone(),
            br#"{"actions":[]}"#.to_vec(),
            serde_json::json!({
                "path": target.display().to_string(),
            }),
        )
        .expect("prepare locked target"),
    ])
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

#[cfg(unix)]
#[test]
fn commit_asset_writes_rejects_duplicate_export_path_through_symlink_alias() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "rub-orchestration-assets-symlink-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let actual = root.join("actual");
    let alias = root.join("alias");
    std::fs::create_dir_all(&actual).expect("create actual directory");
    symlink(&actual, &alias).expect("create symlink alias");

    let error = commit_asset_writes(vec![
        pending_asset_write(
            actual.join("reply_rule.json"),
            br#"{"actions":[1]}"#.to_vec(),
            serde_json::json!({
                "path": actual.join("reply_rule.json").display().to_string(),
            }),
        )
        .expect("prepare actual target"),
        pending_asset_write(
            alias.join("reply_rule.json"),
            br#"{"actions":[2]}"#.to_vec(),
            serde_json::json!({
                "path": alias.join("reply_rule.json").display().to_string(),
            }),
        )
        .expect("prepare alias target"),
    ])
    .expect_err("symlink alias should collide with canonical authority");

    let envelope = error.into_envelope();
    let context = envelope.context.expect("alias duplicate context");
    assert_eq!(
        context["reason"],
        "orchestration_asset_duplicate_export_path"
    );
    assert!(context.get("authority_path").is_some(), "{context:?}");

    let _ = std::fs::remove_dir_all(root);
}
