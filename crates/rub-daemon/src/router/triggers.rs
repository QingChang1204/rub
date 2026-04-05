use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::CanonicalLocator;
use rub_core::model::{
    TabInfo, TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerInfo,
    TriggerRegistrationSpec, TriggerStatus, TriggerTabBindingInfo,
};

use crate::runtime_refresh::refresh_live_trigger_runtime;
use crate::session::SessionState;
use crate::trigger_workflow_bridge::validate_trigger_workflow_bindings;
use crate::workflow_assets::normalize_workflow_name;

use super::DaemonRouter;
use super::request_args::{parse_json_args, parse_json_spec};

#[derive(Debug)]
enum TriggerCommand {
    Add(TriggerAddArgs),
    List,
    Trace(TriggerTraceArgs),
    Remove(TriggerIdArgs),
    Pause(TriggerIdArgs),
    Resume(TriggerIdArgs),
}

impl TriggerCommand {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("list")
        {
            "add" => Ok(Self::Add(parse_json_args(args, "trigger add")?)),
            "list" => Ok(Self::List),
            "trace" => Ok(Self::Trace(parse_json_args(args, "trigger trace")?)),
            "remove" => Ok(Self::Remove(parse_json_args(args, "trigger remove")?)),
            "pause" => Ok(Self::Pause(parse_json_args(args, "trigger pause")?)),
            "resume" => Ok(Self::Resume(parse_json_args(args, "trigger resume")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown trigger subcommand '{other}'"),
            )),
        }
    }

    async fn execute(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        match self {
            Self::Add(args) => cmd_trigger_add(router, args, state).await,
            Self::List => cmd_trigger_list(router, state).await,
            Self::Trace(args) => cmd_trigger_trace(router, args, state).await,
            Self::Remove(args) => cmd_trigger_remove(router, args.id, state).await,
            Self::Pause(args) => {
                update_trigger_status(router, args.id, state, TriggerStatus::Paused).await
            }
            Self::Resume(args) => {
                update_trigger_status(router, args.id, state, TriggerStatus::Armed).await
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerAddArgs {
    #[serde(rename = "sub")]
    _sub: String,
    spec: String,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    spec_source: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerTraceArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default = "default_trigger_trace_last")]
    last: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerIdArgs {
    #[serde(rename = "sub")]
    _sub: String,
    id: u32,
}

const fn default_trigger_trace_last() -> u64 {
    20
}

pub(super) async fn cmd_trigger(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    TriggerCommand::parse(args)?.execute(router, state).await
}

fn trigger_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: &rub_core::model::TriggerRuntimeInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

fn trigger_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "trigger_registry",
    })
}

fn trigger_subject(id: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "trigger",
        "id": id,
    })
}

async fn cmd_trigger_add(
    router: &DaemonRouter,
    args: TriggerAddArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let mut spec = parse_json_spec::<TriggerRegistrationSpec>(&args.spec, "trigger add")?;
    validate_trigger_registration_spec(&mut spec)?;

    let tabs = refresh_live_trigger_runtime(&router.browser, state).await?;
    let source_tab = resolve_trigger_tab_binding(&tabs, spec.source_tab, "source")?;
    let target_tab = resolve_trigger_tab_binding(&tabs, spec.target_tab, "target")?;

    let existing_trigger =
        state.triggers().await.into_iter().find(|trigger| {
            trigger_registration_reusable(trigger, &source_tab, &target_tab, &spec)
        });
    let trigger = if let Some(existing) = existing_trigger {
        existing
    } else {
        state
            .register_trigger(TriggerInfo {
                id: 0,
                status: if args.paused {
                    TriggerStatus::Paused
                } else {
                    TriggerStatus::Armed
                },
                mode: spec.mode,
                source_tab,
                target_tab,
                condition: spec.condition,
                action: spec.action,
                last_condition_evidence: None,
                consumed_evidence_fingerprint: None,
                last_action_result: None,
                unavailable_reason: None,
            })
            .await
    };
    state.reconcile_trigger_runtime(&tabs).await;
    let runtime = state.trigger_runtime().await;
    let trigger = runtime
        .triggers
        .iter()
        .find(|entry| entry.id == trigger.id)
        .cloned()
        .unwrap_or(trigger);

    Ok(trigger_payload(
        trigger_subject(trigger.id),
        serde_json::json!({
            "trigger": trigger,
            "spec_source": args.spec_source.unwrap_or_else(|| serde_json::json!({ "kind": "inline" })),
        }),
        &runtime,
    ))
}

