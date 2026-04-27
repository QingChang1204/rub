use super::*;
use crate::binding_ctl::write_binding_registry;
use crate::binding_execution_ctl::resolve_command_execution_binding;
use crate::binding_memory_ctl::remember_binding_alias;
use crate::commands::EffectiveCli;
use crate::commands::RememberedBindingAliasKindArg;
use crate::main_dispatch::{
    cleanup_compatibility_degraded_owned_error, close_all_partial_failure_error,
};
use crate::main_support::project_sessions_result;
use crate::session_policy::ConnectionRequest;
use rub_core::error::ErrorCode;
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCreatedVia, BindingPersistencePolicy,
    BindingRecord, BindingRegistryData, BindingScope, BindingSessionReference,
    BindingSessionReferenceKind,
};
use rub_daemon::session::{RegistryEntry, RegistryEntryLiveness, RegistryEntrySnapshot};
use rub_ipc::protocol::IpcResponse;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

fn temp_home() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rub-main-tests-{}", Uuid::now_v7()))
}

fn use_alias_doctor_cli(home: &std::path::Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Doctor,
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: Some("ops-admin".to_string()),
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

fn close_cli(home: &std::path::Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Close { all: false },
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

fn explain_locator_cli(home: &std::path::Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Explain {
            subcommand: commands::ExplainSubcommand::Locator {
                target: commands::ElementAddressArgs::default(),
            },
        },
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

fn find_explain_cli(home: &std::path::Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Find {
            target: commands::ElementAddressArgs::default(),
            content: false,
            explain: true,
            limit: None,
        },
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

fn exec_raw_cli(home: &std::path::Path) -> EffectiveCli {
    let mut cli = close_cli(home);
    cli.command = Commands::Exec {
        code: "1 + 1".to_string(),
        raw: true,
        wait_after: commands::WaitAfterArgs::default(),
    };
    cli
}

fn history_export_cli(home: &std::path::Path, output: &std::path::Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::History {
            last: 2,
            from: None,
            to: None,
            export_pipe: true,
            export_script: false,
            include_observation: false,
            save_as: None,
            output: Some(output.display().to_string()),
        },
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

fn orchestration_export_cli(home: &std::path::Path, output: &std::path::Path) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.to_path_buf(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Orchestration {
            subcommand: commands::OrchestrationSubcommand::Export {
                id: 1,
                save_as: None,
                output: Some(output.display().to_string()),
            },
        },
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

fn write_profile_binding(home: &std::path::Path) {
    write_binding_registry(
        home,
        &BindingRegistryData {
            schema_version: 1,
            bindings: vec![BindingRecord {
                alias: "old-admin".to_string(),
                scope: BindingScope::RubHomeLocal,
                rub_home_reference: home.display().to_string(),
                session_reference: Some(BindingSessionReference {
                    kind: BindingSessionReferenceKind::LiveSessionHint,
                    session_id: "sess-work".to_string(),
                    session_name: "work".to_string(),
                }),
                attachment_identity: Some("profile:/tmp/work/Profile 3".to_string()),
                profile_directory_reference: Some("/tmp/work/Profile 3".to_string()),
                user_data_dir_reference: Some("/tmp/work".to_string()),
                auth_provenance: BindingAuthProvenance {
                    created_via: BindingCreatedVia::BoundExistingRuntime,
                    auth_input_mode: BindingAuthInputMode::Unknown,
                    capture_fence: None,
                    captured_from_session: Some("work".to_string()),
                    captured_from_attachment_identity: Some(
                        "profile:/tmp/work/Profile 3".to_string(),
                    ),
                },
                persistence_policy: BindingPersistencePolicy::RubHomeLocalDurable,
                created_at: "2026-04-14T00:00:00Z".to_string(),
                last_captured_at: "2026-04-14T00:00:00Z".to_string(),
            }],
        },
    )
    .unwrap();
    remember_binding_alias(
        home,
        "ops-admin",
        "old-admin",
        RememberedBindingAliasKindArg::Workspace,
    )
    .unwrap();
}

#[test]
fn explain_locator_projection_failure_fails_closed_after_daemon_commit() {
    let home = temp_home();
    let cli = explain_locator_cli(&home);
    let mut response = IpcResponse::success("req-explain", json!({ "result": { "matches": [] } }))
        .with_command_id("cmd-explain")
        .expect("static command_id must be valid")
        .with_daemon_session_id("daemon-session")
        .expect("static daemon_session_id must be valid");

    let output = finalize_response_output(
        &cli,
        FinalizeResponseContext {
            command_name: "explain",
            session: "default",
            rub_home: &home,
            pretty: false,
            command_deadline: Instant::now() + Duration::from_secs(30),
            timeout_ms: 30_000,
            binding_execution_projection: None,
        },
        &mut response,
    );
    let value: serde_json::Value =
        serde_json::from_str(&output.output).expect("formatted output should be valid json");

    assert!(!output.success);
    assert_eq!(value["success"], false, "{value}");
    assert!(value["data"].is_null(), "{value}");
    assert_eq!(value["request_id"], "req-explain", "{value}");
    assert_eq!(value["command_id"], "cmd-explain", "{value}");
    assert_eq!(
        value["error"]["context"]["reason"], "post_commit_locator_explain_failed",
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["daemon_request_committed"], true,
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["committed_response_projection"]["result"]["matches"],
        json!([]),
        "{value}"
    );
}

#[test]
fn find_explain_projection_failure_fails_closed_after_daemon_commit() {
    let home = temp_home();
    let cli = find_explain_cli(&home);
    let mut response = IpcResponse::success("req-find", json!({ "result": { "matches": [] } }))
        .with_command_id("cmd-find")
        .expect("static command_id must be valid")
        .with_daemon_session_id("daemon-session")
        .expect("static daemon_session_id must be valid");

    let output = finalize_response_output(
        &cli,
        FinalizeResponseContext {
            command_name: "find",
            session: "default",
            rub_home: &home,
            pretty: false,
            command_deadline: Instant::now() + Duration::from_secs(30),
            timeout_ms: 30_000,
            binding_execution_projection: None,
        },
        &mut response,
    );
    let value: serde_json::Value =
        serde_json::from_str(&output.output).expect("formatted output should be valid json");

    assert!(!output.success);
    assert_eq!(value["success"], false, "{value}");
    assert!(value["data"].is_null(), "{value}");
    assert_eq!(value["request_id"], "req-find", "{value}");
    assert_eq!(value["command_id"], "cmd-find", "{value}");
    assert_eq!(
        value["error"]["context"]["reason"], "post_commit_find_locator_explain_failed",
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["daemon_request_committed"], true,
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["committed_response_projection"]["result"]["matches"],
        json!([]),
        "{value}"
    );
}

#[test]
fn exec_raw_missing_result_fails_closed_after_daemon_commit() {
    let home = temp_home();
    let cli = exec_raw_cli(&home);
    let mut response = IpcResponse::success("req-raw", json!({ "ok": true }))
        .with_command_id("cmd-raw")
        .expect("static command_id must be valid")
        .with_daemon_session_id("daemon-session")
        .expect("static daemon_session_id must be valid");

    let output = finalize_response_output(
        &cli,
        FinalizeResponseContext {
            command_name: "exec",
            session: "default",
            rub_home: &home,
            pretty: false,
            command_deadline: Instant::now() + Duration::from_secs(30),
            timeout_ms: 30_000,
            binding_execution_projection: None,
        },
        &mut response,
    );
    let value: serde_json::Value =
        serde_json::from_str(&output.output).expect("raw failure should use JSON error envelope");

    assert!(!output.success);
    assert_eq!(value["success"], false, "{value}");
    assert_eq!(value["request_id"], "req-raw", "{value}");
    assert_eq!(value["command_id"], "cmd-raw", "{value}");
    assert_eq!(
        value["error"]["context"]["reason"], "post_commit_exec_raw_projection_failed",
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["daemon_request_committed"], true,
        "{value}"
    );
}

#[test]
fn finalize_response_output_uses_effect_truth_for_interaction_exit_surface() {
    let home = temp_home();
    let cli = EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: home.clone(),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Click {
            index: None,
            target: commands::ElementAddressArgs::default(),
            xy: None,
            double: false,
            right: false,
            wait_after: commands::WaitAfterArgs::default(),
        },
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    };
    let mut response = IpcResponse::success(
        "req-click",
        json!({
            "interaction": {
                "semantic_class": "activate",
                "interaction_confirmed": false,
                "confirmation_status": "degraded"
            }
        }),
    )
    .with_command_id("cmd-click")
    .expect("static command_id must be valid");

    let output = finalize_response_output(
        &cli,
        FinalizeResponseContext {
            command_name: "click",
            session: "default",
            rub_home: &home,
            pretty: false,
            command_deadline: Instant::now() + Duration::from_secs(30),
            timeout_ms: 30_000,
            binding_execution_projection: None,
        },
        &mut response,
    );
    let value: serde_json::Value =
        serde_json::from_str(&output.output).expect("formatted output should be valid json");

    assert!(!output.success);
    assert_eq!(value["success"], false, "{value}");
    assert_eq!(
        value["error"]["code"], "INTERACTION_NOT_CONFIRMED",
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_status"],
        "degraded",
        "{value}"
    );
}

#[test]
fn finalize_response_output_history_export_failure_uses_committed_top_level_error() {
    let home = temp_home();
    let blocked_output = home.join("blocked-output");
    std::fs::create_dir_all(&blocked_output).expect("create blocked output directory");
    let cli = history_export_cli(&home, &blocked_output);
    let mut response = IpcResponse::success(
        "req-history-export",
        json!({
            "result": {
                "format": "pipe",
                "projection_state": {
                    "surface": "workflow_capture_export",
                    "truth_level": "replayable_export_projection",
                    "projection_kind": "durable_workflow_export",
                    "projection_authority": "session.workflow_capture_export",
                    "upstream_commit_truth": "daemon_response_committed",
                    "control_role": "workflow_asset_source",
                    "durability": "durable",
                    "lossy": false,
                    "lossy_reasons": []
                },
                "complete": true,
                "entries": [
                    {
                        "command": "open",
                        "args": { "url": "https://example.com" }
                    }
                ]
            }
        }),
    )
    .with_command_id("cmd-history-export")
    .expect("static command_id must be valid");

    let output = finalize_response_output(
        &cli,
        FinalizeResponseContext {
            command_name: "history",
            session: "default",
            rub_home: &home,
            pretty: false,
            command_deadline: Instant::now() + Duration::from_secs(30),
            timeout_ms: 30_000,
            binding_execution_projection: None,
        },
        &mut response,
    );
    let value: serde_json::Value =
        serde_json::from_str(&output.output).expect("formatted output should be valid json");

    assert!(!output.success);
    assert_eq!(value["success"], false, "{value}");
    assert_eq!(
        value["error"]["context"]["reason"],
        "post_commit_history_export_failed"
    );
    assert_eq!(
        value["error"]["context"]["daemon_request_committed"], true,
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["committed_response_projection"]["result"]["format"], "pipe",
        "{value}"
    );
    assert!(
        value.get("data").is_none() || value["data"].is_null(),
        "{value}"
    );
}

#[test]
fn finalize_response_output_orchestration_export_failure_uses_committed_top_level_error() {
    let home = temp_home();
    let blocked_output = home.join("blocked-output");
    std::fs::create_dir_all(&blocked_output).expect("create blocked output directory");
    let cli = orchestration_export_cli(&home, &blocked_output);
    let mut response = IpcResponse::success(
        "req-orchestration-export",
        json!({
            "result": {
                "format": "orchestration",
                "spec": {
                    "source": { "session_id": "source" },
                    "target": { "session_id": "target" },
                    "condition": { "kind": "url_match", "url": "https://example.com" },
                    "actions": [{ "kind": "browser_command", "command": "reload" }]
                }
            }
        }),
    )
    .with_command_id("cmd-orchestration-export")
    .expect("static command_id must be valid");

    let output = finalize_response_output(
        &cli,
        FinalizeResponseContext {
            command_name: "orchestration",
            session: "default",
            rub_home: &home,
            pretty: false,
            command_deadline: Instant::now() + Duration::from_secs(30),
            timeout_ms: 30_000,
            binding_execution_projection: None,
        },
        &mut response,
    );
    let value: serde_json::Value =
        serde_json::from_str(&output.output).expect("formatted output should be valid json");

    assert!(!output.success);
    assert_eq!(value["success"], false, "{value}");
    assert_eq!(
        value["error"]["context"]["reason"], "post_commit_orchestration_export_failed",
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["daemon_request_committed"], true,
        "{value}"
    );
    assert_eq!(
        value["error"]["context"]["committed_response_projection"]["result"]["format"],
        "orchestration",
        "{value}"
    );
    assert!(
        value.get("data").is_none() || value["data"].is_null(),
        "{value}"
    );
}

#[test]
fn daemon_args_forward_v14_policy_flags() {
    let mut cli = EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: std::path::PathBuf::from("/tmp/rub-test"),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Doctor,
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    };
    cli.no_stealth = true;
    cli.humanize = true;
    cli.humanize_speed = "slow".to_string();

    let args = daemon_args(&cli, &ConnectionRequest::None);
    assert!(args.contains(&"--no-stealth".to_string()));
    assert!(args.contains(&"--humanize".to_string()));
    assert!(args.contains(&"--humanize-speed".to_string()));
    assert!(args.contains(&"slow".to_string()));
}

#[test]
fn daemon_args_skip_config_default_user_data_dir_for_profile_request() {
    let cli = EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: std::path::PathBuf::from("/tmp/rub-test"),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: Some("/tmp/config-default-profile-root".to_string()),
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Doctor,
        cdp_url: None,
        connect: false,
        profile: Some("Default".to_string()),
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy {
            user_data_dir: Some("/tmp/config-default-profile-root".to_string()),
            ..commands::RequestedLaunchPolicy::default()
        },
    };

    let args = daemon_args(
        &cli,
        &ConnectionRequest::Profile {
            name: "Default".to_string(),
            dir_name: "Default".to_string(),
            resolved_path: "/tmp/config-default-profile-root/Default".to_string(),
            user_data_root: "/tmp/config-default-profile-root".to_string(),
        },
    );
    assert!(args.contains(&"--profile".to_string()));
    assert!(!args.contains(&"--user-data-dir".to_string()));
}

#[test]
fn daemon_args_forward_materialized_auto_discover_as_explicit_cdp_url() {
    let cli = EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: std::path::PathBuf::from("/tmp/rub-test"),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Doctor,
        cdp_url: None,
        connect: true,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    };

    let args = daemon_args(
        &cli,
        &ConnectionRequest::CdpUrl {
            url: "ws://127.0.0.1:9222/devtools/browser/browser-a".to_string(),
        },
    );
    assert!(args.contains(&"--cdp-url".to_string()));
    assert!(args.contains(&"ws://127.0.0.1:9222/devtools/browser/browser-a".to_string()));
    assert!(!args.contains(&"--connect".to_string()));
}

#[test]
fn daemon_args_forward_explicit_user_data_dir_request() {
    let cli = EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: std::path::PathBuf::from("/tmp/rub-test"),
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: Some("/tmp/rub-profile".to_string()),
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command: Commands::Doctor,
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy {
            user_data_dir: Some("/tmp/rub-profile".to_string()),
            ..commands::RequestedLaunchPolicy::default()
        },
        effective_launch_policy: commands::RequestedLaunchPolicy {
            user_data_dir: Some("/tmp/rub-profile".to_string()),
            ..commands::RequestedLaunchPolicy::default()
        },
    };

    let args = daemon_args(
        &cli,
        &ConnectionRequest::UserDataDir {
            path: "/tmp/rub-profile".to_string(),
        },
    );
    assert!(args.contains(&"--user-data-dir".to_string()));
    assert!(args.contains(&"/tmp/rub-profile".to_string()));
}

#[test]
fn close_command_selector_detection_covers_use_alias_and_profile_root() {
    let home = temp_home();
    let mut cli = close_cli(&home);
    assert!(!close_command_uses_attachment_selector(&cli));

    cli.use_alias = Some("ops-admin".to_string());
    assert!(close_command_uses_attachment_selector(&cli));

    cli.use_alias = None;
    cli.profile = Some("Work".to_string());
    assert!(close_command_uses_attachment_selector(&cli));
}

#[test]
fn close_all_selector_error_is_invalid_input() {
    let envelope = close_all_selector_error().into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert_eq!(
        envelope.context.expect("context")["reason"],
        serde_json::json!("close_all_selector_not_supported")
    );
}

#[test]
fn close_all_partial_failure_error_reports_failed_sessions_as_non_success() {
    let envelope = close_all_partial_failure_error(
        std::path::Path::new("/tmp/rub-home"),
        &crate::daemon_ctl::BatchCloseResult {
            closed: vec!["default".to_string()],
            cleaned_stale: vec!["work".to_string()],
            compatibility_degraded_owned_sessions: vec![],
            failed: vec!["broken".to_string()],
            session_error_details: vec![crate::daemon_ctl::BatchCloseSessionError {
                session: "broken".to_string(),
                error: rub_core::error::ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "replay recovery failed",
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_replay_retry_failed",
                    "recovery_contract": {
                        "kind": "session_post_commit_journal",
                    },
                })),
            }],
        },
    )
    .into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
        Some(&serde_json::json!("close_all_partial_failure"))
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("failed_sessions")),
        Some(&serde_json::json!(["broken"]))
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("session_error_details"))
            .and_then(|value| value.get(0))
            .and_then(|value| value.get("error"))
            .and_then(|value| value.get("context"))
            .and_then(|value| value.get("recovery_contract"))
            .and_then(|value| value.get("kind")),
        Some(&serde_json::json!("session_post_commit_journal"))
    );
}

