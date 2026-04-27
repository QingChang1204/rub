use crate::trigger_workflow_bridge::validate_trigger_workflow_bindings;
use crate::workflow_assets::normalize_workflow_name;
use crate::workflow_policy::{
    trigger_workflow_allowed_step_descriptions, trigger_workflow_request_allowed,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::CanonicalLocator;
use rub_core::model::{
    TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerRegistrationSpec,
};

pub(super) fn validate_trigger_registration_spec(
    spec: &mut TriggerRegistrationSpec,
) -> Result<(), RubError> {
    normalize_optional_key(&mut spec.source_frame_id, "source_frame_id")?;
    normalize_optional_key(&mut spec.target_frame_id, "target_frame_id")?;
    validate_trigger_condition(&spec.condition)?;
    validate_trigger_action(&mut spec.action)
}

pub(crate) fn validate_trigger_condition(
    condition: &rub_core::model::TriggerConditionSpec,
) -> Result<(), RubError> {
    match condition.kind {
        TriggerConditionKind::TextPresent => {
            reject_irrelevant_trigger_condition_fields(condition, &["text"], "text_present")?;
            require_non_empty(condition.text.as_deref(), "condition.text")?;
        }
        TriggerConditionKind::LocatorPresent => {
            reject_irrelevant_trigger_condition_fields(condition, &["locator"], "locator_present")?;
            let locator = condition.locator.as_ref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "trigger locator_present condition requires a locator",
                )
            })?;
            if matches!(
                locator,
                CanonicalLocator::Index { .. } | CanonicalLocator::Ref { .. }
            ) {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    "trigger locator_present condition must use a live locator, not index/ref",
                ));
            }
        }
        TriggerConditionKind::UrlMatch => {
            reject_irrelevant_trigger_condition_fields(condition, &["url_pattern"], "url_match")?;
            require_non_empty(condition.url_pattern.as_deref(), "condition.url_pattern")?;
        }
        TriggerConditionKind::Readiness => {
            reject_irrelevant_trigger_condition_fields(
                condition,
                &["readiness_state"],
                "readiness",
            )?;
            require_non_empty(
                condition.readiness_state.as_deref(),
                "condition.readiness_state",
            )?;
        }
        TriggerConditionKind::NetworkRequest => {
            reject_irrelevant_trigger_condition_fields(
                condition,
                &["url_pattern", "method", "status_code"],
                "network_request",
            )?;
            require_non_empty(condition.url_pattern.as_deref(), "condition.url_pattern")?;
        }
        TriggerConditionKind::StorageValue => {
            reject_irrelevant_trigger_condition_fields(
                condition,
                &["storage_area", "key", "value"],
                "storage_value",
            )?;
            require_non_empty(condition.key.as_deref(), "condition.key")?;
        }
    }
    Ok(())
}

pub(crate) fn validate_trigger_action(action: &mut TriggerActionSpec) -> Result<(), RubError> {
    match action.kind {
        TriggerActionKind::BrowserCommand => validate_browser_command_trigger_action(action),
        TriggerActionKind::Workflow => validate_workflow_trigger_action(action),
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Err(RubError::domain(
                ErrorCode::InvalidInput,
                "trigger currently supports action.kind='browser_command' or 'workflow'; provider/script/webhook remain future slices",
            ))
        }
    }
}

fn validate_browser_command_trigger_action(action: &mut TriggerActionSpec) -> Result<(), RubError> {
    let command = require_non_empty(action.command.as_deref(), "action.command")?;
    if !browser_trigger_command_allowed(command) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "trigger browser_command '{command}' is not supported in V1; use one of: click, type, fill, open, reload"
            ),
        ));
    }

    match action.payload.take() {
        Some(payload) if payload.is_object() => {
            reject_reserved_trigger_metadata_keys(payload.as_object().expect("validated object"))?;
            action.payload = Some(payload);
        }
        Some(_) => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "trigger browser_command payload must be a JSON object",
            ));
        }
        None => {
            action.payload = Some(serde_json::json!({}));
        }
    }

    Ok(())
}

fn validate_workflow_trigger_action(action: &mut TriggerActionSpec) -> Result<(), RubError> {
    if action.command.is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow action must not set action.command; encode workflow source in action.payload",
        ));
    }

    let payload = match action.payload.take() {
        Some(payload) if payload.is_object() => payload,
        Some(_) => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "trigger workflow payload must be a JSON object",
            ));
        }
        None => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "trigger workflow action requires action.payload",
            ));
        }
    };

    let object = payload.as_object().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InternalError,
            "workflow trigger payload was not a JSON object after validation",
        )
    })?;
    reject_reserved_trigger_metadata_keys(object)?;
    let has_name = object
        .get("workflow_name")
        .and_then(|value| value.as_str())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_steps = object
        .get("steps")
        .and_then(|value| value.as_array())
        .map(|steps| !steps.is_empty())
        .unwrap_or(false);

    if let Some(vars) = object.get("vars") {
        validate_workflow_vars(vars)?;
    }
    validate_trigger_workflow_bindings(object)?;
    validate_inline_trigger_workflow_steps(object)?;

    match (has_name, has_steps) {
        (true, false) => {
            let name = object
                .get("workflow_name")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::InternalError,
                        "workflow_name key absent after has_name validation",
                    )
                })?;
            let normalized = normalize_workflow_name(name)?;
            let mut payload = payload;
            let object = payload.as_object_mut().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InternalError,
                    "workflow trigger payload was not a JSON object after validation",
                )
            })?;
            object.insert("workflow_name".to_string(), serde_json::json!(normalized));
            action.payload = Some(payload);
            Ok(())
        }
        (false, true) => {
            action.payload = Some(payload);
            Ok(())
        }
        (true, true) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload must provide exactly one of payload.workflow_name or payload.steps",
        )),
        (false, false) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload requires non-empty payload.workflow_name or payload.steps",
        )),
    }
}

