//! JSON output formatter for CLI → stdout.

mod continuity;

use self::continuity::attach_workflow_continuity;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    CommandResult, InteractionConfirmationStatus, projected_interaction_confirmation_status,
    projected_interaction_effect_success,
};
use rub_ipc::protocol::IpcResponse;
use serde_json::{Map, Value, json};
use std::path::Path;

#[cfg(test)]
const POST_COMMIT_LOCAL_FAILURE_STATE: &str = "daemon_committed_local_followup_failed";
const STDOUT_CONTRACT_FALLBACK_SURFACE: &str = "cli_stdout_contract_fallback";
const INTERACTION_EFFECT_FAILURE_SURFACE: &str = "cli_interaction_effect_failure";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionTraceMode {
    Compact,
    Verbose,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedCommandResult {
    pub output: String,
    pub success: bool,
}

/// Convert an IPC response to a CLI stdout CommandResult.
#[cfg(test)]
pub fn format_response(
    response: &IpcResponse,
    command: &str,
    session: &str,
    rub_home: &Path,
    pretty: bool,
    trace_mode: InteractionTraceMode,
) -> String {
    format_response_with_success(response, command, session, rub_home, pretty, trace_mode).output
}

pub fn format_response_with_success(
    response: &IpcResponse,
    command: &str,
    session: &str,
    rub_home: &Path,
    pretty: bool,
    trace_mode: InteractionTraceMode,
) -> FormattedCommandResult {
    let result = checked_command_result(command_result_from_response(
        response, command, session, rub_home, trace_mode,
    ));
    let success = result.success;
    let output = serialize_command_result_json(&result, pretty);
    FormattedCommandResult { output, success }
}

fn command_result_from_response(
    response: &IpcResponse,
    command: &str,
    session: &str,
    rub_home: &Path,
    trace_mode: InteractionTraceMode,
) -> CommandResult {
    if let Some(envelope) = response.contract_error_envelope() {
        return CommandResult {
            success: false,
            command: command.to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: stdout_request_id(&response.request_id),
            command_id: stdout_command_id(response.command_id.as_deref()),
            session: session.to_string(),
            timing: response.timing,
            data: None,
            error: Some(attach_authority_error_guidance(envelope)),
        };
    }

    let mut result = CommandResult {
        success: response.status == rub_ipc::protocol::ResponseStatus::Success,
        command: command.to_string(),
        stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
        request_id: response.request_id.clone(),
        command_id: response.command_id.clone(),
        session: session.to_string(),
        timing: response.timing,
        data: response.data.clone(),
        error: response.error.clone().map(attach_authority_error_guidance),
    };
    attach_interaction_trace(&mut result, trace_mode);
    attach_workflow_continuity(&mut result, rub_home);
    if let Some(effect_failure) = interaction_effect_failure_result(&result) {
        return effect_failure;
    }
    result
}

/// Format the explicit raw stdout surface for `exec --raw`.
///
/// This surface is intentionally separate from the standard JSON envelope.
/// Callers must opt into it explicitly instead of routing through
/// `format_response()`.
pub fn format_exec_raw_response(response: &IpcResponse, pretty: bool) -> Option<String> {
    if response.status != rub_ipc::protocol::ResponseStatus::Success {
        return None;
    }
    let result = response.data.as_ref().and_then(|data| data.get("result"))?;
    Some(format_raw_value(result, pretty))
}

fn format_raw_value(value: &Value, pretty: bool) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ if pretty => {
            serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
    }
}

/// Format a CLI-side error (before IPC, e.g. daemon start failure).
pub fn format_cli_error(
    command: &str,
    session: &str,
    envelope: ErrorEnvelope,
    pretty: bool,
) -> String {
    let result = CommandResult::error(
        command,
        session,
        uuid::Uuid::now_v7().to_string(),
        attach_authority_error_guidance(envelope),
    );
    serialize_checked_command_result(result, pretty)
}

