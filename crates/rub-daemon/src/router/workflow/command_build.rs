use super::args::{FillStepSpec, StepWaitSpec};
use super::*;
use crate::router::addressing::resolve_element;
use crate::router::element_semantics::{
    accessible_label, editor_safe_text_target_kind, is_content_editable, semantic_role, test_id,
};
use crate::router::request_args::{LocatorRequestArgs, locator_json};
use rub_core::model::{Element, ElementTag};

pub(super) struct FillValueTargetClassification {
    pub(super) supported: bool,
    pub(super) write_mode: &'static str,
    pub(super) rollback_class: &'static str,
    pub(super) rejection_reason: Option<&'static str>,
    pub(super) recommended_safe_fallback: Option<&'static str>,
}

pub(super) fn build_fill_step_locator_args(step: &FillStepSpec) -> serde_json::Value {
    locator_json(LocatorRequestArgs {
        index: step.index,
        element_ref: step.element_ref.clone(),
        selector: step.selector.clone(),
        target_text: step.target_text.clone(),
        role: step.role.clone(),
        label: step.label.clone(),
        testid: step.testid.clone(),
        visible: false,
        prefer_enabled: false,
        topmost: false,
        first: step.first,
        last: step.last,
        nth: step.nth,
    })
}

pub(super) fn attach_snapshot_id(target: &mut serde_json::Value, snapshot_id: Option<&str>) {
    let Some(snapshot_id) = snapshot_id else {
        return;
    };
    if let Some(object) = target.as_object_mut() {
        object.insert("snapshot_id".to_string(), serde_json::json!(snapshot_id));
    }
}

pub(super) async fn build_fill_step_command(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    step: &FillStepSpec,
    locator_args: &serde_json::Value,
) -> Result<(&'static str, serde_json::Value), RubError> {
    let resolved = resolve_element(router, locator_args, state, deadline, "fill").await?;

    if let Some(value) = &step.value {
        let classification = classify_fill_value_target(&resolved.element);
        return match classification.write_mode {
            "select_choice" => Ok((
                "select",
                serde_json::json!({
                    "index": resolved.element.index,
                    "snapshot_id": resolved.snapshot_id,
                    "value": value,
                }),
            )),
            "type_text" | "type_editor_text" => Ok((
                "type",
                serde_json::json!({
                    "index": resolved.element.index,
                    "snapshot_id": resolved.snapshot_id,
                    "text": value,
                    "clear": step.clear.unwrap_or(true),
                }),
            )),
            _ => Err(build_fill_value_target_error(
                &resolved.element,
                &classification,
            )),
        };
    }

    if step.activate.unwrap_or(false) {
        return Ok((
            "click",
            serde_json::json!({
                "index": resolved.element.index,
                "snapshot_id": resolved.snapshot_id,
            }),
        ));
    }

    Err(RubError::domain(
        ErrorCode::InvalidInput,
        "fill step requires either 'value' or 'activate: true'",
    ))
}

pub(super) fn build_fill_step_command_for_resolved_target(
    step: &FillStepSpec,
    element: &Element,
) -> Result<(&'static str, serde_json::Value), RubError> {
    let target = stable_live_target_locator(element)?;

    if let Some(value) = &step.value {
        let classification = classify_fill_value_target(element);
        return match classification.write_mode {
            "select_choice" => Ok((
                "select",
                serde_json::json!({
                    "element_ref": target,
                    "value": value,
                }),
            )),
            "type_text" | "type_editor_text" => Ok((
                "type",
                serde_json::json!({
                    "element_ref": target,
                    "text": value,
                    "clear": step.clear.unwrap_or(true),
                }),
            )),
            _ => Err(build_fill_value_target_error(element, &classification)),
        };
    }

    if step.activate.unwrap_or(false) {
        return Ok((
            "click",
            serde_json::json!({
                "element_ref": target,
            }),
        ));
    }

    Err(RubError::domain(
        ErrorCode::InvalidInput,
        "fill step requires either 'value' or 'activate: true'",
    ))
}

pub(super) fn build_submit_command_for_resolved_target(
    element: &Element,
) -> Result<serde_json::Value, RubError> {
    let target = stable_live_target_locator(element)?;
    Ok(serde_json::json!({
        "element_ref": target,
    }))
}

pub(super) fn build_atomic_rollback_command_for_resolved_target(
    element: &Element,
    write_mode: &str,
    original_value: &str,
) -> Result<(&'static str, serde_json::Value), RubError> {
    let target = stable_live_target_locator(element)?;
    match write_mode {
        "type_text" => Ok((
            "type",
            serde_json::json!({
                "element_ref": target,
                "text": original_value,
                "clear": true,
            }),
        )),
        "select_choice" => Ok((
            "select",
            serde_json::json!({
                "element_ref": target,
                "value": original_value,
            }),
        )),
        _ => Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "fill --atomic v1 only supports rollbackable value/select writes",
            serde_json::json!({
                "write_mode": write_mode,
                "target": project_fill_target_summary(element),
            }),
        )),
    }
}

pub(super) fn atomic_fill_write_mode_supported(write_mode: &str) -> bool {
    matches!(write_mode, "type_text" | "select_choice")
}

pub(super) fn stable_live_target_locator(element: &Element) -> Result<String, RubError> {
    element.element_ref.clone().ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "fill --snapshot requires stable target identity for every resolved element",
            serde_json::json!({
                "index": element.index,
                "reason": "missing_element_ref",
            }),
        )
    })
}