#[test]
fn close_all_partial_failure_error_treats_compatibility_degraded_owned_sessions_as_non_success() {
    let envelope = close_all_partial_failure_error(
        std::path::Path::new("/tmp/rub-home"),
        &crate::daemon_ctl::BatchCloseResult {
            closed: vec!["default".to_string()],
            cleaned_stale: vec![],
            compatibility_degraded_owned_sessions: vec![
                crate::daemon_ctl::CompatibilityDegradedOwnedSession {
                    session: "legacy".to_string(),
                    daemon_session_id: "sess-legacy".to_string(),
                    reason:
                        crate::daemon_ctl::CompatibilityDegradedOwnedReason::ProtocolIncompatible,
                },
            ],
            failed: vec![],
            session_error_details: vec![],
        },
    )
    .into_envelope();
    assert_eq!(envelope.code, ErrorCode::SessionBusy);
    assert!(
        envelope
            .message
            .contains("Failed to fully release 1 session"),
        "{envelope:?}"
    );
    assert_eq!(
        envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
        Some(&serde_json::json!(
            "close_all_compatibility_degraded_owned_sessions"
        ))
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("compatibility_degraded_owned_sessions")),
        Some(&serde_json::json!([{
            "session": "legacy",
            "daemon_session_id": "sess-legacy",
            "reason": "protocol_incompatible"
        }]))
    );
}

