use super::*;
use crate::trigger_workflow_bridge::{
    resolve_trigger_workflow_parameterization, trigger_workflow_source_var_keys,
};
use crate::workflow_assets::{
    annotate_workflow_asset_path_state, load_named_workflow_spec_with_authority,
    resolve_named_workflow_path, workflow_asset_path_state,
};

pub(super) async fn fire_trigger(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    tabs: &[TabInfo],
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    command_id: &str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    let target_tab = resolve_bound_tab(tabs, &trigger.target_tab.target_id)
        .map_err(|error| ErrorEnvelope::new(ErrorCode::TabNotFound, error.to_string()))?;

    if !target_tab.active {
        let switch_request = IpcRequest::new(
            "switch",
            serde_json::json!({
                "index": target_tab.index,
                "_trigger": trigger_request_meta(trigger, evidence, "target_switch"),
            }),
            TRIGGER_ACTION_BASE_TIMEOUT_MS,
        );
        let response = router
            .dispatch_within_active_transaction(switch_request, state)
            .await;
        ensure_trigger_response_success(response)?;
    }

    ensure_trigger_target_continuity(router, state, &trigger.target_tab.target_id).await?;

    match trigger.action.kind {
        TriggerActionKind::BrowserCommand => {
            fire_browser_command_trigger(router, state, trigger, evidence, command_id).await
        }
        TriggerActionKind::Workflow => {
            fire_workflow_trigger(router, state, trigger, evidence, command_id).await
        }
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Err(ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "trigger action.kind is not yet executable in this runtime slice",
            ))
        }
    }
}

async fn fire_browser_command_trigger(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    command_id: &str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    let command = trigger.action.command.as_deref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger browser_command action is missing action.command",
        )
    })?;
    let mut payload = trigger
        .action
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let object = payload.as_object_mut().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger browser_command action payload must be a JSON object",
        )
    })?;
    object.insert(
        "_trigger".to_string(),
        trigger_request_meta(trigger, evidence, "action"),
    );

    let response = router
        .dispatch_within_active_transaction(
            {
                let args = serde_json::Value::Object(object.clone());
                IpcRequest::new(
                    command,
                    args.clone(),
                    trigger_action_timeout_ms(command, &args),
                )
                .with_command_id(command_id)
                .map_err(|reason| {
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        format!("trigger action command_id is not protocol-valid: {reason}"),
                    )
                })?
            },
            state,
        )
        .await;
    let data = ensure_trigger_response_success(response)?;
    ensure_committed_automation_result(command, data.as_ref())?;
    Ok(data)
}

async fn fire_workflow_trigger(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    command_id: &str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    let payload = trigger.action.payload.as_ref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger workflow action is missing action.payload",
        )
    })?;
    let object = payload.as_object().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger workflow payload must be a JSON object",
        )
    })?;

    let (raw_spec, mut spec_source) = resolve_trigger_workflow_spec(object, &state.rub_home)
        .map_err(|error| error.into_envelope())?;
    let parameterized = resolve_trigger_workflow_parameterization(
        &router.browser_port(),
        &trigger.source_tab.target_id,
        None,
        object,
        &raw_spec,
    )
    .await
    .map_err(|error| error.into_envelope())?;
    if let Some(spec_source_object) = spec_source.as_object_mut() {
        spec_source_object.insert(
            "vars".to_string(),
            serde_json::json!(parameterized.parameter_keys),
        );
    }

    let args = serde_json::json!({
        "spec": parameterized.resolved_spec,
        "spec_source": spec_source,
        "_trigger": trigger_request_meta(trigger, evidence, "action"),
    });
    let response = router
        .dispatch_within_active_transaction(
            IpcRequest::new(
                "pipe",
                args.clone(),
                trigger_action_timeout_ms("pipe", &args),
            )
            .with_command_id(command_id)
            .map_err(|reason| {
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!("trigger action command_id is not protocol-valid: {reason}"),
                )
            })?,
            state,
        )
        .await;
    ensure_trigger_response_success(response)
}