pub(super) fn classify_fill_value_target(element: &Element) -> FillValueTargetClassification {
    match element.tag {
        ElementTag::Input | ElementTag::TextArea => FillValueTargetClassification {
            supported: true,
            write_mode: "type_text",
            rollback_class: "candidate_value_revert",
            rejection_reason: None,
            recommended_safe_fallback: None,
        },
        ElementTag::Select => FillValueTargetClassification {
            supported: true,
            write_mode: "select_choice",
            rollback_class: "candidate_value_revert",
            rejection_reason: None,
            recommended_safe_fallback: None,
        },
        ElementTag::Other => {
            if editor_safe_text_target_kind(element).is_some() {
                FillValueTargetClassification {
                    supported: true,
                    write_mode: "type_editor_text",
                    rollback_class: "candidate_value_revert",
                    rejection_reason: None,
                    recommended_safe_fallback: None,
                }
            } else if semantic_role(element) == "textbox" {
                FillValueTargetClassification {
                    supported: false,
                    write_mode: "unsupported_value_target",
                    rollback_class: "not_supported",
                    rejection_reason: Some("semantic_textbox_without_verified_editable_dom"),
                    recommended_safe_fallback: Some(
                        "Target the verified input/textarea/contenteditable node directly, or fall back to `rub exec` only if you need a site-specific editor path.",
                    ),
                }
            } else {
                FillValueTargetClassification {
                    supported: false,
                    write_mode: "unsupported_value_target",
                    rollback_class: "not_supported",
                    rejection_reason: Some("unsupported_safe_path_write_surface"),
                    recommended_safe_fallback: Some(
                        "Use `activate: true` for click-like controls, or wait for editor-safe write support for this surface.",
                    ),
                }
            }
        }
        ElementTag::Button
        | ElementTag::Link
        | ElementTag::Checkbox
        | ElementTag::Radio
        | ElementTag::Option => FillValueTargetClassification {
            supported: false,
            write_mode: "unsupported_value_target",
            rollback_class: "not_supported",
            rejection_reason: Some("activation_only_control"),
            recommended_safe_fallback: Some(
                "Use `activate: true` or a click step instead of a value write for activation-only targets.",
            ),
        },
    }
}

pub(super) fn project_fill_target_summary(element: &Element) -> serde_json::Value {
    serde_json::json!({
        "index": element.index,
        "tag": element.tag,
        "element_ref": element.element_ref,
        "role": semantic_role(element),
        "label": accessible_label(element),
        "testid": test_id(element),
        "is_content_editable": is_content_editable(element),
        "editor_safe_kind": editor_safe_text_target_kind(element),
        "readonly": element.attributes.contains_key("readonly"),
        "aria_readonly": element
            .attributes
            .get("aria-readonly")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        "text": element.text,
    })
}

pub(super) fn project_fill_rejection_issue(
    element: &Element,
    classification: &FillValueTargetClassification,
) -> serde_json::Value {
    serde_json::json!({
        "code": ErrorCode::InvalidInput,
        "message": "fill rejected the resolved target on the safe path",
        "reason": classification.rejection_reason,
        "write_mode": classification.write_mode,
        "target": project_fill_target_summary(element),
        "recommended_safe_fallback": classification.recommended_safe_fallback,
        "suggestion": classification.recommended_safe_fallback,
    })
}

fn build_fill_value_target_error(
    element: &Element,
    classification: &FillValueTargetClassification,
) -> RubError {
    RubError::domain_with_context_and_suggestion(
        ErrorCode::InvalidInput,
        "fill rejected the resolved target on the safe path",
        serde_json::json!({
            "target": project_fill_target_summary(element),
            "write_mode_classification": classification.write_mode,
            "rollback_class": classification.rollback_class,
            "rejection_reason": classification.rejection_reason,
            "recommended_safe_fallback": classification.recommended_safe_fallback,
        }),
        classification
            .recommended_safe_fallback
            .unwrap_or("Use `rub explain interactability` and `rub explain locator` to understand why this target is not safe for fill."),
    )
}

pub(super) fn attach_step_wait_after(target: &mut serde_json::Value, wait_after: &StepWaitSpec) {
    let mut wait = serde_json::Map::new();
    if let Some(selector) = &wait_after.selector {
        wait.insert("selector".to_string(), serde_json::json!(selector));
    }
    if let Some(target_text) = &wait_after.target_text {
        wait.insert("target_text".to_string(), serde_json::json!(target_text));
    }
    if let Some(role) = &wait_after.role {
        wait.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(label) = &wait_after.label {
        wait.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(testid) = &wait_after.testid {
        wait.insert("testid".to_string(), serde_json::json!(testid));
    }
    if let Some(text) = &wait_after.text {
        wait.insert("text".to_string(), serde_json::json!(text));
    }
    if wait_after.first {
        wait.insert("first".to_string(), serde_json::json!(true));
    }
    if wait_after.last {
        wait.insert("last".to_string(), serde_json::json!(true));
    }
    if let Some(nth) = wait_after.nth {
        wait.insert("nth".to_string(), serde_json::json!(nth));
    }
    if let Some(timeout_ms) = wait_after.timeout_ms {
        wait.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
    }
    if let Some(state) = &wait_after.state {
        wait.insert("state".to_string(), serde_json::json!(state));
    }
    if let Some(object) = target.as_object_mut()
        && !wait.is_empty()
    {
        object.insert("wait_after".to_string(), serde_json::Value::Object(wait));
    }
}
