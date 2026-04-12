use super::args::{FillArgs, FillStepSpec, submit_args};
use super::command_build::{
    atomic_fill_write_mode_supported, build_fill_step_locator_args, classify_fill_value_target,
    project_fill_rejection_issue, project_fill_target_summary,
};
use super::spec::parse_fill_steps;
use super::*;
use crate::router::addressing::load_snapshot;
use crate::router::addressing::resolve_elements_against_snapshot;
use crate::router::request_args::parse_json_args;
use crate::router::request_args::{
    LocatorParseOptions, canonical_locator_json, parse_canonical_locator,
};
use rub_core::error::ErrorEnvelope;

pub(super) async fn cmd_fill_validate(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed_args: FillArgs = parse_json_args(args, "fill")?;
    let parsed = parse_fill_steps(&parsed_args.spec, &state.rub_home)?;
    let steps = parsed.value;

    let prefer_a11y = plan_requires_a11y(&steps, &parsed_args.submit);
    let snapshot = load_snapshot(router, args, state, deadline, prefer_a11y).await?;

    let mut projected_steps = Vec::with_capacity(steps.len());
    let mut overall_valid = true;
    let mut snapshot_preflight_compatible = true;
    let mut atomic_candidate = true;
    let mut live_confirmation_required = parsed_args._wait_after.is_some();

    for (index, step) in steps.iter().enumerate() {
        let projected = project_fill_step_validation(router, &snapshot, step, index).await;
        overall_valid &= projected.get("status").and_then(serde_json::Value::as_str) == Some("ok");
        snapshot_preflight_compatible &= projected
            .get("snapshot_preflight_compatible")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        atomic_candidate &= projected
            .get("atomic_candidate")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        live_confirmation_required |= projected
            .get("live_confirmation_needed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        projected_steps.push(projected);
    }

    let submit_projection =
        project_submit_validation(router, &snapshot, &parsed_args.submit).await?;
    overall_valid &= submit_projection
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|status| status == "ok");
    snapshot_preflight_compatible &= submit_projection
        .get("snapshot_preflight_compatible")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    atomic_candidate &= submit_projection
        .get("atomic_candidate")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    live_confirmation_required |= submit_projection
        .get("live_confirmation_needed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let result = serde_json::json!({
        "subject": {
            "kind": "fill_validation",
            "source": "live_page",
            "snapshot_id": snapshot.snapshot_id,
            "snapshot_scope": "resolution_only",
        },
        "result": {
            "valid": overall_valid,
            "step_count": projected_steps.len(),
            "atomic_requested": parsed_args.atomic,
            "submit_requested": submit_projection["requested"],
            "global_wait_after_requested": parsed_args._wait_after.is_some(),
            "snapshot_preflight_compatible": snapshot_preflight_compatible,
            "atomic_candidate": atomic_candidate && parsed_args._wait_after.is_none(),
            "live_confirmation_required": live_confirmation_required,
            "steps": projected_steps,
            "submit": submit_projection,
            "global_wait_after": serde_json::json!({
                "requested": parsed_args._wait_after.is_some(),
                "probe": parsed_args._wait_after.clone().unwrap_or(serde_json::Value::Null),
                "effect_on_atomic_candidate": parsed_args._wait_after.is_some(),
                "effect_on_live_confirmation": parsed_args._wait_after.is_some(),
            }),
            "outcome_summary": {
                "class": if overall_valid { "fill_plan_validated" } else { "fill_plan_invalid" },
                "authoritative": true,
                "summary": if overall_valid {
                    "The fill plan was resolved against one authoritative snapshot and is ready for live execution."
                } else {
                    "The fill plan was analyzed, but one or more steps are invalid or incompatible with the current runtime surface."
                },
            },
        }
    });

    Ok(result)
}

fn plan_requires_a11y(steps: &[FillStepSpec], submit: &super::args::SubmitLocatorArgs) -> bool {
    steps.iter().any(step_requires_a11y) || submit_requires_a11y(submit)
}