#[test]
fn cleanup_compatibility_degraded_owned_error_uses_shared_family_projection() {
    let envelope = cleanup_compatibility_degraded_owned_error(
        std::path::Path::new("/tmp/rub-home"),
        &crate::cleanup_ctl::CleanupResult {
            compatibility_degraded_owned_sessions: vec![
                crate::daemon_ctl::CompatibilityDegradedOwnedSession {
                    session: "legacy".to_string(),
                    daemon_session_id: "sess-legacy".to_string(),
                    reason:
                        crate::daemon_ctl::CompatibilityDegradedOwnedReason::ProtocolIncompatible,
                },
            ],
            ..Default::default()
        },
    )
    .into_envelope();
    assert_eq!(envelope.code, ErrorCode::SessionBusy);
    assert_eq!(
        envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
        Some(&serde_json::json!(
            "cleanup_compatibility_degraded_owned_sessions"
        ))
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("compatibility_degraded_owned_sessions")),
        Some(&serde_json::json!([{
            "session": "legacy",
            "daemon_session_id": "sess-legacy",
            "reason": "protocol_incompatible"
        }]))
    );
}

#[tokio::test]
async fn close_selector_attachment_identity_resolves_to_canonical_cdp_authority() {
    let home = temp_home();
    let mut cli = close_cli(&home);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr");
    let ws_url = format!("ws://{address}/devtools/browser/test");
    cli.cdp_url = Some(format!("http://{address}"));

    let server = tokio::spawn({
        let ws_url = ws_url.clone();
        async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await.expect("read request");
            let body = format!(r#"{{"webSocketDebuggerUrl":"{ws_url}"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });

    let request = ConnectionRequest::CdpUrl {
        url: format!("http://{address}/json/version"),
    };
    let resolved = crate::main_dispatch::resolve_close_selector_attachment_identity_until(
        &cli,
        &request,
        Instant::now() + Duration::from_secs(1),
        1_000,
    )
    .await
    .expect("close selector identity should canonicalize");
    assert_eq!(resolved.as_deref(), Some(format!("cdp:{ws_url}").as_str()));

    server.await.expect("server join");
}

#[test]
fn handle_sessions_reports_registry_read_failure_instead_of_empty_success() {
    let temp = std::env::temp_dir().join(format!("rub-sessions-test-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    std::fs::write(&temp, b"not-a-directory").expect("temp file should be writable");

    let error = handle_sessions(&temp, "default", false)
        .expect_err("registry read failure should propagate")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::IoError);
    let context = error
        .context
        .expect("sessions failure should publish context");
    assert_eq!(
        context["reason"],
        serde_json::json!("session_registry_read_failed")
    );
    assert_eq!(
        context["rub_home_state"]["path_authority"],
        "cli.sessions.subject.rub_home"
    );

    let _ = std::fs::remove_file(temp);
}

#[test]
fn rub_home_create_error_marks_rub_home_state() {
    let envelope = rub_home_create_error(
        std::path::Path::new("/tmp/rub-home"),
        &std::io::Error::other("boom"),
    );
    let context = envelope.context.expect("rub_home startup context");
    assert_eq!(context["reason"], "rub_home_create_failed");
    assert_eq!(
        context["rub_home_state"]["path_authority"],
        "cli.main.subject.rub_home"
    );
    assert_eq!(context["rub_home_state"]["upstream_truth"], "cli_rub_home");
}

#[test]
fn project_sessions_result_marks_local_runtime_paths() {
    let projected = project_sessions_result(
        std::path::Path::new("/tmp/rub-home"),
        vec![RegistryEntrySnapshot {
            entry: RegistryEntry {
                session_id: "sess-default".to_string(),
                session_name: "default".to_string(),
                pid: 4242,
                socket_path: "/tmp/rub-home/default.sock".to_string(),
                created_at: "2026-04-08T00:00:00Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: Some("/tmp/rub-home/browser/default".to_string()),
                attachment_identity: None,
                connection_target: None,
            },
            liveness: RegistryEntryLiveness::ProtocolIncompatible,
            pid_live: true,
        }],
    );

    assert_eq!(
        projected["subject"]["rub_home_state"]["path_authority"],
        "cli.sessions.subject.rub_home"
    );
    assert_eq!(
        projected["result"]["items"][0]["socket_state"]["path_authority"],
        "cli.sessions.result.items.socket"
    );
    assert_eq!(
        projected["result"]["items"][0]["socket_state"]["truth_level"],
        "local_runtime_reference"
    );
    assert_eq!(
        projected["result"]["items"][0]["user_data_dir_state"]["path_authority"],
        "cli.sessions.result.items.user_data_dir"
    );
    assert_eq!(
        projected["result"]["items"][0]["liveness"],
        "protocol_incompatible"
    );
    assert_eq!(projected["result"]["items"][0]["attach_supported"], false);
    assert_eq!(
        projected["result"]["items"][0]["compatibility_degraded_owned_reason"],
        "protocol_incompatible"
    );
}

fn local_surface_use_alias_cli(command: Commands) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home: std::path::PathBuf::from("/tmp/rub-test"),
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
        profile_resolved_path: None,
        use_alias: Some("ops-admin".to_string()),
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: commands::RequestedLaunchPolicy::default(),
        effective_launch_policy: commands::RequestedLaunchPolicy::default(),
    }
}

#[test]
fn use_alias_local_surface_error_rejects_local_only_surfaces() {
    let binding_cli = local_surface_use_alias_cli(Commands::Binding {
        subcommand: commands::BindingSubcommand::List,
    });
    let binding_error = use_alias_local_surface_error(&binding_cli).expect("binding must fail");
    assert_eq!(binding_error.code, ErrorCode::InvalidInput);
    assert_eq!(
        binding_error.context.expect("context")["surface"],
        serde_json::json!("binding")
    );

    let secret_cli = local_surface_use_alias_cli(Commands::Secret {
        subcommand: commands::SecretSubcommand::List,
    });
    let secret_error = use_alias_local_surface_error(&secret_cli).expect("secret must fail");
    assert_eq!(
        secret_error.context.expect("context")["surface"],
        serde_json::json!("secret")
    );

    let sessions_cli = local_surface_use_alias_cli(Commands::Sessions);
    let sessions_error = use_alias_local_surface_error(&sessions_cli).expect("sessions must fail");
    assert_eq!(
        sessions_error.context.expect("context")["surface"],
        serde_json::json!("sessions")
    );

    let cleanup_cli = local_surface_use_alias_cli(Commands::Cleanup);
    let cleanup_error = use_alias_local_surface_error(&cleanup_cli).expect("cleanup must fail");
    assert_eq!(
        cleanup_error.context.expect("context")["surface"],
        serde_json::json!("cleanup")
    );

    let teardown_cli = local_surface_use_alias_cli(Commands::Teardown);
    let teardown_error = use_alias_local_surface_error(&teardown_cli).expect("teardown must fail");
    assert_eq!(
        teardown_error.context.expect("context")["surface"],
        serde_json::json!("teardown")
    );

    let pipe_list_cli = local_surface_use_alias_cli(Commands::Pipe {
        spec: None,
        file: None,
        workflow: None,
        list_workflows: true,
        vars: Vec::new(),
        wait_after: commands::WaitAfterArgs::default(),
    });
    let pipe_list_error =
        use_alias_local_surface_error(&pipe_list_cli).expect("pipe list-workflows must fail");
    assert_eq!(
        pipe_list_error.context.expect("context")["surface"],
        serde_json::json!("pipe list-workflows")
    );

    let orchestration_assets_cli = local_surface_use_alias_cli(Commands::Orchestration {
        subcommand: commands::OrchestrationSubcommand::ListAssets,
    });
    let orchestration_assets_error = use_alias_local_surface_error(&orchestration_assets_cli)
        .expect("orchestration list-assets must fail");
    assert_eq!(
        orchestration_assets_error.context.expect("context")["surface"],
        serde_json::json!("orchestration list-assets")
    );

    let inspect_harvest_cli =
        local_surface_use_alias_cli(Commands::Inspect(commands::InspectSubcommand::Harvest {
            file: "/tmp/rows.json".to_string(),
            input_field: None,
            url_field: None,
            name_field: None,
            base_url: None,
            extract: None,
            extract_file: None,
            field: Vec::new(),
            limit: None,
        }));
    let inspect_harvest_error =
        use_alias_local_surface_error(&inspect_harvest_cli).expect("inspect harvest must fail");
    assert_eq!(
        inspect_harvest_error.context.expect("context")["surface"],
        serde_json::json!("inspect harvest")
    );
}

#[test]
fn use_alias_profile_binding_routes_to_daemon_profile_flag() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    write_profile_binding(&home);

    let resolved = resolve_command_execution_binding(&use_alias_doctor_cli(&home)).unwrap();
    assert_eq!(resolved.cli.profile.as_deref(), Some("Profile 3"));
    assert!(resolved.cli.user_data_dir.is_none());
    assert!(resolved.cli.use_alias.is_none());
    assert_eq!(
        resolved.connection_request_override,
        Some(ConnectionRequest::Profile {
            name: "Profile 3".to_string(),
            dir_name: "Profile 3".to_string(),
            resolved_path: "/tmp/work/Profile 3".to_string(),
            user_data_root: "/tmp/work".to_string(),
        })
    );

    let args = daemon_args(
        &resolved.cli,
        resolved
            .connection_request_override
            .as_ref()
            .expect("profile alias should preserve connection request authority"),
    );
    assert!(args.contains(&"--profile".to_string()));
    assert!(args.contains(&"Profile 3".to_string()));
    assert!(args.contains(&"--profile-resolved-path".to_string()));
    assert!(args.contains(&"/tmp/work/Profile 3".to_string()));
    assert!(!args.contains(&"--user-data-dir".to_string()));

    let _ = std::fs::remove_dir_all(home);
}