pub(super) fn resolve_trigger_workflow_spec(
    payload: &serde_json::Map<String, serde_json::Value>,
    rub_home: &std::path::Path,
) -> Result<(String, serde_json::Value), RubError> {
    match (
        payload
            .get("workflow_name")
            .and_then(|value| value.as_str()),
        payload.get("steps"),
    ) {
        (Some(name), None) => {
            let (normalized, contents, path) = load_named_workflow_spec_with_authority(
                rub_home,
                name,
                "trigger.workflow.spec_source.path",
                "trigger_workflow_payload.workflow_name",
            )?;
            let mut spec_source = serde_json::json!({
                "kind": "workflow",
                "name": normalized,
                "path": path.display().to_string(),
            });
            annotate_workflow_asset_path_state(
                &mut spec_source,
                "path_state",
                "trigger.workflow.spec_source.path",
                "trigger_workflow_payload.workflow_name",
            );
            Ok((contents, spec_source))
        }
        (None, Some(steps)) if steps.is_array() => {
            let raw_steps = serde_json::to_string(steps).map_err(RubError::from)?;
            Ok((
                raw_steps,
                serde_json::json!({
                    "kind": "trigger_inline_workflow",
                    "step_count": steps.as_array().map(|steps| steps.len()).unwrap_or(0),
                }),
            ))
        }
        (Some(_), Some(_)) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload must provide exactly one of payload.workflow_name or payload.steps",
        )),
        _ => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload requires non-empty payload.workflow_name or payload.steps",
        )),
    }
}

async fn ensure_trigger_target_continuity(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    target_id: &str,
) -> Result<(), ErrorEnvelope> {
    let browser = router.browser_port();
    let tabs = refresh_live_trigger_runtime(&browser, state)
        .await
        .map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                format!("trigger target continuity refresh failed: {error}"),
            )
            .with_context(serde_json::json!({
                "reason": "continuity_tab_refresh_failed",
                "target_tab_target_id": target_id,
            }))
        })?;
    let target_tab = resolve_bound_tab(&tabs, target_id).map_err(|error| {
        ErrorEnvelope::new(ErrorCode::TabNotFound, error.to_string()).with_context(
            serde_json::json!({
                "reason": "continuity_target_tab_missing",
                "target_tab_target_id": target_id,
            }),
        )
    })?;
    if !target_tab.active {
        return Err(ErrorEnvelope::new(
            ErrorCode::BrowserCrashed,
            "Trigger target continuity fence failed: target tab is not active after switch",
        )
        .with_context(serde_json::json!({
            "reason": "continuity_target_not_active",
            "target_tab_target_id": target_id,
            "target_tab_index": target_tab.index,
        })));
    }

    refresh_live_runtime_state(&browser, state).await;
    refresh_live_frame_runtime(&browser, state).await;
    let frame_runtime = state.frame_runtime().await;
    let readiness = state.readiness_state().await;
    if let Some((reason, message)) =
        trigger_target_continuity_failure(target_id, &frame_runtime, &readiness)
    {
        return Err(
            ErrorEnvelope::new(ErrorCode::BrowserCrashed, message).with_context(
                serde_json::json!({
                    "reason": reason,
                    "target_tab_target_id": target_id,
                    "frame_runtime": frame_runtime,
                    "readiness_state": readiness,
                }),
            ),
        );
    }

    Ok(())
}

fn ensure_trigger_response_success(
    response: rub_ipc::protocol::IpcResponse,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    match response.status {
        ResponseStatus::Success => Ok(response.data),
        ResponseStatus::Error => Err(response.error.unwrap_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                "trigger action returned an error response without an error envelope",
            )
        })),
    }
}

pub(super) fn resolve_bound_tab<'a>(
    tabs: &'a [TabInfo],
    target_id: &str,
) -> Result<&'a TabInfo, RubError> {
    tabs.iter()
        .find(|tab| tab.target_id == target_id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::TabNotFound,
                format!("Tab target '{target_id}' is not present in the current session"),
            )
        })
}

pub(super) fn trigger_target_continuity_failure(
    target_tab_id: &str,
    frame_runtime: &rub_core::model::FrameRuntimeInfo,
    readiness: &ReadinessInfo,
) -> Option<(&'static str, &'static str)> {
    if matches!(
        frame_runtime.status,
        rub_core::model::FrameContextStatus::Unknown
            | rub_core::model::FrameContextStatus::Stale
            | rub_core::model::FrameContextStatus::Degraded
    ) || frame_runtime.current_frame.is_none()
    {
        return Some((
            "continuity_frame_unavailable",
            "Trigger target continuity fence failed: frame context became unavailable",
        ));
    }
    if frame_runtime
        .current_frame
        .as_ref()
        .and_then(|frame| frame.target_id.as_deref())
        != Some(target_tab_id)
    {
        return Some((
            "continuity_frame_target_mismatch",
            "Trigger target continuity fence failed: frame context no longer matches the target tab authority",
        ));
    }
    if matches!(readiness.status, rub_core::model::ReadinessStatus::Degraded) {
        return Some((
            "continuity_readiness_degraded",
            "Trigger target continuity fence failed: readiness surface degraded",
        ));
    }
    None
}

