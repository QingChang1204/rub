use rub_core::model::{OrchestrationRuleInfo, OrchestrationRuntimeInfo};

pub(super) fn orchestration_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: &OrchestrationRuntimeInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

pub(super) fn orchestration_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "orchestration_registry",
    })
}

pub(super) fn orchestration_rule_subject(id: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "orchestration_rule",
        "id": id,
    })
}

pub(super) fn orchestration_rule_identity_projection(
    rule: &OrchestrationRuleInfo,
) -> serde_json::Value {
    serde_json::json!({
        "surface": "orchestration_rule_identity",
        "truth_level": "operator_projection",
        "projection_kind": "live_rule_identity",
        "projection_authority": "session.orchestration_runtime.rules",
        "upstream_truth": "session_orchestration_rule",
        "control_role": "display_only",
        "durability": "best_effort",
        "canonical_spec_kind": "replayable_orchestration_registration_spec",
        "stripped_from_spec": ["correlation_key", "idempotency_key"],
        "correlation_key": rule.correlation_key,
        "idempotency_key": rule.idempotency_key,
    })
}

#[cfg(test)]
mod tests {
    use super::orchestration_rule_identity_projection;
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
        OrchestrationRuleInfo, OrchestrationRuleStatus, TriggerActionKind, TriggerActionSpec,
        TriggerConditionKind, TriggerConditionSpec,
    };

    #[test]
    fn orchestration_rule_identity_projection_is_self_describing() {
        let rule = OrchestrationRuleInfo {
            id: 7,
            status: OrchestrationRuleStatus::Armed,
            source: OrchestrationAddressInfo {
                session_id: "source-id".to_string(),
                session_name: "source".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("source-tab".to_string()),
                frame_id: None,
            },
            target: OrchestrationAddressInfo {
                session_id: "target-id".to_string(),
                session_name: "target".to_string(),
                tab_index: Some(1),
                tab_target_id: Some("target-tab".to_string()),
                frame_id: None,
            },
            mode: OrchestrationMode::Once,
            execution_policy: OrchestrationExecutionPolicyInfo::default(),
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
            actions: vec![TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(serde_json::json!({ "selector": "#continue" })),
            }],
            correlation_key: "corr-7".to_string(),
            idempotency_key: "idem-7".to_string(),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        };

        let projection = orchestration_rule_identity_projection(&rule);
        assert_eq!(projection["surface"], "orchestration_rule_identity");
        assert_eq!(projection["truth_level"], "operator_projection");
        assert_eq!(projection["projection_kind"], "live_rule_identity");
        assert_eq!(
            projection["projection_authority"],
            "session.orchestration_runtime.rules"
        );
        assert_eq!(projection["upstream_truth"], "session_orchestration_rule");
        assert_eq!(projection["control_role"], "display_only");
        assert_eq!(projection["durability"], "best_effort");
        assert_eq!(
            projection["canonical_spec_kind"],
            "replayable_orchestration_registration_spec"
        );
        assert_eq!(
            projection["stripped_from_spec"],
            serde_json::json!(["correlation_key", "idempotency_key"])
        );
        assert_eq!(projection["correlation_key"], "corr-7");
        assert_eq!(projection["idempotency_key"], "idem-7");
    }
}