async fn cmd_trigger_list(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let runtime = state.trigger_runtime().await;
    Ok(trigger_payload(
        trigger_registry_subject(),
        serde_json::json!({
            "items": runtime.triggers.clone(),
        }),
        &runtime,
    ))
}

async fn cmd_trigger_trace(
    router: &DaemonRouter,
    args: TriggerTraceArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let last = usize::try_from(args.last).unwrap_or(usize::MAX);
    let runtime = state.trigger_runtime().await;
    let trace = state.trigger_trace(last).await;
    Ok(trigger_payload(
        serde_json::json!({
            "kind": "trigger_trace",
            "last": last,
        }),
        serde_json::to_value(trace).map_err(RubError::from)?,
        &runtime,
    ))
}

async fn cmd_trigger_remove(
    router: &DaemonRouter,
    id: u32,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let removed = state.remove_trigger(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Trigger id {id} is not present in the current registry"),
        )
    })?;
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let runtime = state.trigger_runtime().await;
    Ok(trigger_payload(
        trigger_subject(id),
        serde_json::json!({
            "removed": removed,
        }),
        &runtime,
    ))
}

async fn update_trigger_status(
    router: &DaemonRouter,
    id: u32,
    state: &Arc<SessionState>,
    next_status: TriggerStatus,
) -> Result<serde_json::Value, RubError> {
    let current = state
        .triggers()
        .await
        .into_iter()
        .find(|trigger| trigger.id == id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Trigger id {id} is not present in the current registry"),
            )
        })?;

    let trigger = match (current.status, next_status) {
        (TriggerStatus::Armed, TriggerStatus::Paused)
        | (TriggerStatus::Paused, TriggerStatus::Armed) => state
            .set_trigger_status(id, next_status)
            .await
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Trigger id {id} disappeared while applying status update"
                ))
            })?,
        (status, requested) if status == requested => current,
        _ => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Trigger id {id} cannot transition from '{}' to '{}'",
                    trigger_status_name(current.status),
                    trigger_status_name(next_status),
                ),
            ));
        }
    };

    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let runtime = state.trigger_runtime().await;
    let trigger = runtime
        .triggers
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(trigger);
    Ok(trigger_payload(
        trigger_subject(id),
        serde_json::json!({
            "trigger": trigger,
        }),
        &runtime,
    ))
}

#[cfg(test)]
mod id_tests {
    use super::TriggerIdArgs;
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;

    #[test]
    fn typed_trigger_id_payload_rejects_values_larger_than_u32() {
        let error = parse_json_args::<TriggerIdArgs>(
            &serde_json::json!({"sub": "pause", "id": u64::from(u32::MAX) + 1}),
            "trigger pause",
        )
        .expect_err("oversized id should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }
}

fn resolve_trigger_tab_binding(
    tabs: &[TabInfo],
    index: u32,
    role: &str,
) -> Result<TriggerTabBindingInfo, RubError> {
    let tab = tabs.iter().find(|tab| tab.index == index).ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("{role} tab index {index} is not present in the current session"),
        )
    })?;
    Ok(TriggerTabBindingInfo {
        index: tab.index,
        target_id: tab.target_id.clone(),
        url: tab.url.clone(),
        title: tab.title.clone(),
    })
}

fn validate_trigger_registration_spec(spec: &mut TriggerRegistrationSpec) -> Result<(), RubError> {
    validate_trigger_condition(&spec.condition)?;
    validate_trigger_action(&mut spec.action)
}

