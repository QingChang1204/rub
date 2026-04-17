use crate::binding_execution_ctl;
use crate::commands::{Commands, EffectiveCli};
use crate::explain_ctl;
use crate::orchestration_assets;
use crate::output;
use crate::session_policy::ConnectionRequest;
use crate::workflow_assets;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::BindingExecutionResolutionInfo;
use rub_ipc::protocol::{IpcResponse, ResponseStatus};

pub(crate) fn use_alias_local_surface_error(cli: &EffectiveCli) -> Option<ErrorEnvelope> {
    let alias = cli.use_alias.as_deref()?;
    let surface = cli.command.local_projection_surface()?;
    Some(
        RubError::domain_with_context(
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

pub(crate) fn command_timeout_envelope(timeout_ms: u64) -> ErrorEnvelope {
    command_timeout_envelope_for_phase(timeout_ms, "command_dispatch")
}

pub(crate) fn command_timeout_envelope_for_phase(timeout_ms: u64, phase: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!("Command exceeded the declared timeout budget of {timeout_ms}ms"),
    )
    .with_context(serde_json::json!({
        "reason": "command_deadline_exhausted",
        "timeout_ms": timeout_ms,
        "phase": phase,
    }))
}

pub(crate) fn command_timeout_error(timeout_ms: u64, phase: &str) -> RubError {
    RubError::Domain(command_timeout_envelope_for_phase(timeout_ms, phase))
}

pub(crate) fn exec_raw_requested(command: &Commands) -> bool {
    matches!(command, Commands::Exec { raw: true, .. })
}

pub(crate) fn output_trace_mode(cli: &EffectiveCli) -> output::InteractionTraceMode {
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

pub(crate) fn rub_home_create_error(
    rub_home: &std::path::Path,
    error: &std::io::Error,
) -> ErrorEnvelope {
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

pub(crate) fn project_sessions_result(
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

pub(crate) fn handle_sessions(
    rub_home: &std::path::Path,
    session: &str,
    pretty: bool,
) -> Result<(), RubError> {
    use rub_core::model::CommandResult;

    let entries = rub_daemon::session::active_registry_entries(rub_home).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::IoError,
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

fn format_post_commit_error_output(
    response: &IpcResponse,
    command_name: &str,
    session: &str,
    pretty: bool,
    error: RubError,
    reason: &'static str,
) -> String {
    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert("reason".to_string(), serde_json::json!(reason));
    context.insert(
        "daemon_request_committed".to_string(),
        serde_json::json!(true),
    );
    envelope.context = Some(serde_json::Value::Object(context));
    output::format_post_commit_cli_error(response, command_name, session, envelope, pretty)
}

pub(crate) fn finalize_response_output(
    cli: &EffectiveCli,
    command_name: &str,
    session: &str,
    rub_home: &std::path::Path,
    pretty: bool,
    binding_execution_projection: Option<&BindingExecutionResolutionInfo>,
    response: &mut IpcResponse,
) -> Result<String, String> {
    if response.status == ResponseStatus::Success
        && let Commands::Explain {
            subcommand: crate::commands::ExplainSubcommand::Locator { target },
        } = &cli.command
    {
        match explain_ctl::project_locator_explain_response(target, response.data.take()) {
            Ok(data) => response.data = Some(data),
            Err(error) => {
                return Err(output::format_cli_error(
                    command_name,
                    session,
                    error.into_envelope(),
                    pretty,
                ));
            }
        }
    }
    if response.status == ResponseStatus::Success
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
                return Err(output::format_cli_error(
                    command_name,
                    session,
                    error.into_envelope(),
                    pretty,
                ));
            }
        }
    }
    if response.status == ResponseStatus::Success
        && let Some(data) = response.data.as_mut()
        && let Err(error) = workflow_assets::persist_history_export_asset(cli, data)
    {
        return Err(format_post_commit_error_output(
            response,
            command_name,
            session,
            pretty,
            error,
            "post_commit_history_export_failed",
        ));
    }
    if response.status == ResponseStatus::Success
        && let Some(data) = response.data.as_mut()
        && let Err(error) = orchestration_assets::persist_orchestration_export_asset(cli, data)
    {
        return Err(format_post_commit_error_output(
            response,
            command_name,
            session,
            pretty,
            error,
            "post_commit_orchestration_export_failed",
        ));
    }
    if let Some(projection) = binding_execution_projection {
        binding_execution_ctl::attach_binding_execution_projection(&mut response.data, projection);
    }
    let output = if exec_raw_requested(&cli.command) {
        output::format_exec_raw_response(response, pretty).unwrap_or_else(|| {
            output::format_response(
                response,
                command_name,
                session,
                rub_home,
                pretty,
                output_trace_mode(cli),
            )
        })
    } else {
        output::format_response(
            response,
            command_name,
            session,
            rub_home,
            pretty,
            output_trace_mode(cli),
        )
    };
    Ok(output)
}
