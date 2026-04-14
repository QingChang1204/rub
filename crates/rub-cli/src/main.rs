//! rub — Rust Browser Automation CLI entry point.

mod binding_ctl;
mod binding_execution_ctl;
mod binding_memory_ctl;
mod cleanup_ctl;
mod commands;
mod connection_hardening;
mod daemon_ctl;
mod explain_ctl;
mod extract_help_ctl;
mod harvest_ctl;
mod inspect_list_help_ctl;
mod internal_daemon;
mod orchestration_assets;
mod output;
mod persisted_artifacts;
mod secret_ctl;
mod session_policy;
mod teardown_ctl;
mod timeout_budget;
mod workflow_assets;
mod workflow_params;

use clap::Parser;
use commands::{Cli, Commands, EffectiveCli, ExplainSubcommand, InspectSubcommand};
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
    let pretty = cli.json_pretty;
    let timeout = cli.timeout;
    let session = session_name.as_str();

    if let Some(error) = use_alias_local_surface_error(&cli) {
        println!(
            "{}",
            output::format_cli_error(command_name.as_str(), session, error, pretty,)
        );
        std::process::exit(1);
    }

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

    if let Commands::Binding { subcommand } = &cli.command {
        if let Err(error) = binding_ctl::handle_binding_command(&cli, subcommand).await {
            println!(
                "{}",
                output::format_cli_error("binding", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        return;
    }

    if let Commands::Secret { subcommand } = &cli.command {
        if let Err(error) = secret_ctl::handle_secret_command(&cli, subcommand) {
            println!(
                "{}",
                output::format_cli_error("secret", session, error.into_envelope(), pretty)
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
                        &rub_home,
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

    if matches!(&cli.command, Commands::Teardown) {
        match teardown_ctl::teardown_runtime(&rub_home, timeout).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "teardown",
                        session,
                        &rub_home,
                        teardown_ctl::project_teardown_result(&rub_home, &result),
                        pretty,
                        output_trace_mode(&cli),
                    )
                );
                return;
            }
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("teardown", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    let binding_execution = match binding_execution_ctl::resolve_command_execution_binding(&cli) {
        Ok(resolved) => resolved,
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
    let binding_execution_projection = binding_execution.projection;
    let cli = binding_execution.cli;
    let session = cli.session.as_str();

    if let Commands::Explain {
        subcommand: ExplainSubcommand::Extract { .. },
    } = &cli.command
    {
        let Commands::Explain { subcommand } = &cli.command else {
            unreachable!("guarded by explain extract match");
        };
        match explain_ctl::project_explain(subcommand, &rub_home) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "explain",
                        session,
                        &rub_home,
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
                    output::format_cli_error("explain", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    if let Commands::Extract {
        examples, schema, ..
    } = &cli.command
        && (*schema || examples.is_some())
    {
        match extract_help_ctl::project_extract_help(examples.as_deref(), *schema) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "extract",
                        session,
                        &rub_home,
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
                    output::format_cli_error("extract", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        }
    }

    if let Commands::Inspect(InspectSubcommand::List {
        builder_help: true, ..
    }) = &cli.command
    {
        match inspect_list_help_ctl::project_inspect_list_builder_help() {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "inspect",
                        session,
                        &rub_home,
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
                        &rub_home,
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
                    &rub_home,
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
                        &rub_home,
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
                        &rub_home,
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
                        &rub_home,
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
                        &rub_home,
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
                && let Commands::Explain {
                    subcommand: ExplainSubcommand::Locator { target },
                } = &cli.command
            {
                match explain_ctl::project_locator_explain_response(target, response.data.take()) {
                    Ok(data) => response.data = Some(data),
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
                }
            }
            if response.status == rub_ipc::protocol::ResponseStatus::Success
                && let Commands::Find {
                    target,
                    content: false,
                    explain: true,
                    ..
                } = &cli.command
            {
                match explain_ctl::project_locator_explain_response(target, response.data.take()) {
                    Ok(data) => response.data = Some(data),
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
                }
            }
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
            if let Some(projection) = binding_execution_projection.as_ref() {
                binding_execution_ctl::attach_binding_execution_projection(
                    &mut response.data,
                    projection,
                );
            }
            let output = if exec_raw_requested(&cli.command) {
                output::format_exec_raw_response(&response, pretty).unwrap_or_else(|| {
                    output::format_response(
                        &response,
                        command_name.as_str(),
                        session,
                        &rub_home,
                        pretty,
                        output_trace_mode(&cli),
                    )
                })
            } else {
                output::format_response(
                    &response,
                    command_name.as_str(),
                    session,
                    &rub_home,
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

fn use_alias_local_surface_error(cli: &EffectiveCli) -> Option<ErrorEnvelope> {
    let alias = cli.use_alias.as_deref()?;
    let surface = cli.command.local_projection_surface()?;
    Some(
        rub_core::error::RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Remembered alias reuse with --use is only available for browser-backed commands; local-only surface '{surface}' cannot reuse a runtime binding"
            ),
            serde_json::json!({
                "alias": alias,
                "surface": surface,
                "reason": "binding_execution_unavailable_for_local_surface",
            }),
        )
        .into_envelope(),
    )
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
        ConnectionRequest::UserDataDir { path } => {
            args.push("--user-data-dir".to_string());
            args.push(path.clone());
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
#[path = "main/tests.rs"]
mod tests;
