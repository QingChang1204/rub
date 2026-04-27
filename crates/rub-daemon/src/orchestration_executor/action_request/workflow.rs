use super::protocol::RemoteDispatchContract;
use super::*;
use crate::router::TransactionDeadline;
use crate::trigger_workflow_bridge::{
    resolve_trigger_workflow_source_bindings, trigger_workflow_source_var_keys,
};
use crate::workflow_assets::{
    annotate_workflow_asset_path_state, load_named_workflow_spec,
    load_named_workflow_spec_with_authority, workflow_asset_path_state,
};
use crate::workflow_params::{
    parse_workflow_json_parameter_bindings, resolve_workflow_binding_map,
};
use rub_core::json_spec::NormalizedJsonSpec;
use serde_json::Value;
use std::path::Path;

pub(crate) const SOURCE_MATERIALIZATION_TIMEOUT_SENTINEL: &str =
    "__rub_source_materialization_timeout_sentinel__";

pub(crate) fn orchestration_action_timeout_ms(command: &str, args: &serde_json::Value) -> u64 {
    ORCHESTRATION_ACTION_BASE_TIMEOUT_MS.saturating_add(
        rub_core::automation_timeout::command_additional_timeout_ms(command, args),
    )
}

pub(crate) fn source_materialization_timeout_authority_error(path: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidInput,
        "orchestration source_vars cannot drive timeout-sensitive workflow fields during source materialization",
    )
    .with_context(serde_json::json!({
        "reason": "orchestration_source_materialization_timeout_authority_ambiguous",
        "path": path,
    }))
}

pub(crate) fn inspect_timeout_sensitive_value(
    value: Option<&Value>,
    path: &str,
) -> Result<(), ErrorEnvelope> {
    if let Some(text) = value.and_then(Value::as_str)
        && text.contains(SOURCE_MATERIALIZATION_TIMEOUT_SENTINEL)
    {
        return Err(source_materialization_timeout_authority_error(path));
    }
    Ok(())
}

pub(crate) fn normalized_timeout_spec_value(
    spec: &Value,
    scope: &str,
) -> Result<Value, ErrorEnvelope> {
    if let Some(raw) = spec.as_str() {
        return serde_json::from_str(raw)
            .map_err(|_| source_materialization_timeout_authority_error(scope));
    }
    Ok(spec.clone())
}

pub(crate) fn value_contains_timeout_sentinel(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains(SOURCE_MATERIALIZATION_TIMEOUT_SENTINEL),
        Value::Array(values) => values.iter().any(value_contains_timeout_sentinel),
        Value::Object(map) => map.values().any(value_contains_timeout_sentinel),
        _ => false,
    }
}

pub(crate) fn inspect_fill_workflow_timeout_authority(
    spec: &Value,
    scope: &str,
) -> Result<(), ErrorEnvelope> {
    if !value_contains_timeout_sentinel(spec) {
        return Ok(());
    }

    let steps = normalized_timeout_spec_value(spec, scope)?;
    let Some(steps) = steps.as_array() else {
        return Err(source_materialization_timeout_authority_error(scope));
    };

    for (index, step) in steps.iter().enumerate() {
        inspect_timeout_sensitive_value(
            step.get("wait_after")
                .and_then(|wait| wait.get("timeout_ms")),
            &format!("{scope}[{index}].wait_after.timeout_ms"),
        )?;
    }

    Ok(())
}

pub(crate) fn normalized_resolved_workflow_spec_value(
    resolved_spec: &str,
) -> Result<Value, ErrorEnvelope> {
    NormalizedJsonSpec::from_raw_str(resolved_spec, "pipe")
        .map(NormalizedJsonSpec::into_value)
        .map_err(|error| ErrorEnvelope::new(ErrorCode::InvalidInput, error.to_string()))
}

