use super::args::{FillStepSpec, StepWaitSpec};
use super::*;
use crate::router::addressing::resolve_element;
use crate::router::request_args::{LocatorRequestArgs, locator_json};

pub(super) async fn build_fill_step_command(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    step: &FillStepSpec,
) -> Result<(&'static str, serde_json::Value), RubError> {
    let locator_args = locator_json(LocatorRequestArgs {
        index: step.index,
        element_ref: step.element_ref.clone(),
        selector: step.selector.clone(),
        target_text: step.target_text.clone(),
        role: step.role.clone(),
        label: step.label.clone(),
        testid: step.testid.clone(),
        first: step.first,
        last: step.last,
        nth: step.nth,
    });
    let resolved = resolve_element(router, &locator_args, state, deadline, "fill").await?;

    if let Some(value) = &step.value {
        return match resolved.element.tag {
            rub_core::model::ElementTag::Select => Ok((
                "select",
                serde_json::json!({
                    "index": resolved.element.index,
                    "snapshot_id": resolved.snapshot_id,
                    "value": value,
                }),
            )),
            rub_core::model::ElementTag::Input | rub_core::model::ElementTag::TextArea => Ok((
                "type",
                serde_json::json!({
                    "index": resolved.element.index,
                    "snapshot_id": resolved.snapshot_id,
                    "text": value,
                    "clear": step.clear.unwrap_or(true),
                }),
            )),
            tag => Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("fill value is unsupported for target tag '{tag:?}'"),
                serde_json::json!({
                    "index": resolved.element.index,
                    "tag": tag,
                }),
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