/// Format a CLI-side error that happened after the daemon had already committed a response.
#[cfg(test)]
pub fn format_post_commit_cli_error(
    response: &IpcResponse,
    command: &str,
    session: &str,
    envelope: ErrorEnvelope,
    pretty: bool,
) -> String {
    let followup_error = attach_authority_error_guidance(envelope);
    let data = Some(annotate_post_commit_local_failure_data(
        response.data.as_ref().unwrap_or(&Value::Null),
        &followup_error,
    ));
    let result = CommandResult {
        success: true,
        command: command.to_string(),
        stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
        request_id: response.request_id.clone(),
        command_id: response.command_id.clone(),
        session: session.to_string(),
        timing: response.timing,
        data,
        error: None,
    };
    serialize_checked_command_result(result, pretty)
}

/// Format a CLI-side failure that happened after the daemon had already
/// committed a response, but where the caller-visible command contract now
/// fails closed.
pub fn format_committed_cli_error(
    response: &IpcResponse,
    command: &str,
    session: &str,
    envelope: ErrorEnvelope,
    pretty: bool,
) -> String {
    let result = CommandResult {
        success: false,
        command: command.to_string(),
        stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
        request_id: response.request_id.clone(),
        command_id: response.command_id.clone(),
        session: session.to_string(),
        timing: response.timing,
        data: None,
        error: Some(attach_authority_error_guidance(envelope)),
    };
    serialize_checked_command_result(result, pretty)
}

#[cfg(test)]
fn annotate_post_commit_local_failure_data(data: &Value, followup_error: &ErrorEnvelope) -> Value {
    let followup_error = serde_json::to_value(followup_error)
        .unwrap_or_else(|_| json!({"code": "INTERNAL_ERROR", "message": "failed to serialize post-commit follow-up error"}));
    match data {
        Value::Object(object) => {
            let mut annotated = object.clone();
            // Keep the legacy string field as a compatibility mirror, but the
            // stdout contract now keys off the typed post_commit_followup_state
            // object instead of this magic value.
            annotated.insert(
                "commit_state".to_string(),
                Value::String(POST_COMMIT_LOCAL_FAILURE_STATE.to_string()),
            );
            annotated.insert(
                "post_commit_followup_state".to_string(),
                post_commit_followup_state_json(),
            );
            annotated.insert("post_commit_followup_error".to_string(), followup_error);
            Value::Object(annotated)
        }
        other => json!({
            // Compatibility mirror only; typed readers should use
            // post_commit_followup_state below.
            "commit_state": POST_COMMIT_LOCAL_FAILURE_STATE,
            "post_commit_followup_state": post_commit_followup_state_json(),
            "post_commit_followup_error": followup_error,
            "daemon_response": other,
        }),
    }
}

#[cfg(test)]
fn post_commit_followup_state_json() -> Value {
    json!({
        "surface": "cli_post_commit_followup_failure",
        "truth_level": "operator_projection",
        "projection_kind": "cli_post_commit_followup_failure",
        "projection_authority": "cli.post_commit_followup",
        "upstream_commit_truth": "daemon_response_committed",
        "control_role": "display_only",
        "durability": "best_effort",
        "recovery_contract": "no_public_recovery_contract",
    })
}

/// Format a CLI-side success result when no IPC response exists yet.
pub fn format_cli_success(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: serde_json::Value,
    pretty: bool,
    trace_mode: InteractionTraceMode,
) -> String {
    let mut result =
        CommandResult::success(command, session, uuid::Uuid::now_v7().to_string(), data);
    attach_interaction_trace(&mut result, trace_mode);
    attach_workflow_continuity(&mut result, rub_home);
    serialize_checked_command_result(result, pretty)
}

fn stdout_request_id(request_id: &str) -> String {
    if request_id.trim().is_empty() {
        uuid::Uuid::now_v7().to_string()
    } else {
        request_id.to_string()
    }
}