fn step_requires_a11y(step: &FillStepSpec) -> bool {
    canonical_step_locator(step)
        .map(|locator| locator.requires_a11y_snapshot())
        .unwrap_or(false)
}

fn submit_requires_a11y(submit: &super::args::SubmitLocatorArgs) -> bool {
    submit_args(submit)
        .and_then(|value| {
            parse_canonical_locator(&value, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS).ok()
        })
        .flatten()
        .map(|locator| locator.requires_a11y_snapshot())
        .unwrap_or(false)
}

async fn project_fill_step_validation(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    step: &FillStepSpec,
    step_index: usize,
) -> serde_json::Value {
    let locator_args = build_fill_step_locator_args(step);
    let canonical_locator =
        parse_canonical_locator(&locator_args, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS)
            .ok()
            .flatten();
    let intent = classify_step_intent(step);
    let mut notes = Vec::new();
    if step.value.is_some() && step.activate.unwrap_or(false) {
        notes.push("activate_is_ignored_when_value_is_present".to_string());
    }

    match resolve_elements_against_snapshot(router, snapshot, &locator_args, "fill --validate")
        .await
    {
        Ok(resolved) => {
            let element = resolved
                .elements
                .first()
                .cloned()
                .expect("resolved element list must not be empty");
            let classification = classify_resolved_step(step, &element);
            let atomic_candidate = classification.atomic_candidate && step.wait_after.is_none();
            serde_json::json!({
                "step_index": step_index,
                "status": if classification.supported { "ok" } else { "invalid" },
                "locator": canonical_locator.as_ref().map(canonical_locator_json).unwrap_or(serde_json::Value::Null),
                "target": project_fill_target_summary(&element),
                "intent_class": intent,
                "write_mode": classification.write_mode,
                "rollback_class": classification.rollback_class,
                "snapshot_preflight_compatible": true,
                "atomic_candidate": atomic_candidate,
                "live_confirmation_needed": classification.live_confirmation_needed || step.wait_after.is_some(),
                "wait_after": serde_json::json!({
                    "requested": step.wait_after.is_some(),
                    "probe": step.wait_after.as_ref().map(project_wait_after).unwrap_or(serde_json::Value::Null),
                }),
                "notes": notes,
                "issues": classification.issues,
            })
        }
        Err(error) => {
            let envelope = error.into_envelope();
            serde_json::json!({
                "step_index": step_index,
                "status": "invalid",
                "locator": canonical_locator.as_ref().map(canonical_locator_json).unwrap_or(serde_json::Value::Null),
                "target": serde_json::Value::Null,
                "intent_class": intent,
                "write_mode": "unresolved_target",
                "rollback_class": "not_supported",
                "snapshot_preflight_compatible": false,
                "atomic_candidate": false,
                "live_confirmation_needed": false,
                "wait_after": serde_json::json!({
                    "requested": step.wait_after.is_some(),
                    "probe": step.wait_after.as_ref().map(project_wait_after).unwrap_or(serde_json::Value::Null),
                }),
                "notes": notes,
                "issues": vec![project_issue(&envelope)],
            })
        }
    }
}

async fn project_submit_validation(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    submit: &super::args::SubmitLocatorArgs,
) -> Result<serde_json::Value, RubError> {
    let Some(locator_args) = submit_args(submit) else {
        return Ok(serde_json::json!({
            "requested": false,
            "status": serde_json::Value::Null,
            "locator": serde_json::Value::Null,
            "target": serde_json::Value::Null,
            "write_mode": serde_json::Value::Null,
            "rollback_class": serde_json::Value::Null,
            "snapshot_preflight_compatible": true,
            "atomic_candidate": true,
            "live_confirmation_needed": false,
            "issues": [],
        }));
    };

    let canonical_locator =
        parse_canonical_locator(&locator_args, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS)?
            .map(|locator| canonical_locator_json(&locator))
            .unwrap_or(serde_json::Value::Null);

    match resolve_elements_against_snapshot(
        router,
        snapshot,
        &locator_args,
        "fill --validate submit",
    )
    .await
    {
        Ok(resolved) => {
            let element = resolved
                .elements
                .first()
                .cloned()
                .expect("resolved submit element must not be empty");
            Ok(serde_json::json!({
                "requested": true,
                "status": "ok",
                "locator": canonical_locator,
                "target": project_fill_target_summary(&element),
                "write_mode": "click_submit",
                "rollback_class": "non_rollbackable",
                "snapshot_preflight_compatible": true,
                "atomic_candidate": false,
                "live_confirmation_needed": true,
                "issues": [],
            }))
        }
        Err(error) => {
            let envelope = error.into_envelope();
            Ok(serde_json::json!({
                "requested": true,
                "status": "invalid",
                "locator": canonical_locator,
                "target": serde_json::Value::Null,
                "write_mode": "click_submit",
                "rollback_class": "non_rollbackable",
                "snapshot_preflight_compatible": false,
                "atomic_candidate": false,
                "live_confirmation_needed": true,
                "issues": vec![project_issue(&envelope)],
            }))
        }
    }
}

