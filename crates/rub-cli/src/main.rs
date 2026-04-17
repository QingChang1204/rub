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
mod local_asset_paths;
mod local_registry;
mod main_dispatch;
mod main_support;
mod orchestration_assets;
mod output;
mod persisted_artifacts;
mod secret_ctl;
mod session_policy;
mod teardown_ctl;
mod timeout_budget;
mod workflow_assets;
mod workflow_params;

#[cfg(test)]
use self::main_dispatch::close_all_selector_error;
use self::main_dispatch::{
    close_command_uses_attachment_selector, resolve_connection_request_for_cli,
    try_handle_local_command_before_binding, try_handle_prebootstrap_command,
};
use self::main_support::{
    command_timeout_envelope, daemon_args, finalize_response_output, rub_home_create_error,
};
#[cfg(test)]
use self::main_support::{handle_sessions, use_alias_local_surface_error};
use clap::Parser;
use commands::{Cli, Commands};
use session_policy::{
    materialize_connection_request_with_deadline, requested_attachment_identity,
    requires_existing_session_validation,
    validate_existing_session_connection_request_with_deadline,
};
use std::time::Instant;

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
    let close_command_has_attachment_selector = matches!(&cli.command, Commands::Close { .. })
        && close_command_uses_attachment_selector(&cli);

    if try_handle_local_command_before_binding(
        &cli,
        command_name.as_str(),
        session,
        &rub_home,
        pretty,
        timeout,
    )
    .await
    {
        return;
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
    let binding_execution_connection_request = binding_execution.connection_request_override;
    let cli = binding_execution.cli;
    let session = cli.session.as_str();

    if try_handle_prebootstrap_command(
        &cli,
        command_name.as_str(),
        session,
        &rub_home,
        pretty,
        timeout,
        close_command_has_attachment_selector,
        binding_execution_projection.as_ref(),
        binding_execution_connection_request.as_ref(),
    )
    .await
    {
        return;
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

    let command_started_at = Instant::now();
    let request = match timeout_budget::build_request(&cli) {
        Ok(request) => request,
        Err(error) => {
            let build_timeout_ms = timeout_budget::command_timeout_ms(&cli);
            let build_deadline =
                timeout_budget::deadline_from_start(command_started_at, build_timeout_ms);
            let envelope = if timeout_budget::remaining_budget_duration(build_deadline).is_none() {
                command_timeout_envelope(build_timeout_ms)
            } else {
                error.into_envelope()
            };
            println!(
                "{}",
                output::format_cli_error(command_name.as_str(), session, envelope, pretty,)
            );
            std::process::exit(1);
        }
    };
    let command_deadline =
        timeout_budget::deadline_from_start(command_started_at, request.timeout_ms);

    if let Err(error) = timeout_budget::ensure_remaining_budget(
        command_deadline,
        request.timeout_ms,
        "request_build",
    ) {
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

    let connection_request = match resolve_connection_request_for_cli(
        &cli,
        binding_execution_connection_request.as_ref(),
    ) {
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
    let connection_request = match materialize_connection_request_with_deadline(
        &connection_request,
        Some(command_deadline),
        Some(request.timeout_ms),
    )
    .await
    {
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
        cli.session_id.as_deref(),
        command_deadline,
        request.timeout_ms,
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
        && let Err(error) = validate_existing_session_connection_request_with_deadline(
            &cli,
            &connection_request,
            &mut client,
            cli.session_id.as_deref().or(daemon_session_id.as_deref()),
            Some(command_deadline),
            Some(request.timeout_ms),
        )
        .await
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
            let output = match finalize_response_output(
                &cli,
                command_name.as_str(),
                session,
                &rub_home,
                pretty,
                binding_execution_projection.as_ref(),
                &mut response,
            ) {
                Ok(output) => output,
                Err(output) => {
                    println!("{output}");
                    std::process::exit(1);
                }
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

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