fn stdout_command_id(command_id: Option<&str>) -> Option<String> {
    command_id
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn interaction_effect_failure_result(result: &CommandResult) -> Option<CommandResult> {
    if projected_interaction_effect_success(result.success, result.data.as_ref()) {
        return None;
    }
    let data = result.data.as_ref()?;
    let confirmation_status = projected_interaction_confirmation_status(Some(data))?;
    let envelope =
        interaction_effect_failure_envelope(result.command.as_str(), data, confirmation_status);
    Some(CommandResult {
        success: false,
        command: result.command.clone(),
        stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
        request_id: result.request_id.clone(),
        command_id: result.command_id.clone(),
        session: result.session.clone(),
        timing: result.timing,
        data: None,
        error: Some(attach_authority_error_guidance(envelope)),
    })
}

fn interaction_effect_failure_envelope(
    command: &str,
    data: &serde_json::Value,
    confirmation_status: InteractionConfirmationStatus,
) -> ErrorEnvelope {
    let interaction = data
        .as_object()
        .and_then(|object| object.get("interaction"))
        .and_then(Value::as_object);

    let message = match confirmation_status {
        InteractionConfirmationStatus::Contradicted => {
            "Interaction actuation completed, but the observed browser effect contradicted the requested outcome"
        }
        InteractionConfirmationStatus::Degraded => {
            "Interaction actuation completed, but the browser-side effect could not be confirmed before the commit fence degraded"
        }
        InteractionConfirmationStatus::Unconfirmed => {
            "Interaction actuation completed, but the browser-side effect was not confirmed"
        }
        InteractionConfirmationStatus::Confirmed => {
            "Interaction actuation completed and the browser-side effect was confirmed"
        }
    };

    let mut context = Map::new();
    context.insert(
        "reason".to_string(),
        Value::String("interaction_effect_not_confirmed".to_string()),
    );
    context.insert(
        "effect_state".to_string(),
        interaction_effect_failure_state_json(confirmation_status),
    );
    context.insert("command".to_string(), Value::String(command.to_string()));
    context.insert("daemon_request_committed".to_string(), Value::Bool(true));
    context.insert("committed_response_projection".to_string(), data.clone());
    if let Some(kind) = interaction
        .and_then(|interaction| interaction.get("confirmation_kind"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
    {
        context.insert("confirmation_kind".to_string(), Value::String(kind));
    }
    ErrorEnvelope::new(ErrorCode::InteractionNotConfirmed, message)
        .with_context(Value::Object(context))
}

fn interaction_effect_failure_state_json(
    confirmation_status: InteractionConfirmationStatus,
) -> Value {
    let confirmation_status = match confirmation_status {
        InteractionConfirmationStatus::Confirmed => "confirmed",
        InteractionConfirmationStatus::Unconfirmed => "unconfirmed",
        InteractionConfirmationStatus::Contradicted => "contradicted",
        InteractionConfirmationStatus::Degraded => "degraded",
    };
    json!({
        "surface": INTERACTION_EFFECT_FAILURE_SURFACE,
        "truth_level": "operator_projection",
        "projection_kind": INTERACTION_EFFECT_FAILURE_SURFACE,
        "projection_authority": "cli.interaction_effect",
        "upstream_commit_truth": "daemon_response_committed",
        "control_role": "display_only",
        "durability": "best_effort",
        "recovery_contract": "no_public_recovery_contract",
        "confirmation_status": confirmation_status,
    })
}

fn checked_command_result(result: CommandResult) -> CommandResult {
    if let Some(envelope) = result.contract_error_envelope() {
        return CommandResult {
            success: false,
            command: result.command,
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: stdout_request_id(&result.request_id),
            command_id: stdout_command_id(result.command_id.as_deref()),
            session: result.session,
            timing: result.timing,
            data: None,
            error: Some(stdout_contract_fallback_error(
                attach_authority_error_guidance(envelope),
            )),
        };
    }
    result
}

fn serialize_checked_command_result(result: CommandResult, pretty: bool) -> String {
    serialize_command_result_json(&checked_command_result(result), pretty)
}

fn serialize_command_result_json(result: &CommandResult, pretty: bool) -> String {
    if pretty {
        serde_json::to_string_pretty(result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

fn attach_interaction_trace(result: &mut CommandResult, trace_mode: InteractionTraceMode) {
    if matches!(trace_mode, InteractionTraceMode::Compact) {
        return;
    }
    let Some(data) = result.data.as_mut() else {
        return;
    };
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get("interaction")
        .and_then(Value::as_object)
        .cloned()
    else {
        return;
    };

    let trace_id = result
        .command_id
        .clone()
        .unwrap_or_else(|| result.request_id.clone());

    let mut trace = Map::new();
    trace.insert("trace_id".to_string(), Value::String(trace_id));
    trace.insert("command".to_string(), Value::String(result.command.clone()));

    copy_field(&interaction, &mut trace, "semantic_class");
    copy_field(&interaction, &mut trace, "element_verified");
    copy_field(&interaction, &mut trace, "actuation");
    copy_field(&interaction, &mut trace, "interaction_confirmed");
    copy_field(&interaction, &mut trace, "confirmation_status");
    copy_field(&interaction, &mut trace, "confirmation_kind");

    if matches!(trace_mode, InteractionTraceMode::Trace)
        && let Some(effects) = summarize_observed_effects(&interaction, &trace)
    {
        trace.insert("observed_effects".to_string(), effects);
    }

    object.insert("interaction_trace".to_string(), Value::Object(trace));
}

fn attach_authority_error_guidance(mut envelope: ErrorEnvelope) -> ErrorEnvelope {
    let Some(context) = envelope.context.as_mut() else {
        return envelope;
    };
    let Some(guidance) = authority_error_guidance(envelope.code, context) else {
        return envelope;
    };
    if let Some(object) = context.as_object_mut() {
        object.insert("authority_guidance".to_string(), guidance);
    }
    envelope
}

fn stdout_contract_fallback_error(mut envelope: ErrorEnvelope) -> ErrorEnvelope {
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert("stdout_contract_fallback".to_string(), Value::Bool(true));
    context.insert(
        "projection_kind".to_string(),
        Value::String(STDOUT_CONTRACT_FALLBACK_SURFACE.to_string()),
    );
    context.insert(
        "projection_authority".to_string(),
        Value::String("cli.stdout_contract_fallback".to_string()),
    );
    context.insert(
        "truth_level".to_string(),
        Value::String("operator_projection".to_string()),
    );
    context.insert(
        "replay_identity_truth".to_string(),
        Value::String("absent".to_string()),
    );
    envelope.context = Some(Value::Object(context));
    envelope
}

fn authority_error_guidance(code: ErrorCode, context: &Value) -> Option<Value> {
    let object = context.as_object()?;
    let authority_state = object.get("authority_state").and_then(Value::as_str);
    let reason = object.get("reason").and_then(Value::as_str);

    match (code, authority_state, reason) {
        (ErrorCode::StaleSnapshot, Some("selected_frame_context_drifted"), _) => {
            Some(authority_guidance(
                "selected_frame_context_drifted",
                "The selected frame changed after the snapshot was captured. Re-select the current frame and refresh snapshot authority before continuing.",
                vec![
                    guidance_command_hint(
                        "rub frames",
                        "inspect the live frame inventory before choosing the current frame again",
                    ),
                    guidance_command_hint(
                        "rub frame --top or rub frame --name <frame-name>",
                        "restore the intended frame selection before taking a fresh snapshot",
                    ),
                    guidance_command_hint(
                        "rub state",
                        "capture a fresh snapshot after frame authority has been restored",
                    ),
                ],
            ))
        }
        (ErrorCode::StaleSnapshot, Some("selected_frame_context_stale"), _) => {
            Some(authority_guidance(
                "selected_frame_context_stale",
                "The selected frame context is no longer live. Re-establish frame authority and capture a fresh snapshot before continuing.",
                vec![
                    guidance_command_hint(
                        "rub frames",
                        "inspect the live frame inventory to confirm which frames still exist",
                    ),
                    guidance_command_hint(
                        "rub frame --top or rub frame --name <frame-name>",
                        "select the intended live frame again before retrying",
                    ),
                    guidance_command_hint(
                        "rub state",
                        "capture a fresh snapshot from the restored frame context",
                    ),
                ],
            ))
        }
        (ErrorCode::StaleSnapshot, Some("explicit_frame_scope_mismatch"), _) => {
            Some(authority_guidance(
                "explicit_frame_scope_mismatch",
                "The cached snapshot does not match the explicitly requested frame scope. Re-select the intended frame and capture a fresh snapshot before continuing.",
                vec![
                    guidance_command_hint(
                        "rub frames",
                        "inspect the live frame inventory and confirm the intended frame name or id",
                    ),
                    guidance_command_hint(
                        "rub frame --name <frame-name> or rub frame --top",
                        "restore the explicit frame scope before retrying the command",
                    ),
                    guidance_command_hint(
                        "rub state",
                        "capture a fresh snapshot after restoring the intended frame scope",
                    ),
                ],
            ))
        }
        (ErrorCode::BrowserCrashed, _, Some("continuity_no_active_tab"))
        | (ErrorCode::SessionBusy, _, Some("continuity_no_active_tab"))
        | (ErrorCode::SessionBusy, _, Some("continuity_target_tab_missing"))
        | (ErrorCode::SessionBusy, _, Some("continuity_tab_refresh_failed")) => {
            Some(authority_guidance(
                match reason {
                    Some("continuity_target_tab_missing") => "continuity_target_tab_missing",
                    Some("continuity_tab_refresh_failed") => "continuity_tab_refresh_failed",
                    _ => "continuity_no_active_tab",
                },
                "Tab authority became unavailable during continuity recovery. Re-establish the intended tab before continuing automation.",
                vec![
                    guidance_command_hint(
                        "rub tabs",
                        "inspect the current tab registry and confirm which tab, if any, still owns the workflow",
                    ),
                    guidance_command_hint(
                        "rub switch <index>",
                        "restore the intended active tab before attempting to resume",
                    ),
                    guidance_command_hint(
                        "rub runtime takeover",
                        "re-check the takeover runtime surface after tab authority is restored",
                    ),
                ],
            ))
        }
        (ErrorCode::BrowserCrashed, _, Some("continuity_frame_unavailable"))
        | (ErrorCode::SessionBusy, _, Some("continuity_frame_unavailable")) => {
            Some(authority_guidance(
                "continuity_frame_unavailable",
                "Frame authority became unavailable during continuity recovery. Re-select the intended frame and refresh page authority before continuing.",
                vec![
                    guidance_command_hint(
                        "rub frames",
                        "inspect the live frame inventory before choosing the current frame again",
                    ),
                    guidance_command_hint(
                        "rub frame --top or rub frame --name <frame-name>",
                        "restore the intended frame selection before retrying",
                    ),
                    guidance_command_hint(
                        "rub runtime takeover",
                        "re-check the takeover runtime surface after frame authority is restored",
                    ),
                ],
            ))
        }
        _ => None,
    }
}

fn authority_guidance(source_signal: &str, summary: &str, next_command_hints: Vec<Value>) -> Value {
    json!({
        "surface": "cli_authority_guidance",
        "truth_level": "operator_projection",
        "projection_kind": "authority_guidance",
        "projection_authority": "cli.output.error_guidance",
        "upstream_commit_truth": "error_context",
        "control_role": "guidance_only",
        "durability": "ephemeral",
        "source_signal": source_signal,
        "summary": summary,
        "next_command_hints": next_command_hints,
    })
}

fn guidance_command_hint(command: &str, reason: &str) -> Value {
    json!({
        "command": command,
        "reason": reason,
    })
}

fn copy_field(source: &Map<String, Value>, dest: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key) {
        dest.insert(key.to_string(), value.clone());
    }
}

fn summarize_observed_effects(
    interaction: &Map<String, Value>,
    trace: &Map<String, Value>,
) -> Option<Value> {
    let mut observed = interaction
        .get("observed_effects")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for (key, value) in interaction {
        if key != "observed_effects" && !trace.contains_key(key) {
            observed.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    if observed.is_empty() {
        None
    } else {
        Some(Value::Object(observed))
    }
}

#[cfg(test)]
mod tests;