pub(crate) fn inspect_pipe_workflow_timeout_authority(
    spec: &Value,
    scope: &str,
) -> Result<(), ErrorEnvelope> {
    if !value_contains_timeout_sentinel(spec) {
        return Ok(());
    }

    let workflow = normalized_timeout_spec_value(spec, scope)?;
    let (steps, steps_scope) = if let Some(steps) = workflow.as_array() {
        (steps, scope.to_string())
    } else if let Some(steps) = workflow.get("steps").and_then(Value::as_array) {
        (steps, format!("{scope}.steps"))
    } else {
        return Err(source_materialization_timeout_authority_error(scope));
    };

    for (index, step) in steps.iter().enumerate() {
        let step_scope = format!("{steps_scope}[{index}]");
        inspect_timeout_sensitive_value(step.get("command"), &format!("{step_scope}.command"))?;
        let command = step
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = step.get("args").unwrap_or(&Value::Null);
        inspect_timeout_sensitive_value(
            args.get("wait_after")
                .and_then(|wait| wait.get("timeout_ms")),
            &format!("{step_scope}.args.wait_after.timeout_ms"),
        )?;
        match command {
            "wait" => inspect_timeout_sensitive_value(
                args.get("timeout_ms"),
                &format!("{step_scope}.args.timeout_ms"),
            )?,
            "fill" => {
                if let Some(spec) = args.get("spec") {
                    inspect_fill_workflow_timeout_authority(
                        spec,
                        &format!("{step_scope}.args.spec"),
                    )?;
                }
            }
            "pipe" => {
                if let Some(spec) = args.get("spec") {
                    inspect_pipe_workflow_timeout_authority(
                        spec,
                        &format!("{step_scope}.args.spec"),
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

pub(crate) fn ensure_static_source_materialization_timeout_authority(
    resolved_spec: &Value,
) -> Result<(), ErrorEnvelope> {
    inspect_pipe_workflow_timeout_authority(resolved_spec, "$")
}

pub(crate) fn orchestration_source_materialization_wait_budget_ms(
    action: &TriggerActionSpec,
    rub_home: &Path,
) -> Result<u64, ErrorEnvelope> {
    match action.kind {
        TriggerActionKind::BrowserCommand => {
            let command = action.command.as_deref().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration browser_command action is missing action.command",
                )
            })?;
            let payload = action
                .payload
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            let args = payload.as_object().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration browser_command action payload must be a JSON object",
                )
            })?;
            Ok(orchestration_action_timeout_ms(
                command,
                &Value::Object(args.clone()),
            ))
        }
        TriggerActionKind::Workflow => {
            let payload = action.payload.as_ref().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration workflow action is missing action.payload",
                )
            })?;
            let object = payload.as_object().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration workflow action payload must be a JSON object",
                )
            })?;
            let (raw_spec, _) = resolve_orchestration_workflow_spec(object, rub_home)?;
            let explicit = object
                .get("vars")
                .and_then(|value| value.as_object())
                .map(parse_workflow_json_parameter_bindings)
                .transpose()
                .map_err(|error| error.into_envelope())?;
            let mut bindings = explicit.unwrap_or_default();
            for key in
                trigger_workflow_source_var_keys(object).map_err(|error| error.into_envelope())?
            {
                bindings
                    .entry(key)
                    .or_insert_with(|| SOURCE_MATERIALIZATION_TIMEOUT_SENTINEL.to_string());
            }
            let parameterized = resolve_workflow_binding_map(&raw_spec, &bindings)
                .map_err(|error| error.into_envelope())?;
            let resolved_spec =
                normalized_resolved_workflow_spec_value(&parameterized.resolved_spec)?;
            ensure_static_source_materialization_timeout_authority(&resolved_spec)?;
            let args = serde_json::json!({
                "spec": resolved_spec,
            });
            Ok(orchestration_action_timeout_ms("pipe", &args))
        }
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Err(ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "orchestration action.kind is not yet executable in this runtime slice",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_action_kind_not_supported",
                "kind": action.kind,
            })))
        }
    }
}