fn validate_inline_trigger_workflow_steps(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), RubError> {
    let Some(steps) = payload.get("steps").and_then(|value| value.as_array()) else {
        return Ok(());
    };
    for (index, step) in steps.iter().enumerate() {
        let Some(command) = step
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let args = step.get("args").unwrap_or(&serde_json::Value::Null);
        if !trigger_workflow_request_allowed(command, args) {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("trigger workflow step command '{command}' is not supported"),
                serde_json::json!({
                    "reason": "trigger_workflow_step_not_supported",
                    "step_index": index,
                    "command": command,
                    "allowed_commands": trigger_workflow_allowed_step_descriptions(),
                }),
            ));
        }
    }
    Ok(())
}

fn validate_workflow_vars(vars: &serde_json::Value) -> Result<(), RubError> {
    let object = vars.as_object().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload.vars must be a JSON object of string bindings",
        )
    })?;
    for (key, value) in object {
        if !is_valid_workflow_var_name(key) {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Invalid trigger workflow var '{key}'; use letters, digits, and underscores"
                ),
            ));
        }
        if !value.is_string() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("trigger workflow var '{key}' must be a string value"),
            ));
        }
    }
    Ok(())
}

fn is_valid_workflow_var_name(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn reject_reserved_trigger_metadata_keys(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), RubError> {
    for key in ["_trigger", "_orchestration"] {
        if object.contains_key(key) {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "trigger action payload must not include reserved internal metadata key '{key}'"
                ),
            ));
        }
    }
    Ok(())
}

fn browser_trigger_command_allowed(command: &str) -> bool {
    matches!(command, "click" | "type" | "fill" | "open" | "reload")
}

fn require_non_empty<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, RubError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("trigger requires non-empty {field}"),
            )
        })
}

fn normalize_optional_key(key: &mut Option<String>, field: &str) -> Result<(), RubError> {
    if let Some(value) = key {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("trigger requires non-empty {field}"),
            ));
        }
        *value = trimmed.to_string();
    }
    Ok(())
}

fn reject_irrelevant_trigger_condition_fields(
    condition: &rub_core::model::TriggerConditionSpec,
    allowed_fields: &[&str],
    kind_name: &str,
) -> Result<(), RubError> {
    for (field, present) in [
        ("locator", condition.locator.is_some()),
        ("text", condition.text.is_some()),
        ("url_pattern", condition.url_pattern.is_some()),
        ("readiness_state", condition.readiness_state.is_some()),
        ("method", condition.method.is_some()),
        ("status_code", condition.status_code.is_some()),
        ("storage_area", condition.storage_area.is_some()),
        ("key", condition.key.is_some()),
        ("value", condition.value.is_some()),
    ] {
        if present && !allowed_fields.contains(&field) {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("trigger {kind_name} condition must not set condition.{field}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_trigger_condition;
    use rub_core::model::{TriggerConditionKind, TriggerConditionSpec};
    use rub_core::storage::StorageArea;

    fn text_present_condition() -> TriggerConditionSpec {
        TriggerConditionSpec {
            kind: TriggerConditionKind::TextPresent,
            locator: None,
            text: Some("Ready".to_string()),
            url_pattern: None,
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        }
    }

    #[test]
    fn trigger_condition_rejects_irrelevant_known_field_for_text_present() {
        let mut condition = text_present_condition();
        condition.url_pattern = Some("/ready".to_string());

        let error = validate_trigger_condition(&condition)
            .expect_err("kind-irrelevant known fields must fail closed");
        assert!(error.to_string().contains("condition.url_pattern"));
    }

    #[test]
    fn trigger_network_request_condition_accepts_method_and_status_code_filters() {
        let condition = TriggerConditionSpec {
            kind: TriggerConditionKind::NetworkRequest,
            locator: None,
            text: None,
            url_pattern: Some("/api/reply".to_string()),
            readiness_state: None,
            method: Some("POST".to_string()),
            status_code: Some(201),
            storage_area: None,
            key: None,
            value: None,
        };

        validate_trigger_condition(&condition)
            .expect("network request condition should accept method/status filters");
    }

    #[test]
    fn trigger_storage_value_condition_rejects_irrelevant_url_pattern() {
        let condition = TriggerConditionSpec {
            kind: TriggerConditionKind::StorageValue,
            locator: None,
            text: None,
            url_pattern: Some("/reply".to_string()),
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: Some(StorageArea::Local),
            key: Some("reply_state".to_string()),
            value: Some("done".to_string()),
        };

        let error = validate_trigger_condition(&condition)
            .expect_err("storage_value must reject unrelated known fields");
        assert!(error.to_string().contains("condition.url_pattern"));
    }
}
