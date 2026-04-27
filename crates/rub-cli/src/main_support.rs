use crate::binding_execution_ctl;
use crate::commands::{Commands, EffectiveCli};
use crate::daemon_ctl::compatibility_degraded_owned_from_snapshot;
use crate::explain_ctl;
use crate::orchestration_assets;
use crate::output;
use crate::session_policy::ConnectionRequest;
use crate::workflow_assets;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::BindingExecutionResolutionInfo;
use rub_ipc::protocol::{IpcResponse, ResponseStatus};

pub(crate) struct FinalizedResponseOutput {
    pub(crate) output: String,
    pub(crate) success: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct FinalizeResponseContext<'a> {
    pub(crate) command_name: &'a str,
    pub(crate) session: &'a str,
    pub(crate) rub_home: &'a std::path::Path,
    pub(crate) pretty: bool,
    pub(crate) command_deadline: std::time::Instant,
    pub(crate) timeout_ms: u64,
    pub(crate) binding_execution_projection: Option<&'a BindingExecutionResolutionInfo>,
}

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

fn registry_liveness_name(liveness: rub_daemon::session::RegistryEntryLiveness) -> &'static str {
    match liveness {
        rub_daemon::session::RegistryEntryLiveness::Live => "live",
        rub_daemon::session::RegistryEntryLiveness::BusyOrUnknown => "busy_or_unknown",
        rub_daemon::session::RegistryEntryLiveness::ProbeContractFailure => {
            "probe_contract_failure"
        }
        rub_daemon::session::RegistryEntryLiveness::ProtocolIncompatible => "protocol_incompatible",
        rub_daemon::session::RegistryEntryLiveness::HardCutReleasePending => {
            "hard_cut_release_pending"
        }
        rub_daemon::session::RegistryEntryLiveness::PendingStartup => "pending_startup",
        rub_daemon::session::RegistryEntryLiveness::Dead => "dead",
    }
}

fn attach_supported_for_registry_liveness(
    liveness: rub_daemon::session::RegistryEntryLiveness,
) -> bool {
    matches!(liveness, rub_daemon::session::RegistryEntryLiveness::Live)
}

