use crate::binding_ctl;
use crate::binding_execution_ctl;
use crate::cleanup_ctl;
use crate::commands::{Commands, EffectiveCli, ExplainSubcommand, InspectSubcommand};
use crate::daemon_ctl;
use crate::extract_help_ctl;
use crate::harvest_ctl;
use crate::inspect_list_help_ctl;
use crate::internal_daemon;
use crate::main_support::{
    finalize_response_output, handle_sessions, output_trace_mode, use_alias_local_surface_error,
};
use crate::orchestration_assets;
use crate::output;
use crate::secret_ctl;
use crate::session_policy::{
    ConnectionRequest, materialize_connection_request_with_deadline, parse_connection_request,
    resolve_attachment_identity,
};
use crate::teardown_ctl;
use crate::timeout_budget::run_with_remaining_budget;
use crate::workflow_assets;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::BindingExecutionResolutionInfo;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) fn close_command_uses_attachment_selector(cli: &EffectiveCli) -> bool {
    cli.use_alias.is_some()
        || cli.cdp_url.is_some()
        || cli.connect
        || cli.profile.is_some()
        || cli.requested_launch_policy.user_data_dir.is_some()
}

pub(crate) fn close_all_selector_error() -> rub_core::error::RubError {
    rub_core::error::RubError::domain_with_context(
        rub_core::error::ErrorCode::InvalidInput,
        "close --all does not accept a browser attachment selector",
        serde_json::json!({
            "reason": "close_all_selector_not_supported",
        }),
    )
}

pub(crate) fn close_all_partial_failure_error(
    rub_home: &Path,
    result: &daemon_ctl::BatchCloseResult,
) -> RubError {
    let failed_count = result.failed.len();
    RubError::Domain(
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!("Failed to close {failed_count} session(s) during close --all"),
        )
        .with_suggestion(
            "Inspect result.failed, rerun 'rub close <session>' for those sessions, or use 'rub doctor' before retrying.",
        )
        .with_context(serde_json::json!({
            "reason": "close_all_partial_failure",
            "close_all": daemon_ctl::project_batch_close_result(rub_home, result),
            "failed_sessions": result.failed,
        })),
    )
}

fn close_noop_payload() -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "session_browser",
        },
        "result": {
            "closed": false,
            "daemon_stopped": false,
            "daemon_exit_policy": "no_existing_daemon_authority",
        }
    })
}

