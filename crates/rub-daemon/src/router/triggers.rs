use std::sync::Arc;

use crate::session::SessionState;
use rub_core::error::RubError;

use super::DaemonRouter;

mod command;
mod mutation;
mod projection;
mod validation;

use command::TriggerCommand;
#[cfg(test)]
use command::TriggerIdArgs;
pub(crate) use validation::{validate_trigger_action, validate_trigger_condition};

pub(super) async fn cmd_trigger(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    TriggerCommand::parse(args)?.execute(router, state).await
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

#[cfg(test)]
mod tests {
    use super::projection::{
        resolve_trigger_tab_binding, trigger_registration_equivalent, trigger_registration_reusable,
    };
    use super::validation::{validate_trigger_action, validate_trigger_condition};
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