fn canonical_step_locator(step: &FillStepSpec) -> Option<rub_core::locator::CanonicalLocator> {
    let locator_args = build_fill_step_locator_args(step);
    parse_canonical_locator(&locator_args, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS)
        .ok()
        .flatten()
}

fn classify_step_intent(step: &FillStepSpec) -> &'static str {
    if step.value.is_some() {
        "value_write"
    } else if step.activate.unwrap_or(false) {
        "activate"
    } else {
        "missing_intent"
    }
}

struct StepClassification {
    supported: bool,
    write_mode: &'static str,
    rollback_class: &'static str,
    atomic_candidate: bool,
    live_confirmation_needed: bool,
    issues: Vec<serde_json::Value>,
}

fn classify_resolved_step(
    step: &FillStepSpec,
    element: &rub_core::model::Element,
) -> StepClassification {
    if step.value.is_some() {
        let classification = classify_fill_value_target(element);
        return StepClassification {
            supported: classification.supported,
            write_mode: classification.write_mode,
            rollback_class: classification.rollback_class,
            atomic_candidate: classification.supported
                && atomic_fill_write_mode_supported(classification.write_mode),
            live_confirmation_needed: false,
            issues: if classification.supported {
                Vec::new()
            } else {
                vec![project_fill_rejection_issue(element, &classification)]
            },
        };
    }

    if step.activate.unwrap_or(false) {
        return StepClassification {
            supported: true,
            write_mode: "click_activate",
            rollback_class: "non_rollbackable",
            atomic_candidate: false,
            live_confirmation_needed: true,
            issues: Vec::new(),
        };
    }

    StepClassification {
        supported: false,
        write_mode: "missing_write_intent",
        rollback_class: "not_supported",
        atomic_candidate: false,
        live_confirmation_needed: false,
        issues: vec![serde_json::json!({
            "code": ErrorCode::InvalidInput,
            "message": "fill step requires either 'value' or 'activate: true'",
            "suggestion": "Add a 'value' for text/select style writes, or set 'activate: true' for click-like steps.",
        })],
    }
}

fn project_wait_after(wait_after: &super::args::StepWaitSpec) -> serde_json::Value {
    serde_json::json!({
        "selector": wait_after.selector,
        "target_text": wait_after.target_text,
        "role": wait_after.role,
        "label": wait_after.label,
        "testid": wait_after.testid,
        "text": wait_after.text,
        "first": wait_after.first,
        "last": wait_after.last,
        "nth": wait_after.nth,
        "timeout_ms": wait_after.timeout_ms,
        "state": wait_after.state,
    })
}

fn project_issue(envelope: &ErrorEnvelope) -> serde_json::Value {
    serde_json::json!({
        "code": envelope.code,
        "message": envelope.message,
        "suggestion": envelope.suggestion,
        "context": envelope.context,
    })
}