fn trigger_registration_equivalent(
    existing: &TriggerInfo,
    source_tab: &TriggerTabBindingInfo,
    target_tab: &TriggerTabBindingInfo,
    spec: &TriggerRegistrationSpec,
) -> bool {
    existing.mode == spec.mode
        && existing.source_tab.target_id == source_tab.target_id
        && existing.target_tab.target_id == target_tab.target_id
        && existing.condition == spec.condition
        && existing.action == spec.action
}

fn trigger_registration_reusable(
    existing: &TriggerInfo,
    source_tab: &TriggerTabBindingInfo,
    target_tab: &TriggerTabBindingInfo,
    spec: &TriggerRegistrationSpec,
) -> bool {
    matches!(existing.status, TriggerStatus::Armed)
        && trigger_registration_equivalent(existing, source_tab, target_tab, spec)
}

pub(super) fn validate_trigger_condition(
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

pub(super) fn validate_trigger_action(action: &mut TriggerActionSpec) -> Result<(), RubError> {
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

fn trigger_status_name(status: TriggerStatus) -> &'static str {
    match status {
        TriggerStatus::Armed => "armed",
        TriggerStatus::Paused => "paused",
        TriggerStatus::Fired => "fired",
        TriggerStatus::Blocked => "blocked",
        TriggerStatus::Degraded => "degraded",
        TriggerStatus::Expired => "expired",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_trigger_tab_binding, trigger_registration_equivalent,
        trigger_registration_reusable, validate_trigger_action, validate_trigger_condition,
    };
    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        TabInfo, TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerInfo, TriggerMode, TriggerRegistrationSpec, TriggerStatus, TriggerTabBindingInfo,
    };
    use serde_json::json;

    #[test]
    fn resolve_trigger_tab_binding_captures_stable_target_identity() {
        let binding = resolve_trigger_tab_binding(
            &[TabInfo {
                index: 4,
                target_id: "page-target-4".to_string(),
                url: "https://example.com/source".to_string(),
                title: "Source".to_string(),
                active: true,
            }],
            4,
            "source",
        )
        .expect("binding should resolve");

        assert_eq!(binding.target_id, "page-target-4");
        assert_eq!(binding.index, 4);
    }

    #[test]
    fn validate_trigger_condition_rejects_snapshot_bound_locators() {
        let error = validate_trigger_condition(&TriggerConditionSpec {
            kind: TriggerConditionKind::LocatorPresent,
            locator: Some(CanonicalLocator::Index { index: 7 }),
            text: None,
            url_pattern: None,
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        })
        .expect_err("index locators should be rejected");
        assert!(
            error
                .to_string()
                .contains("must use a live locator, not index/ref")
        );
    }

    #[test]
    fn validate_trigger_action_rejects_future_action_kinds() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Provider,
            command: None,
            payload: None,
        };
        let error = validate_trigger_action(&mut action).expect_err("provider should be rejected");
        assert!(error.to_string().contains("browser_command"));
        assert!(error.to_string().contains("workflow"));
    }

    #[test]
    fn validate_trigger_action_normalizes_missing_payload_to_empty_object() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::BrowserCommand,
            command: Some("click".to_string()),
            payload: None,
        };
        validate_trigger_action(&mut action).expect("browser command should validate");
        assert_eq!(action.payload, Some(json!({})));
    }

    #[test]
    fn validate_trigger_action_accepts_named_workflow_payload() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "workflow_name": "reply_flow.json"
            })),
        };
        validate_trigger_action(&mut action).expect("named workflow should validate");
        assert_eq!(
            action.payload.as_ref().unwrap()["workflow_name"],
            json!("reply_flow")
        );
    }

    #[test]
    fn validate_trigger_action_accepts_inline_workflow_steps() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "steps": [
                    {"command": "click", "args": {"selector": "#continue"}}
                ]
            })),
        };
        validate_trigger_action(&mut action).expect("inline workflow should validate");
    }

    #[test]
    fn validate_trigger_action_accepts_workflow_vars_object() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "workflow_name": "reply_flow",
                "vars": {
                    "target": "https://example.com"
                }
            })),
        };
        validate_trigger_action(&mut action).expect("vars object should validate");
    }

    #[test]
    fn validate_trigger_action_accepts_workflow_source_vars_object() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "workflow_name": "reply_flow",
                "source_vars": {
                    "reply_name": {
                        "kind": "text",
                        "selector": "#question",
                        "first": true
                    }
                }
            })),
        };
        validate_trigger_action(&mut action).expect("source vars object should validate");
    }

    #[test]
    fn validate_trigger_action_rejects_invalid_workflow_name() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "workflow_name": "../bad"
            })),
        };
        let error =
            validate_trigger_action(&mut action).expect_err("invalid workflow name should fail");
        assert!(error.to_string().contains("Invalid workflow name"));
    }

    #[test]
    fn validate_trigger_action_rejects_non_string_workflow_vars() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "workflow_name": "reply_flow",
                "vars": {
                    "target": 42
                }
            })),
        };
        let error = validate_trigger_action(&mut action).expect_err("non-string vars should fail");
        assert!(error.to_string().contains("must be a string value"));
    }

    #[test]
    fn validate_trigger_action_rejects_duplicate_explicit_and_source_vars() {
        let mut action = TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(json!({
                "workflow_name": "reply_flow",
                "vars": {
                    "reply_name": "Grace"
                },
                "source_vars": {
                    "reply_name": {
                        "kind": "text",
                        "selector": "#question"
                    }
                }
            })),
        };
        let error = validate_trigger_action(&mut action)
            .expect_err("duplicate explicit/source vars should fail");
        assert!(error.to_string().contains("duplicates variable bindings"));
    }

    #[test]
    fn trigger_registration_equivalence_reuses_semantically_identical_trigger() {
        let source_tab = TriggerTabBindingInfo {
            index: 1,
            target_id: "tab-source".to_string(),
            url: "https://example.com/source".to_string(),
            title: "Source".to_string(),
        };
        let target_tab = TriggerTabBindingInfo {
            index: 2,
            target_id: "tab-target".to_string(),
            url: "https://example.com/target".to_string(),
            title: "Target".to_string(),
        };
        let condition = TriggerConditionSpec {
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
        };
        let action = TriggerActionSpec {
            kind: TriggerActionKind::BrowserCommand,
            command: Some("click".to_string()),
            payload: Some(json!({ "selector": "#continue" })),
        };
        let existing = TriggerInfo {
            id: 17,
            status: TriggerStatus::Armed,
            mode: TriggerMode::Once,
            source_tab: source_tab.clone(),
            target_tab: target_tab.clone(),
            condition: condition.clone(),
            action: action.clone(),
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        };
        let spec = TriggerRegistrationSpec {
            mode: TriggerMode::Once,
            source_tab: source_tab.index,
            target_tab: target_tab.index,
            condition,
            action,
        };

        assert!(trigger_registration_equivalent(
            &existing,
            &source_tab,
            &target_tab,
            &spec,
        ));
    }

    #[test]
    fn trigger_registration_equivalence_does_not_make_non_armed_trigger_reusable() {
        let source_tab = TriggerTabBindingInfo {
            index: 1,
            target_id: "tab-source".to_string(),
            url: "https://example.com/source".to_string(),
            title: "Source".to_string(),
        };
        let target_tab = TriggerTabBindingInfo {
            index: 2,
            target_id: "tab-target".to_string(),
            url: "https://example.com/target".to_string(),
            title: "Target".to_string(),
        };
        let spec = TriggerRegistrationSpec {
            mode: TriggerMode::Once,
            source_tab: source_tab.index,
            target_tab: target_tab.index,
            condition: TriggerConditionSpec {
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
            },
            action: TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(json!({ "selector": "#continue" })),
            },
        };
        let existing = TriggerInfo {
            id: 99,
            status: TriggerStatus::Degraded,
            mode: spec.mode,
            source_tab: source_tab.clone(),
            target_tab: target_tab.clone(),
            condition: spec.condition.clone(),
            action: spec.action.clone(),
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        };

        assert!(trigger_registration_equivalent(
            &existing,
            &source_tab,
            &target_tab,
            &spec,
        ));
        assert!(!trigger_registration_reusable(
            &existing,
            &source_tab,
            &target_tab,
            &spec,
        ));
    }
}