pub(crate) async fn resolve_orchestration_workflow_parameterization(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    payload: &serde_json::Map<String, serde_json::Value>,
    raw_spec: &str,
    outer_deadline: Option<TransactionDeadline>,
) -> Result<
    (
        crate::workflow_params::WorkflowParameterization,
        Vec<String>,
    ),
    ErrorEnvelope,
> {
    let explicit = payload
        .get("vars")
        .and_then(|value| value.as_object())
        .map(parse_workflow_json_parameter_bindings)
        .transpose()
        .map_err(|error| error.into_envelope())?;
    let mut bindings = explicit.unwrap_or_default();
    let source_bindings = resolve_orchestration_workflow_source_bindings(
        router,
        state,
        runtime,
        rule,
        payload,
        outer_deadline,
    )
    .await?;
    let mut source_var_keys = source_bindings.keys().cloned().collect::<Vec<_>>();
    source_var_keys.sort();
    for (name, value) in source_bindings {
        if bindings.insert(name.clone(), value).is_some() {
            return Err(ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                format!(
                    "orchestration workflow parameter '{name}' is defined by both payload.vars and payload.source_vars"
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_workflow_duplicate_var_binding",
                "name": name,
            })));
        }
    }
    let parameterized =
        resolve_workflow_binding_map(raw_spec, &bindings).map_err(|error| error.into_envelope())?;
    Ok((parameterized, source_var_keys))
}

pub(crate) async fn resolve_orchestration_workflow_source_bindings(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    payload: &serde_json::Map<String, serde_json::Value>,
    outer_deadline: Option<TransactionDeadline>,
) -> Result<std::collections::BTreeMap<String, String>, ErrorEnvelope> {
    if payload.get("source_vars").is_none() {
        return Ok(Default::default());
    }

    let source_target_id = rule.source.tab_target_id.as_deref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "orchestration workflow source_vars require source.tab_target_id to be bound",
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_source_tab_target_missing",
            "source_session_id": rule.source.session_id,
            "source_session_name": rule.source.session_name,
        }))
    })?;

    if rule.source.session_id == state.session_id {
        return run_orchestration_future_with_outer_deadline(
            outer_deadline,
            || {
                source_var_timeout_budget_exhausted_error(
                    &rule.source.session_id,
                    &rule.source.session_name,
                )
            },
            async {
                resolve_trigger_workflow_source_bindings(
                    &router.browser_port(),
                    source_target_id,
                    rule.source.frame_id.as_deref(),
                    payload,
                )
                .await
                .map_err(|error| error.into_envelope())
            },
        )
        .await;
    }

    let source_session = resolve_source_session(runtime, rule)?;
    dispatch_to_source_session_for_workflow_bindings(
        source_session,
        source_target_id,
        rule.source.frame_id.as_deref(),
        payload,
        outer_deadline,
    )
    .await
}

pub(crate) fn resolve_source_session<'a>(
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> Result<&'a OrchestrationSessionInfo, ErrorEnvelope> {
    let session = runtime
        .known_sessions
        .iter()
        .find(|session| session.session_id == rule.source.session_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Source session '{}' is not available for orchestration workflow parameterization",
                    rule.source.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_session_missing",
                "source_session_id": rule.source.session_id,
                "source_session_name": rule.source.session_name,
            }))
        })?;
    if crate::orchestration_runtime::orchestration_session_addressability_reason(session).is_some()
    {
        return Err(
            crate::orchestration_runtime::orchestration_session_not_addressable_error(
                session,
                ErrorCode::SessionBusy,
                format!(
                    "Source session '{}' is still present but not addressable for orchestration workflow parameterization",
                    rule.source.session_name
                ),
                "orchestration_source_session_not_addressable",
                "source_session_id",
                "source_session_name",
            ),
        );
    }
    Ok(session)
}