#[cfg(test)]
mod tests {
    use super::{classify_resolved_step, classify_step_intent};
    use crate::router::workflow::args::FillStepSpec;
    use crate::router::workflow::command_build::project_fill_target_summary;
    use rub_core::model::{Element, ElementTag};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn classify_value_step_on_text_input_as_revertible_type() {
        let step: FillStepSpec = serde_json::from_value(json!({
            "selector": "#email",
            "value": "user@example.com"
        }))
        .expect("step should parse");
        let element = sample_element(ElementTag::Input);

        let classification = classify_resolved_step(&step, &element);
        assert!(classification.supported);
        assert_eq!(classification.write_mode, "type_text");
        assert_eq!(classification.rollback_class, "candidate_value_revert");
        assert!(classification.atomic_candidate);
        assert!(!classification.live_confirmation_needed);
        assert_eq!(classify_step_intent(&step), "value_write");
    }

    #[test]
    fn classify_activate_step_as_non_rollbackable_confirmation_required() {
        let step: FillStepSpec = serde_json::from_value(json!({
            "selector": "#submit",
            "activate": true
        }))
        .expect("step should parse");
        let element = sample_element(ElementTag::Button);

        let classification = classify_resolved_step(&step, &element);
        assert!(classification.supported);
        assert_eq!(classification.write_mode, "click_activate");
        assert_eq!(classification.rollback_class, "non_rollbackable");
        assert!(!classification.atomic_candidate);
        assert!(classification.live_confirmation_needed);
        assert_eq!(classify_step_intent(&step), "activate");
    }

    #[test]
    fn classify_value_step_rejects_button_target() {
        let step: FillStepSpec = serde_json::from_value(json!({
            "selector": "#submit",
            "value": "wrong"
        }))
        .expect("step should parse");
        let element = sample_element(ElementTag::Button);

        let classification = classify_resolved_step(&step, &element);
        assert!(!classification.supported);
        assert_eq!(classification.write_mode, "unsupported_value_target");
        assert_eq!(classification.rollback_class, "not_supported");
        assert!(!classification.atomic_candidate);
        assert!(!classification.issues.is_empty());
        assert_eq!(
            classification.issues[0]["reason"],
            "activation_only_control"
        );
        assert_eq!(
            classification.issues[0]["recommended_safe_fallback"],
            "Use `activate: true` or a click step instead of a value write for activation-only targets."
        );
    }

    #[test]
    fn classify_value_step_accepts_contenteditable_editor_target() {
        let step: FillStepSpec = serde_json::from_value(json!({
            "selector": ".composer",
            "value": "Automated Benchmark Execution with Rub CLI..."
        }))
        .expect("step should parse");
        let mut element = sample_element(ElementTag::Other);
        element
            .attributes
            .insert("contenteditable".to_string(), "true".to_string());

        let classification = classify_resolved_step(&step, &element);
        assert!(classification.supported);
        assert_eq!(classification.write_mode, "type_editor_text");
        assert_eq!(classification.rollback_class, "candidate_value_revert");
        assert!(
            !classification.atomic_candidate,
            "atomic v1 should stay on generic input/textarea/select rollback surfaces"
        );
    }

    #[test]
    fn target_summary_projects_role_label_and_testid() {
        let mut element = sample_element(ElementTag::Button);
        element.text = "Consent".to_string();
        element
            .attributes
            .insert("data-testid".to_string(), "consent-cta".to_string());
        let target = project_fill_target_summary(&element);
        assert_eq!(target["tag"], "button");
        assert_eq!(target["label"], "Consent");
        assert_eq!(target["testid"], "consent-cta");
    }

    #[test]
    fn target_summary_projects_editor_safe_metadata() {
        let mut element = sample_element(ElementTag::Other);
        element
            .attributes
            .insert("contenteditable".to_string(), "plaintext-only".to_string());
        element
            .attributes
            .insert("role".to_string(), "textbox".to_string());
        let target = project_fill_target_summary(&element);
        assert_eq!(target["role"], "textbox");
        assert_eq!(target["is_content_editable"], true);
        assert_eq!(target["editor_safe_kind"], "semantic_textbox");
    }

    fn sample_element(tag: ElementTag) -> Element {
        Element {
            index: 0,
            tag,
            text: String::new(),
            attributes: HashMap::new(),
            element_ref: Some("main:1".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: Some(0),
        }
    }
}
