use crate::trigger_workflow_bridge::validate_trigger_workflow_bindings;
use crate::workflow_assets::normalize_workflow_name;
use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::CanonicalLocator;
use rub_core::model::{
    TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerRegistrationSpec,
};

pub(super) fn validate_trigger_registration_spec(
    spec: &mut TriggerRegistrationSpec,
) -> Result<(), RubError> {
    validate_trigger_condition(&spec.condition)?;
    validate_trigger_action(&mut spec.action)
}

pub(crate) fn validate_trigger_condition(
    condition: &rub_core::model::TriggerConditionSpec,
) -> Result<(), RubError> {
    match condition.kind {
        TriggerConditionKind::TextPresent => {
            require_non_empty(condition.text.as_deref(), "condition.text")?;
        }
        TriggerConditionKind::LocatorPresent => {
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
            require_non_empty(condition.url_pattern.as_deref(), "condition.url_pattern")?;
        }
        TriggerConditionKind::Readiness => {
            require_non_empty(
                condition.readiness_state.as_deref(),
                "condition.readiness_state",
            )?;
        }
        TriggerConditionKind::NetworkRequest => {
            require_non_empty(condition.url_pattern.as_deref(), "condition.url_pattern")?;
        }
        TriggerConditionKind::StorageValue => {
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
                "trigger browser_command '{command}' is not supported in V1; use one of: click, type, fill, open, reload, exec"
            ),
        ));
    }

    match action.payload.take() {
        Some(payload) if payload.is_object() => {
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

fn browser_trigger_command_allowed(command: &str) -> bool {
    matches!(
        command,
        "click" | "type" | "fill" | "open" | "reload" | "exec"
    )
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
