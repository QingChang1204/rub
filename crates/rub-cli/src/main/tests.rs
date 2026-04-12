use super::*;
use rub_core::error::ErrorCode;
use rub_daemon::session::RegistryEntry;

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
