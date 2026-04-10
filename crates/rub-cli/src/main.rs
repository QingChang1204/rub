//! rub — Rust Browser Automation CLI entry point.

mod cleanup_ctl;
mod commands;
mod connection_hardening;
mod daemon_ctl;
mod harvest_ctl;
mod internal_daemon;
mod orchestration_assets;
mod output;
mod persisted_artifacts;
mod session_policy;
mod timeout_budget;
mod workflow_assets;
mod workflow_params;

use clap::Parser;
use commands::{Cli, Commands, EffectiveCli};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use session_policy::{
    ConnectionRequest, materialize_connection_request, parse_connection_request,
    requested_attachment_identity, requires_existing_session_validation,
    validate_existing_session_connection_request,
};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    let parsed = Cli::parse();
    let session_name = parsed.session.clone();
    let pretty = parsed.json_pretty;
    let command_name = parsed.command.canonical_name().to_string();
    let cli = match parsed.effective() {
        Ok(cli) => cli,
        Err(error) => {
            println!(
                "{}",
                output::format_cli_error(
                    command_name.as_str(),
                    &session_name,
                    error.into_envelope(),
                    pretty,
                )
            );
            std::process::exit(1);
        }
    };
    let rub_home = cli.rub_home.clone();
    let session = session_name.as_str();
    let pretty = cli.json_pretty;
    let timeout = cli.timeout;

    if matches!(&cli.command, Commands::Sessions) {
        if let Err(error) = handle_sessions(&rub_home, session, pretty) {
            println!(
                "{}",
                output::format_cli_error("sessions", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        return;
    }

    if matches!(&cli.command, Commands::Cleanup) {
        match cleanup_ctl::cleanup_runtime(&rub_home, timeout).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "cleanup",
                        session,
                        cleanup_ctl::project_cleanup_result(&rub_home, &result),
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("cleanup", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    if matches!(&cli.command, Commands::InternalDaemon) {
        internal_daemon::run(cli).await;
        return;
    }

    // v1.3: close --all — handle before connecting to a single daemon
    if let Commands::Close { all: true } = &cli.command {
        if let Err(error) = parse_connection_request(&cli) {
            println!(
                "{}",
                output::format_cli_error("close", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        match daemon_ctl::close_all_sessions(&rub_home, timeout).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "close",
                        session,
                        daemon_ctl::project_batch_close_result(&rub_home, &result),
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("close", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    if let Commands::Close { all: false } = &cli.command {
        if let Err(error) = parse_connection_request(&cli) {
            println!(
                "{}",
                output::format_cli_error("close", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        match daemon_ctl::close_existing_session(&rub_home, session, timeout).await {
            Ok(daemon_ctl::ExistingCloseOutcome::Closed(response)) => {
                let output = output::format_response(
                    &response,
                    "close",
                    session,
                    pretty,
                    output_trace_mode(&cli),
                );
                println!("{output}");
                if response.status == rub_ipc::protocol::ResponseStatus::Error {
                    std::process::exit(1);
                }
                return;
            }
            Ok(daemon_ctl::ExistingCloseOutcome::Noop) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "close",
                        session,
                        serde_json::json!({
                            "subject": {
                                "kind": "session_browser",
                            },
                            "result": {
                                "closed": false,
                                "daemon_stopped": false,
                                "daemon_exit_policy": "no_existing_daemon_authority",
                            }
                        }),
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("close", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    if let Err(e) = std::fs::create_dir_all(&rub_home) {
        let output = output::format_cli_error(
            command_name.as_str(),
            session,
            rub_home_create_error(&rub_home, &e),
            pretty,
        );
        println!("{output}");
        std::process::exit(1);
    }

    if matches!(
        &cli.command,
        Commands::Pipe {
            list_workflows: true,
            ..
        }
    ) {
        match workflow_assets::list_workflows(&rub_home) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "pipe",
                        session,
                        result,
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("pipe", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    if matches!(
        &cli.command,
        Commands::Orchestration {
            subcommand: commands::OrchestrationSubcommand::ListAssets,
        }
    ) {
        match orchestration_assets::list_orchestrations(&rub_home) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "orchestration",
                        session,
                        result,
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error(
                        "orchestration",
                        session,
                        error.into_envelope(),
                        pretty,
                    )
                );
                std::process::exit(1);
            }
        }
    }

    if matches!(
        &cli.command,
        Commands::Inspect(commands::InspectSubcommand::Harvest { .. })
    ) {
        match harvest_ctl::inspect_harvest(&cli).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "inspect",
                        session,
                        result,
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("inspect", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    let connection_request = match parse_connection_request(&cli) {
        Ok(request) => request,
        Err(error) => {
            println!(
                "{}",
                output::format_cli_error(
                    command_name.as_str(),
                    session,
                    error.into_envelope(),
                    pretty,
                )
            );
            std::process::exit(1);
        }
    };
    let connection_request = match materialize_connection_request(&connection_request).await {
        Ok(request) => request,
        Err(error) => {
            println!(
                "{}",
                output::format_cli_error(
                    command_name.as_str(),
                    session,
                    error.into_envelope(),
                    pretty,
                )
            );
            std::process::exit(1);
        }
    };

    let request = match timeout_budget::build_request(&cli) {
        Ok(request) => request,
        Err(error) => {
            println!(
                "{}",
                output::format_cli_error(
                    command_name.as_str(),
                    session,
                    error.into_envelope(),
                    pretty,
                )
            );
            std::process::exit(1);
        }
    };
    let command_deadline = Instant::now() + Duration::from_millis(request.timeout_ms);

    let daemon_args = daemon_args(&cli, &connection_request);
    // Existing-session validation must run before any live probe of an override
    // target, otherwise an unreachable `--cdp-url` masks the real
    // fail-closed error with a transport failure.
    let attachment_identity = requested_attachment_identity(&cli, &connection_request);
    if daemon_ctl::remaining_budget_ms(command_deadline) == 0 {
        println!(
            "{}",
            output::format_cli_error(
                command_name.as_str(),
                session,
                command_timeout_envelope(request.timeout_ms),
                pretty,
            )
        );
        std::process::exit(1);
    }
    let bootstrap = match daemon_ctl::bootstrap_client(
        &rub_home,
        session,
        command_deadline,
        &daemon_args,
        attachment_identity.as_deref(),
    )
    .await
    {
        Ok(bootstrap) => bootstrap,
        Err(e) => {
            let output =
                output::format_cli_error(command_name.as_str(), session, e.into_envelope(), pretty);
            println!("{output}");
            std::process::exit(1);
        }
    };
    let mut client = bootstrap.client;
    let connected_to_existing_daemon = bootstrap.connected_to_existing_daemon;
    let daemon_session_id = bootstrap.daemon_session_id;

    if requires_existing_session_validation(connected_to_existing_daemon, &connection_request, &cli)
        && let Err(error) =
            validate_existing_session_connection_request(&cli, &connection_request).await
    {
        println!(
            "{}",
            output::format_cli_error(
                command_name.as_str(),
                session,
                error.into_envelope(),
                pretty,
            )
        );
        std::process::exit(1);
    }

    if daemon_ctl::remaining_budget_ms(command_deadline) == 0 {
        println!(
            "{}",
            output::format_cli_error(
                command_name.as_str(),
                session,
                command_timeout_envelope(request.timeout_ms),
                pretty,
            )
        );
        std::process::exit(1);
    }

    match daemon_ctl::send_request_with_replay_recovery(
        &mut client,
        &request,
        command_deadline,
        daemon_ctl::ReplayRecoveryContext {
            rub_home: &cli.rub_home,
            session,
            daemon_args: &daemon_args,
            attachment_identity: attachment_identity.as_deref(),
            original_daemon_session_id: daemon_session_id.as_deref(),
        },
    )
    .await
    {
        Ok(mut response) => {
            if response.status == rub_ipc::protocol::ResponseStatus::Success
                && let Some(data) = response.data.as_mut()
                && let Err(error) = workflow_assets::persist_history_export_asset(&cli, data)
            {
                let mut envelope = error.into_envelope();
                let mut context = envelope
                    .context
                    .take()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                context.insert(
                    "reason".to_string(),
                    serde_json::json!("post_commit_history_export_failed"),
                );
                context.insert(
                    "daemon_request_committed".to_string(),
                    serde_json::json!(true),
                );
                envelope.context = Some(serde_json::Value::Object(context));
                println!(
                    "{}",
                    output::format_post_commit_cli_error(
                        &response,
                        command_name.as_str(),
                        session,
                        envelope,
                        pretty,
                    )
                );
                std::process::exit(1);
            }
            if response.status == rub_ipc::protocol::ResponseStatus::Success
                && let Some(data) = response.data.as_mut()
                && let Err(error) =
                    orchestration_assets::persist_orchestration_export_asset(&cli, data)
            {
                let mut envelope = error.into_envelope();
                let mut context = envelope
                    .context
                    .take()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                context.insert(
                    "reason".to_string(),
                    serde_json::json!("post_commit_orchestration_export_failed"),
                );
                context.insert(
                    "daemon_request_committed".to_string(),
                    serde_json::json!(true),
                );
                envelope.context = Some(serde_json::Value::Object(context));
                println!(
                    "{}",
                    output::format_post_commit_cli_error(
                        &response,
                        command_name.as_str(),
                        session,
                        envelope,
                        pretty,
                    )
                );
                std::process::exit(1);
            }
            let output = if exec_raw_requested(&cli.command) {
                output::format_exec_raw_response(&response, pretty).unwrap_or_else(|| {
                    output::format_response(
                        &response,
                        command_name.as_str(),
                        session,
                        pretty,
                        output_trace_mode(&cli),
                    )
                })
            } else {
                output::format_response(
                    &response,
                    command_name.as_str(),
                    session,
                    pretty,
                    output_trace_mode(&cli),
                )
            };
            println!("{output}");
            if response.status == rub_ipc::protocol::ResponseStatus::Error {
                std::process::exit(1);
            }
        }
        Err(error) => {
            let output = output::format_cli_error(
                command_name.as_str(),
                session,
                error.into_envelope(),
                pretty,
            );
            println!("{output}");
            std::process::exit(1);
        }
    }
}

fn command_timeout_envelope(timeout_ms: u64) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!("Command exceeded the declared timeout budget of {timeout_ms}ms"),
    )
    .with_context(serde_json::json!({
        "reason": "command_deadline_exhausted",
        "timeout_ms": timeout_ms,
    }))
}

fn exec_raw_requested(command: &Commands) -> bool {
    matches!(command, Commands::Exec { raw: true, .. })
}

fn output_trace_mode(cli: &EffectiveCli) -> output::InteractionTraceMode {
    if cli.trace {
        output::InteractionTraceMode::Trace
    } else if cli.verbose {
        output::InteractionTraceMode::Verbose
    } else {
        output::InteractionTraceMode::Compact
    }
}

fn local_runtime_path_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> rub_core::model::PathReferenceState {
    rub_core::model::PathReferenceState {
        truth_level: "local_runtime_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

fn rub_home_create_error(rub_home: &std::path::Path, error: &std::io::Error) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::DaemonStartFailed,
        format!("Cannot create RUB_HOME {}: {error}", rub_home.display()),
    )
    .with_context(serde_json::json!({
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": local_runtime_path_state(
            "cli.main.subject.rub_home",
            "cli_rub_home",
            "rub_home_directory",
        ),
        "reason": "rub_home_create_failed",
    }))
}

fn local_session_path_state(
    path_authority: &str,
    path_kind: &str,
) -> rub_core::model::PathReferenceState {
    local_runtime_path_state(path_authority, "cli_sessions_projection", path_kind)
}

fn project_sessions_result(
    rub_home: &std::path::Path,
    entries: Vec<rub_daemon::session::RegistryEntry>,
) -> serde_json::Value {
    let items = entries
        .into_iter()
        .map(|entry| {
            let rub_daemon::session::RegistryEntry {
                session_id,
                session_name,
                pid,
                socket_path,
                created_at,
                ipc_protocol_version,
                user_data_dir,
                ..
            } = entry;

            serde_json::json!({
                "id": session_id,
                "name": session_name,
                "pid": pid,
                "socket": socket_path,
                "socket_state": local_session_path_state(
                    "cli.sessions.result.items.socket",
                    "session_socket"
                ),
                "created_at": created_at,
                "ipc_protocol_version": ipc_protocol_version,
                "user_data_dir": user_data_dir,
                "user_data_dir_state": user_data_dir.as_ref().map(|_| {
                    local_session_path_state(
                        "cli.sessions.result.items.user_data_dir",
                        "session_user_data_dir",
                    )
                }),
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "subject": {
            "kind": "session_registry",
            "rub_home": rub_home.display().to_string(),
            "rub_home_state": local_session_path_state(
                "cli.sessions.subject.rub_home",
                "session_registry_home",
            ),
        },
        "result": {
            "items": items,
        }
    })
}

fn handle_sessions(
    rub_home: &std::path::Path,
    session: &str,
    pretty: bool,
) -> Result<(), rub_core::error::RubError> {
    use rub_core::model::CommandResult;

    let entries = rub_daemon::session::active_registry_entries(rub_home).map_err(|error| {
        rub_core::error::RubError::domain_with_context(
            ErrorCode::DaemonNotRunning,
            format!("Failed to read session registry: {error}"),
            serde_json::json!({
                "rub_home": rub_home.display().to_string(),
                "rub_home_state": local_runtime_path_state(
                    "cli.sessions.subject.rub_home",
                    "cli_rub_home",
                    "session_registry_home",
                ),
                "reason": "session_registry_read_failed",
            }),
        )
    })?;
    let sessions_data = project_sessions_result(rub_home, entries);

    let result = CommandResult::success(
        "sessions",
        session,
        uuid::Uuid::now_v7().to_string(),
        sessions_data,
    );
    let output = if pretty {
        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    };
    println!("{output}");
    Ok(())
}

pub(crate) fn daemon_args(cli: &EffectiveCli, request: &ConnectionRequest) -> Vec<String> {
    let mut args = Vec::new();
    if cli.headed {
        args.push("--headed".to_string());
    }
    if cli.ignore_cert_errors {
        args.push("--ignore-cert-errors".to_string());
    }
    if matches!(request, ConnectionRequest::None)
        && let Some(user_data_dir) = &cli.user_data_dir
    {
        args.push("--user-data-dir".to_string());
        args.push(user_data_dir.clone());
    }
    if !cli.hide_infobars {
        args.push("--show-infobars".to_string());
    }
    if cli.no_stealth {
        args.push("--no-stealth".to_string());
    }
    if cli.humanize {
        args.push("--humanize".to_string());
    }
    if cli.humanize_speed != "normal" {
        args.push("--humanize-speed".to_string());
        args.push(cli.humanize_speed.clone());
    }
    match request {
        ConnectionRequest::CdpUrl { url } => {
            args.push("--cdp-url".to_string());
            args.push(url.clone());
        }
        ConnectionRequest::AutoDiscover => {
            args.push("--connect".to_string());
        }
        ConnectionRequest::Profile { name, .. } => {
            args.push("--profile".to_string());
            args.push(name.clone());
        }
        ConnectionRequest::None => {}
    }
    args
}

#[cfg(test)]
mod tests {
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
}
