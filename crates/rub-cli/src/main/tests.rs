use super::*;
use crate::binding_ctl::write_binding_registry;
use crate::binding_execution_ctl::resolve_command_execution_binding;
use crate::binding_memory_ctl::remember_binding_alias;
use crate::commands::RememberedBindingAliasKindArg;
use rub_core::error::ErrorCode;
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCreatedVia, BindingPersistencePolicy,
    BindingRecord, BindingRegistryData, BindingScope, BindingSessionReference,
    BindingSessionReferenceKind,
};
use rub_daemon::session::RegistryEntry;
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
        use_alias: Some("ops-admin".to_string()),
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
fn handle_sessions_reports_registry_read_failure_instead_of_empty_success() {
    let temp = std::env::temp_dir().join(format!("rub-sessions-test-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    std::fs::write(&temp, b"not-a-directory").expect("temp file should be writable");

    let error = handle_sessions(&temp, "default", false)
        .expect_err("registry read failure should propagate")
        .into_envelope();
    assert_eq!(error.code, ErrorCode::DaemonNotRunning);
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
        vec![RegistryEntry {
            session_id: "sess-default".to_string(),
            session_name: "default".to_string(),
            pid: 4242,
            socket_path: "/tmp/rub-home/default.sock".to_string(),
            created_at: "2026-04-08T00:00:00Z".to_string(),
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: Some("/tmp/rub-home/browser/default".to_string()),
            attachment_identity: None,
            connection_target: None,
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

    let args = daemon_args(
        &resolved.cli,
        &ConnectionRequest::Profile {
            name: "Profile 3".to_string(),
            dir_name: "Profile 3".to_string(),
            resolved_path: "/tmp/work/Profile 3".to_string(),
            user_data_root: "/tmp/work".to_string(),
        },
    );
    assert!(args.contains(&"--profile".to_string()));
    assert!(args.contains(&"Profile 3".to_string()));
    assert!(!args.contains(&"--user-data-dir".to_string()));

    let _ = std::fs::remove_dir_all(home);
}