pub(crate) async fn dispatch_to_source_session_for_workflow_bindings(
    session: &OrchestrationSessionInfo,
    source_target_id: &str,
    source_frame_id: Option<&str>,
    payload: &serde_json::Map<String, serde_json::Value>,
    outer_deadline: Option<TransactionDeadline>,
) -> Result<std::collections::BTreeMap<String, String>, ErrorEnvelope> {
    let timeout_ms =
        bounded_orchestration_timeout_ms(ORCHESTRATION_ACTION_BASE_TIMEOUT_MS, outer_deadline)
            .ok_or_else(|| {
                source_var_timeout_budget_exhausted_error(
                    &session.session_id,
                    &session.session_name,
                )
            })?;
    let request = crate::orchestration_executor::bind_live_orchestration_phase_command_id(
        IpcRequest::new(
            "_orchestration_workflow_source_vars",
            serde_json::json!({
                "tab_target_id": source_target_id,
                "frame_id": source_frame_id,
                "payload": payload,
            }),
            timeout_ms,
        ),
        "orchestration_source_workflow_source_vars",
    )?;
    let response = dispatch_remote_orchestration_request(
        session,
        "source",
        request,
        RemoteDispatchContract {
            dispatch_subject: "workflow source vars",
            unreachable_reason: "orchestration_source_session_unreachable",
            transport_failure_reason: "orchestration_source_var_dispatch_transport_failed",
            protocol_failure_reason: "orchestration_source_var_dispatch_protocol_failed",
            missing_error_message:
                "remote orchestration workflow source vars returned an error without an envelope",
        },
    )
    .await?;

    decode_orchestration_success_payload(
        response,
        session,
        "orchestration_source_var_payload_missing",
        "orchestration workflow source vars returned success without a payload",
        "orchestration_source_var_payload_invalid",
        "orchestration workflow source vars payload",
    )
}

fn source_var_timeout_budget_exhausted_error(
    session_id: &str,
    session_name: &str,
) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        "Orchestration workflow source_vars exhausted the caller-owned timeout budget before authoritative source reads completed",
    )
    .with_context(serde_json::json!({
        "reason": "orchestration_source_var_timeout_budget_exhausted",
        "source_session_id": session_id,
        "source_session_name": session_name,
    }))
}

pub(crate) fn resolve_orchestration_workflow_spec(
    payload: &serde_json::Map<String, serde_json::Value>,
    rub_home: &Path,
) -> Result<(String, serde_json::Value), ErrorEnvelope> {
    match (
        payload.get("workflow_name").and_then(|value| value.as_str()),
        payload.get("steps"),
    ) {
        (Some(name), None) => {
            let (normalized, contents, path) = load_named_workflow_spec_with_authority(
                rub_home,
                name,
                "orchestration.workflow.spec_source.path",
                "orchestration_workflow_payload.workflow_name",
            )
            .map_err(|error| error.into_envelope())?;
            let mut spec_source = serde_json::json!({
                "kind": "orchestration_workflow",
                "name": normalized,
                "path": path.display().to_string(),
            });
            annotate_workflow_asset_path_state(
                &mut spec_source,
                "path_state",
                "orchestration.workflow.spec_source.path",
                "orchestration_workflow_payload.workflow_name",
            );
            Ok((contents, spec_source))
        }
        (None, Some(steps)) if steps.is_array() => {
            let raw_steps = serde_json::to_string(steps).map_err(|error| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    format!("Failed to serialize orchestration inline workflow: {error}"),
                )
            })?;
            Ok((
                raw_steps,
                serde_json::json!({
                    "kind": "orchestration_inline_workflow",
                    "step_count": steps.as_array().map(|steps| steps.len()).unwrap_or(0),
                }),
            ))
        }
        (Some(_), Some(_)) => Err(
            ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "orchestration workflow payload must provide exactly one of payload.workflow_name or payload.steps",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_workflow_shape_invalid",
            })),
        ),
        _ => Err(
            ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "orchestration workflow payload requires non-empty payload.workflow_name or payload.steps",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_workflow_shape_invalid",
            })),
        ),
    }
}

