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
        .map_err(|_| trigger_target_tab_missing_error(&trigger.target_tab.target_id))?;

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
        ensure_trigger_response_success(response, trigger, "target_switch")?;
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
    apply_trigger_frame_override(&mut payload, trigger.target_tab.frame_id.as_deref());
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
                let dispatch_command = if command == "fill" {
                    "_trigger_fill"
                } else {
                    command
                };
                IpcRequest::new(
                    dispatch_command,
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
    let data = ensure_trigger_response_success(response, trigger, "action")?;
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
        trigger.source_tab.frame_id.as_deref(),
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

    let mut args = serde_json::json!({
        "spec": parameterized.resolved_spec,
        "spec_source": spec_source,
        "_trigger": trigger_request_meta(trigger, evidence, "action"),
    });
    apply_trigger_frame_override(&mut args, trigger.target_tab.frame_id.as_deref());
    let response = router
        .dispatch_within_active_transaction(
            IpcRequest::new(
                "_trigger_pipe",
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
    ensure_trigger_response_success(response, trigger, "action")
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
        .map_err(|error| trigger_target_refresh_failure_error(target_id, error.into_envelope()))?;
    let target_tab = resolve_bound_tab(&tabs, target_id)
        .map_err(|_| trigger_target_tab_missing_error(target_id))?;
    if !target_tab.active {
        return Err(trigger_degraded_authority_error(
            "Trigger target continuity fence failed: target tab is not active after switch",
            "continuity_target_not_active",
            serde_json::json!({
                "target_tab_target_id": target_id,
                "target_tab_index": target_tab.index,
            }),
        ));
    }

    refresh_live_runtime_state(&browser, state).await;
    refresh_live_frame_runtime(&browser, state).await;
    let frame_runtime = state.frame_runtime().await;
    let readiness = state.readiness_state().await;
    if let Some((reason, message)) =
        trigger_target_continuity_failure(target_id, &frame_runtime, &readiness)
    {
        return Err(trigger_degraded_authority_error(
            message,
            reason,
            serde_json::json!({
                "target_tab_target_id": target_id,
                "frame_runtime": frame_runtime,
                "readiness_state": readiness,
            }),
        ));
    }

    Ok(())
}

fn trigger_target_tab_missing_error(target_id: &str) -> ErrorEnvelope {
    trigger_degraded_authority_error(
        "Trigger target continuity fence failed: bound target tab is no longer present in the current session",
        "continuity_target_tab_missing",
        serde_json::json!({
            "target_tab_target_id": target_id,
        }),
    )
}

fn trigger_target_refresh_failure_error(target_id: &str, upstream: ErrorEnvelope) -> ErrorEnvelope {
    let upstream_code = upstream.code;
    let upstream_message = upstream.message.clone();
    let upstream_context = upstream.context.clone();
    let upstream_reason = upstream_context
        .as_ref()
        .and_then(|value| value.get("reason"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    trigger_degraded_authority_error(
        "Trigger target continuity fence failed: live target refresh could not re-establish authoritative tab state",
        "continuity_tab_refresh_failed",
        serde_json::json!({
            "target_tab_target_id": target_id,
            "upstream_error_code": upstream_code,
            "upstream_error_message": upstream_message,
            "upstream_error_reason": upstream_reason,
            "upstream_error_context": upstream_context,
        }),
    )
}

fn ensure_trigger_response_success(
    response: rub_ipc::protocol::IpcResponse,
    trigger: &TriggerInfo,
    phase: &'static str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    match response.status {
        ResponseStatus::Success => Ok(response.data),
        ResponseStatus::Error => Err(augment_trigger_error_context(
            response.error.unwrap_or_else(missing_error_envelope),
            trigger,
            phase,
        )),
    }
}

fn augment_trigger_error_context(
    mut error: ErrorEnvelope,
    trigger: &TriggerInfo,
    phase: &'static str,
) -> ErrorEnvelope {
    let mut context = error
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let trigger_context = serde_json::json!({
        "trigger_id": trigger.id,
        "phase": phase,
        "source_tab_target_id": trigger.source_tab.target_id,
        "source_frame_id": trigger.source_tab.frame_id,
        "target_tab_target_id": trigger.target_tab.target_id,
        "target_frame_id": trigger.target_tab.frame_id,
        "action_kind": trigger.action.kind,
        "action_command": trigger.action.command,
    });
    if let Some(trigger_object) = trigger_context.as_object() {
        for (key, value) in trigger_object {
            context.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    error.context = Some(serde_json::Value::Object(context));
    error
}

fn missing_error_envelope() -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcProtocolError,
        "trigger action returned an error response without an error envelope",
    )
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

#[cfg(test)]
mod continuity_tests {
    use super::{
        trigger_target_continuity_failure, trigger_target_refresh_failure_error,
        trigger_target_tab_missing_error,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, OverlayState, ReadinessInfo,
        ReadinessStatus, RouteStability,
    };

    #[test]
    fn target_tab_missing_uses_degraded_authority_family() {
        let envelope = trigger_target_tab_missing_error("tab-target");
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_target_tab_missing")
        );
    }

    #[test]
    fn target_refresh_failure_uses_degraded_authority_family() {
        let envelope = trigger_target_refresh_failure_error(
            "tab-target",
            ErrorEnvelope::new(ErrorCode::BrowserCrashed, "browser died").with_context(
                serde_json::json!({
                    "reason": "browser_disconnected",
                }),
            ),
        );
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_tab_refresh_failed")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("upstream_error_code"))
                .and_then(|value| value.as_str()),
            Some("BROWSER_CRASHED")
        );
    }

    #[test]
    fn target_continuity_fails_when_frame_runtime_is_stale() {
        let frame_runtime = FrameRuntimeInfo {
            status: FrameContextStatus::Stale,
            current_frame: Some(FrameContextInfo {
                frame_id: "missing-frame".to_string(),
                name: None,
                parent_frame_id: None,
                target_id: None,
                url: None,
                depth: 0,
                same_origin_accessible: None,
            }),
            primary_frame: None,
            frame_lineage: vec!["missing-frame".to_string()],
            degraded_reason: Some("selected_frame_not_found".to_string()),
        };
        let readiness = ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("complete".to_string()),
            blocking_signals: Vec::new(),
            degraded_reason: None,
        };

        assert_eq!(
            trigger_target_continuity_failure("tab-target", &frame_runtime, &readiness),
            Some((
                "continuity_frame_unavailable",
                "Trigger target continuity fence failed: frame context became unavailable",
            ))
        );
    }
}

fn trigger_action_timeout_ms(command: &str, args: &serde_json::Value) -> u64 {
    TRIGGER_ACTION_BASE_TIMEOUT_MS.saturating_add(
        rub_core::automation_timeout::command_additional_timeout_ms(command, args),
    )
}

fn apply_trigger_frame_override(args: &mut serde_json::Value, frame_id: Option<&str>) {
    let Some(frame_id) = frame_id else {
        return;
    };
    let Some(object) = args.as_object_mut() else {
        return;
    };
    let orchestration = object
        .entry("_orchestration".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !orchestration.is_object() {
        *orchestration = serde_json::json!({});
    }
    let orchestration_object = orchestration
        .as_object_mut()
        .expect("trigger frame override must normalize orchestration metadata to an object");
    orchestration_object.insert("frame_id".to_string(), serde_json::json!(frame_id));
}

pub(super) fn trigger_action_command_id(
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
) -> String {
    let identity_key = super::trigger_evidence_consumption_key(evidence);
    format!(
        "trigger:{}:{}",
        trigger.id,
        hex_encode_trigger_command_identity(identity_key.as_bytes())
    )
}

fn hex_encode_trigger_command_identity(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String must succeed");
    }
    encoded
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
        "source_frame_id": trigger.source_tab.frame_id,
        "target_tab_target_id": trigger.target_tab.target_id,
        "target_frame_id": trigger.target_tab.frame_id,
        "condition_kind": trigger.condition.kind,
        "evidence": evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_trigger_frame_override, ensure_trigger_response_success, trigger_action_command_id,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::{
        TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerInfo, TriggerMode, TriggerStatus, TriggerTabBindingInfo,
    };
    use rub_ipc::protocol::IpcResponse;
    use serde_json::json;

    fn trigger() -> TriggerInfo {
        TriggerInfo {
            id: 7,
            status: TriggerStatus::Armed,
            lifecycle_generation: 1,
            mode: TriggerMode::Once,
            source_tab: TriggerTabBindingInfo {
                index: 0,
                target_id: "source-tab".to_string(),
                frame_id: Some("source-frame".to_string()),
                url: "https://source.example".to_string(),
                title: "Source".to_string(),
            },
            target_tab: TriggerTabBindingInfo {
                index: 1,
                target_id: "target-tab".to_string(),
                frame_id: Some("target-frame".to_string()),
                url: "https://target.example".to_string(),
                title: "Target".to_string(),
            },
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::Readiness,
                locator: None,
                text: None,
                url_pattern: None,
                readiness_state: Some("ready".to_string()),
                method: None,
                status_code: None,
                storage_area: None,
                key: None,
                value: None,
            },
            action: TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(json!({ "selector": "#send" })),
            },
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        }
    }

    #[test]
    fn trigger_frame_override_overwrites_conflicting_frame_id() {
        let mut args = json!({
            "selector": "#send",
            "_orchestration": {
                "frame_id": "child-frame",
                "command_id": "step-command"
            }
        });

        apply_trigger_frame_override(&mut args, Some("target-frame"));

        assert_eq!(args["_orchestration"]["frame_id"], "target-frame");
        assert_eq!(args["_orchestration"]["command_id"], "step-command");
    }

    #[test]
    fn trigger_frame_override_normalizes_non_object_metadata() {
        let mut args = json!({
            "selector": "#send",
            "_orchestration": "bad-shape"
        });

        apply_trigger_frame_override(&mut args, Some("target-frame"));

        assert_eq!(args["_orchestration"]["frame_id"], "target-frame");
    }

    #[test]
    fn committed_trigger_error_preserves_remote_reason_and_adds_trigger_phase_context() {
        let trigger = trigger();
        let response = IpcResponse::error(
            "req-1",
            ErrorEnvelope::new(ErrorCode::InvalidInput, "selector is stale").with_context(json!({
                "reason": "remote_invalid_selector",
                "selector": "#send"
            })),
        );

        let error =
            ensure_trigger_response_success(response, &trigger, "action").expect_err("error");

        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(
            error.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&json!("remote_invalid_selector"))
        );
        assert_eq!(
            error.context.as_ref().and_then(|ctx| ctx.get("phase")),
            Some(&json!("action"))
        );
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("target_tab_target_id")),
            Some(&json!("target-tab"))
        );
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("target_frame_id")),
            Some(&json!("target-frame"))
        );
    }

    #[test]
    fn trigger_action_command_id_uses_explicit_stable_hex_identity() {
        let trigger = trigger();
        let evidence = rub_core::model::TriggerEvidenceInfo {
            summary: "source_tab_text_present:Ready".to_string(),
            fingerprint: Some("Ready::stable".to_string()),
        };

        let command_id = trigger_action_command_id(&trigger, &evidence);

        assert_eq!(command_id, "trigger:7:52656164793a3a737461626c65");
    }
}