pub(super) fn trigger_action_summary(trigger: &TriggerInfo) -> String {
    match trigger.action.kind {
        TriggerActionKind::BrowserCommand => format!(
            "'{}'",
            trigger
                .action
                .command
                .as_deref()
                .unwrap_or("browser_command")
        ),
        TriggerActionKind::Workflow => trigger
            .action
            .payload
            .as_ref()
            .and_then(|payload| payload.get("workflow_name"))
            .and_then(|value| value.as_str())
            .map(|name| format!("workflow '{name}'"))
            .unwrap_or_else(|| "inline workflow".to_string()),
        TriggerActionKind::Provider => "provider action".to_string(),
        TriggerActionKind::Script => "script action".to_string(),
        TriggerActionKind::Webhook => "webhook action".to_string(),
    }
}

fn trigger_action_timeout_ms(command: &str, args: &serde_json::Value) -> u64 {
    TRIGGER_ACTION_BASE_TIMEOUT_MS.saturating_add(
        rub_core::automation_timeout::command_additional_timeout_ms(command, args),
    )
}

pub(super) fn trigger_action_command_id(
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    trigger.id.hash(&mut hasher);
    super::trigger_evidence_consumption_key(evidence).hash(&mut hasher);
    format!("trigger:{}:{:016x}", trigger.id, hasher.finish())
}

pub(super) fn trigger_action_execution_info(
    trigger: &TriggerInfo,
    rub_home: &std::path::Path,
) -> TriggerActionExecutionInfo {
    let mut vars = trigger
        .action
        .payload
        .as_ref()
        .and_then(|payload| payload.get("vars"))
        .and_then(|vars| vars.as_object())
        .map(|vars| vars.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    vars.sort();
    let source_vars = trigger
        .action
        .payload
        .as_ref()
        .and_then(|payload| payload.as_object())
        .and_then(|payload| trigger_workflow_source_var_keys(payload).ok())
        .unwrap_or_default();

    match trigger.action.kind {
        TriggerActionKind::BrowserCommand => TriggerActionExecutionInfo {
            kind: TriggerActionKind::BrowserCommand,
            command: trigger.action.command.clone(),
            workflow_name: None,
            workflow_path: None,
            workflow_path_state: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
        TriggerActionKind::Workflow => {
            let workflow_name = trigger
                .action
                .payload
                .as_ref()
                .and_then(|payload| payload.get("workflow_name"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let workflow_path = workflow_name
                .as_deref()
                .and_then(|name| resolve_named_workflow_path(rub_home, name).ok())
                .map(|path| path.display().to_string());
            let workflow_path_state = workflow_path.as_ref().map(|_| {
                workflow_asset_path_state(
                    "automation.action.workflow_path",
                    "trigger_action_payload.workflow_name",
                )
            });
            let inline_step_count = trigger
                .action
                .payload
                .as_ref()
                .and_then(|payload| payload.get("steps"))
                .and_then(|steps| steps.as_array())
                .map(|steps| steps.len() as u32);
            TriggerActionExecutionInfo {
                kind: TriggerActionKind::Workflow,
                command: None,
                workflow_name,
                workflow_path,
                workflow_path_state,
                inline_step_count,
                vars,
                source_vars,
            }
        }
        TriggerActionKind::Provider => TriggerActionExecutionInfo {
            kind: TriggerActionKind::Provider,
            command: None,
            workflow_name: None,
            workflow_path: None,
            workflow_path_state: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
        TriggerActionKind::Script => TriggerActionExecutionInfo {
            kind: TriggerActionKind::Script,
            command: None,
            workflow_name: None,
            workflow_path: None,
            workflow_path_state: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
        TriggerActionKind::Webhook => TriggerActionExecutionInfo {
            kind: TriggerActionKind::Webhook,
            command: None,
            workflow_name: None,
            workflow_path: None,
            workflow_path_state: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
    }
}

fn trigger_request_meta(
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    phase: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": trigger.id,
        "phase": phase,
        "source_tab_target_id": trigger.source_tab.target_id,
        "target_tab_target_id": trigger.target_tab.target_id,
        "condition_kind": trigger.condition.kind,
        "evidence": evidence,
    })
}