pub(crate) fn orchestration_step_command_id(
    rule: &OrchestrationRuleInfo,
    command_identity_key: Option<&str>,
    execution_id: &str,
    step_index: u32,
) -> String {
    if let Some(identity_key) = command_identity_key {
        format!(
            "orchestration:{}:{}:{}:{}",
            rule.idempotency_key, identity_key, execution_id, step_index
        )
    } else {
        format!(
            "orchestration:{}:{}:{}",
            rule.idempotency_key, execution_id, step_index
        )
    }
}

pub(crate) fn orchestration_request_meta(
    rule: &OrchestrationRuleInfo,
    command_identity_key: Option<&str>,
    execution_id: &str,
    step_index: u32,
    phase: &str,
) -> serde_json::Value {
    let command_id =
        orchestration_step_command_id(rule, command_identity_key, execution_id, step_index);
    serde_json::json!({
        "id": rule.id,
        "execution_id": execution_id,
        "command_identity_kind": if command_identity_key.is_some() {
            "execution_scoped_evidence_key"
        } else {
            "execution_attempt"
        },
        "command_identity_key": command_identity_key,
        "phase": phase,
        "step_index": step_index,
        "command_id": command_id,
        "correlation_key": rule.correlation_key,
        "idempotency_key": rule.idempotency_key,
        "source_session_id": rule.source.session_id,
        "target_session_id": rule.target.session_id,
        "target_tab_target_id": rule.target.tab_target_id,
        "frame_id": rule.target.frame_id,
    })
}

pub(crate) fn orchestration_action_execution_info(
    action: &TriggerActionSpec,
    rub_home: &Path,
) -> Result<OrchestrationActionExecutionInfo, ErrorEnvelope> {
    let mut vars = action
        .payload
        .as_ref()
        .and_then(|payload| payload.get("vars"))
        .and_then(|vars| vars.as_object())
        .map(|vars| vars.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    vars.sort();
    let source_vars = action
        .payload
        .as_ref()
        .and_then(|payload| payload.as_object())
        .and_then(|payload| trigger_workflow_source_var_keys(payload).ok())
        .unwrap_or_default();

    match action.kind {
        TriggerActionKind::BrowserCommand => Ok(OrchestrationActionExecutionInfo {
            kind: TriggerActionKind::BrowserCommand,
            command: action.command.clone(),
            workflow_name: None,
            workflow_path: None,
            workflow_path_state: None,
            inline_step_count: None,
            vars,
            source_vars,
        }),
        TriggerActionKind::Workflow => {
            let payload = action.payload.as_ref().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration workflow action is missing action.payload",
                )
            })?;
            let workflow_name = payload
                .get("workflow_name")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let workflow_path = workflow_name
                .as_deref()
                .and_then(|name| load_named_workflow_spec(rub_home, name).ok())
                .map(|(_, _, path)| path.display().to_string());
            let workflow_path_state = workflow_path.as_ref().map(|_| {
                workflow_asset_path_state(
                    "automation.action.workflow_path",
                    "orchestration_action_payload.workflow_name",
                )
            });
            let inline_step_count = payload
                .get("steps")
                .and_then(|value| value.as_array())
                .map(|steps| steps.len() as u32);
            Ok(OrchestrationActionExecutionInfo {
                kind: TriggerActionKind::Workflow,
                command: None,
                workflow_name,
                workflow_path,
                workflow_path_state,
                inline_step_count,
                vars,
                source_vars,
            })
        }
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Ok(OrchestrationActionExecutionInfo {
                kind: action.kind,
                command: action.command.clone(),
                workflow_name: None,
                workflow_path: None,
                workflow_path_state: None,
                inline_step_count: None,
                vars,
                source_vars,
            })
        }
    }
}