pub(crate) fn project_sessions_result(
    rub_home: &std::path::Path,
    entries: Vec<rub_daemon::session::RegistryEntrySnapshot>,
) -> serde_json::Value {
    let items = entries
        .into_iter()
        .map(|entry_snapshot| {
            let compatibility_degraded_owned =
                compatibility_degraded_owned_from_snapshot(&entry_snapshot);
            let rub_daemon::session::RegistryEntry {
                session_id,
                session_name,
                pid,
                socket_path,
                created_at,
                ipc_protocol_version,
                user_data_dir,
                ..
            } = entry_snapshot.entry;
            let liveness = entry_snapshot.liveness;

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
                "liveness": registry_liveness_name(liveness),
                "attach_supported": attach_supported_for_registry_liveness(liveness),
                "compatibility_degraded_owned_reason": compatibility_degraded_owned
                    .as_ref()
                    .map(|degraded| degraded.reason),
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
    let entries =
        rub_daemon::session::active_registry_entry_snapshots(rub_home).map_err(|error| {
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

    let output = output::format_cli_success(
        "sessions",
        session,
        rub_home,
        sessions_data,
        pretty,
        output::InteractionTraceMode::Compact,
    );
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
            if let ConnectionRequest::Profile { resolved_path, .. } = request {
                args.push("--profile-resolved-path".to_string());
                args.push(resolved_path.clone());
            }
        }
        ConnectionRequest::None => {}
    }
    args
}

fn format_committed_local_failure_output(
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
    context.insert(
        "committed_response_projection".to_string(),
        response.data.clone().unwrap_or(serde_json::Value::Null),
    );
    envelope.context = Some(serde_json::Value::Object(context));
    output::format_committed_cli_error(response, command_name, session, envelope, pretty)
}

fn project_locator_explain_or_committed_failure(
    target: &crate::commands::ElementAddressArgs,
    response: &mut IpcResponse,
    command_name: &str,
    session: &str,
    pretty: bool,
    reason: &'static str,
) -> Result<Option<String>, String> {
    let original_data = response.data.take();
    match explain_ctl::project_locator_explain_response(target, original_data.clone()) {
        Ok(data) => {
            response.data = Some(data);
            Ok(None)
        }
        Err(error) => {
            response.data = original_data;
            Ok(Some(format_committed_local_failure_output(
                response,
                command_name,
                session,
                pretty,
                error,
                reason,
            )))
        }
    }
}

pub(crate) fn finalize_response_output(
    cli: &EffectiveCli,
    context: FinalizeResponseContext<'_>,
    response: &mut IpcResponse,
) -> FinalizedResponseOutput {
    let FinalizeResponseContext {
        command_name,
        session,
        rub_home,
        pretty,
        command_deadline,
        timeout_ms,
        binding_execution_projection,
    } = context;
    if response.status == ResponseStatus::Success
        && let Commands::Explain {
            subcommand: crate::commands::ExplainSubcommand::Locator { target },
        } = &cli.command
        && let Some(output) = project_locator_explain_or_committed_failure(
            target,
            response,
            command_name,
            session,
            pretty,
            "post_commit_locator_explain_failed",
        )
        .expect("post-commit explain projection should surface as local follow-up output")
    {
        return FinalizedResponseOutput {
            output,
            success: false,
        };
    }
    if response.status == ResponseStatus::Success
        && let Commands::Find {
            target,
            content: false,
            explain: true,
            ..
        } = &cli.command
        && let Some(output) = project_locator_explain_or_committed_failure(
            target,
            response,
            command_name,
            session,
            pretty,
            "post_commit_find_locator_explain_failed",
        )
        .expect("post-commit find projection should surface as local follow-up output")
    {
        return FinalizedResponseOutput {
            output,
            success: false,
        };
    }
    if response.status == ResponseStatus::Success
        && let Some(data) = response.data.as_mut()
        && let Err(error) = workflow_assets::persist_history_export_asset_until(
            cli,
            data,
            command_deadline,
            timeout_ms,
        )
    {
        return FinalizedResponseOutput {
            output: format_committed_local_failure_output(
                response,
                command_name,
                session,
                pretty,
                error,
                "post_commit_history_export_failed",
            ),
            success: false,
        };
    }
    if response.status == ResponseStatus::Success
        && let Some(data) = response.data.as_mut()
        && let Err(error) = orchestration_assets::persist_orchestration_export_asset_until(
            cli,
            data,
            command_deadline,
            timeout_ms,
        )
    {
        return FinalizedResponseOutput {
            output: format_committed_local_failure_output(
                response,
                command_name,
                session,
                pretty,
                error,
                "post_commit_orchestration_export_failed",
            ),
            success: false,
        };
    }
    if let Some(projection) = binding_execution_projection {
        binding_execution_ctl::attach_binding_execution_projection(&mut response.data, projection);
    }
    if exec_raw_requested(&cli.command) && response.status == ResponseStatus::Success {
        if let Some(output) = output::format_exec_raw_response(response, pretty) {
            return FinalizedResponseOutput {
                output,
                success: true,
            };
        }
        return FinalizedResponseOutput {
            output: format_committed_local_failure_output(
                response,
                command_name,
                session,
                pretty,
                RubError::domain_with_context(
                    ErrorCode::IpcProtocolError,
                    "exec --raw response is missing data.result".to_string(),
                    serde_json::json!({
                        "reason": "exec_raw_result_missing",
                    }),
                ),
                "post_commit_exec_raw_projection_failed",
            ),
            success: false,
        };
    }

    let output = output::format_response_with_success(
        response,
        command_name,
        session,
        rub_home,
        pretty,
        output_trace_mode(cli),
    );
    FinalizedResponseOutput {
        output: output.output,
        success: output.success,
    }
}