pub(crate) async fn resolve_close_selector_attachment_identity_until(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<Option<String>, rub_core::error::RubError> {
    run_with_remaining_budget(
        deadline,
        timeout_ms.max(1),
        "close_selector_attachment_identity_resolution",
        async { resolve_attachment_identity(cli, request, None).await },
    )
    .await
}

pub(crate) fn resolve_connection_request_for_cli(
    cli: &EffectiveCli,
    override_request: Option<&ConnectionRequest>,
) -> Result<ConnectionRequest, rub_core::error::RubError> {
    override_request
        .cloned()
        .map_or_else(|| parse_connection_request(cli), Ok)
}

pub(crate) async fn try_handle_local_command_before_binding(
    cli: &EffectiveCli,
    command_name: &str,
    session: &str,
    rub_home: &Path,
    pretty: bool,
    timeout: u64,
) -> bool {
    if let Some(error) = use_alias_local_surface_error(cli) {
        println!(
            "{}",
            output::format_cli_error(command_name, session, error, pretty,)
        );
        std::process::exit(1);
    }

    if matches!(&cli.command, Commands::Sessions) {
        if let Err(error) = handle_sessions(rub_home, session, pretty) {
            println!(
                "{}",
                output::format_cli_error("sessions", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        return true;
    }

    if let Commands::Binding { subcommand } = &cli.command {
        if let Err(error) = binding_ctl::handle_binding_command(cli, subcommand).await {
            println!(
                "{}",
                output::format_cli_error("binding", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        return true;
    }

    if let Commands::Secret { subcommand } = &cli.command {
        if let Err(error) = secret_ctl::handle_secret_command(cli, subcommand) {
            println!(
                "{}",
                output::format_cli_error("secret", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        return true;
    }

    if matches!(&cli.command, Commands::Cleanup) {
        match cleanup_ctl::cleanup_runtime(rub_home, timeout).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "cleanup",
                        session,
                        rub_home,
                        cleanup_ctl::project_cleanup_result(rub_home, &result),
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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
        match teardown_ctl::teardown_runtime(rub_home, timeout).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "teardown",
                        session,
                        rub_home,
                        teardown_ctl::project_teardown_result(rub_home, &result),
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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

    false
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_handle_prebootstrap_command(
    cli: &EffectiveCli,
    _command_name: &str,
    session: &str,
    rub_home: &Path,
    pretty: bool,
    timeout: u64,
    close_command_has_attachment_selector: bool,
    binding_execution_projection: Option<&BindingExecutionResolutionInfo>,
    binding_execution_connection_request: Option<&ConnectionRequest>,
) -> bool {
    if let Commands::Explain {
        subcommand: ExplainSubcommand::Extract { .. },
    } = &cli.command
    {
        let Commands::Explain { subcommand } = &cli.command else {
            unreachable!("guarded by explain extract match");
        };
        match crate::explain_ctl::project_explain(subcommand, rub_home) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "explain",
                        session,
                        rub_home,
                        result,
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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
                        rub_home,
                        result,
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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
                        rub_home,
                        result,
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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
        internal_daemon::run(cli.clone()).await;
        return true;
    }

    if let Commands::Close { all: true } = &cli.command {
        if close_command_has_attachment_selector {
            println!(
                "{}",
                output::format_cli_error(
                    "close",
                    session,
                    close_all_selector_error().into_envelope(),
                    pretty,
                )
            );
            std::process::exit(1);
        }
        if let Err(error) =
            resolve_connection_request_for_cli(cli, binding_execution_connection_request)
        {
            println!(
                "{}",
                output::format_cli_error("close", session, error.into_envelope(), pretty)
            );
            std::process::exit(1);
        }
        match daemon_ctl::close_all_sessions(rub_home, timeout).await {
            Ok(result) if result.failed.is_empty() => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "close",
                        session,
                        rub_home,
                        daemon_ctl::project_batch_close_result(rub_home, &result),
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
            }
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_error(
                        "close",
                        session,
                        close_all_partial_failure_error(rub_home, &result).into_envelope(),
                        pretty,
                    )
                );
                std::process::exit(1);
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
        let connection_request =
            match resolve_connection_request_for_cli(cli, binding_execution_connection_request) {
                Ok(request) => request,
                Err(error) => {
                    println!(
                        "{}",
                        output::format_cli_error("close", session, error.into_envelope(), pretty)
                    );
                    std::process::exit(1);
                }
            };
        let close_deadline = Instant::now() + Duration::from_millis(timeout.max(1));
        let connection_request = match materialize_connection_request_with_deadline(
            &connection_request,
            Some(close_deadline),
            Some(timeout),
        )
        .await
        {
            Ok(request) => request,
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("close", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        };
        let attachment_identity = match resolve_close_selector_attachment_identity_until(
            cli,
            &connection_request,
            close_deadline,
            timeout,
        )
        .await
        {
            Ok(identity) => identity,
            Err(error) => {
                println!(
                    "{}",
                    output::format_cli_error("close", session, error.into_envelope(), pretty)
                );
                std::process::exit(1);
            }
        };
        let close_outcome = if let Some(attachment_identity) = attachment_identity {
            match daemon_ctl::resolve_existing_close_target_by_attachment_identity(
                rub_home,
                &attachment_identity,
                timeout,
            )
            .await
            {
                Ok(Some(target)) => {
                    daemon_ctl::close_existing_session_targeted(
                        rub_home,
                        &target.session_name,
                        Some(target.daemon_session_id.as_str()),
                        timeout,
                    )
                    .await
                }
                Ok(None) => Ok(daemon_ctl::ExistingCloseOutcome::Noop),
                Err(error) => Err(error),
            }
        } else {
            daemon_ctl::close_existing_session(rub_home, session, timeout).await
        };
        match close_outcome {
            Ok(daemon_ctl::ExistingCloseOutcome::Closed(response)) => {
                let mut response = *response;
                let output = match finalize_response_output(
                    cli,
                    "close",
                    session,
                    rub_home,
                    pretty,
                    binding_execution_projection,
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
                return true;
            }
            Ok(daemon_ctl::ExistingCloseOutcome::Noop) => {
                let mut data = Some(close_noop_payload());
                if let Some(projection) = binding_execution_projection {
                    binding_execution_ctl::attach_binding_execution_projection(
                        &mut data, projection,
                    );
                }
                println!(
                    "{}",
                    output::format_cli_success(
                        "close",
                        session,
                        rub_home,
                        data.unwrap_or_else(close_noop_payload),
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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

    if matches!(
        &cli.command,
        Commands::Pipe {
            list_workflows: true,
            ..
        }
    ) {
        match workflow_assets::list_workflows(rub_home) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "pipe",
                        session,
                        rub_home,
                        result,
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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
            subcommand: crate::commands::OrchestrationSubcommand::ListAssets,
        }
    ) {
        match orchestration_assets::list_orchestrations(rub_home) {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "orchestration",
                        session,
                        rub_home,
                        result,
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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
        Commands::Inspect(crate::commands::InspectSubcommand::Harvest { .. })
    ) {
        match harvest_ctl::inspect_harvest(cli).await {
            Ok(result) => {
                println!(
                    "{}",
                    output::format_cli_success(
                        "inspect",
                        session,
                        rub_home,
                        result,
                        pretty,
                        output_trace_mode(cli),
                    )
                );
                return true;
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

    false
}
