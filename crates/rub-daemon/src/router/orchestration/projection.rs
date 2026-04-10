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
        "projection_kind": "live_rule_identity",
        "projection_authority": "session.orchestration_runtime.rules",
        "upstream_truth": "session_orchestration_rule",
        "canonical_spec_kind": "replayable_orchestration_registration_spec",
        "stripped_from_spec": ["correlation_key", "idempotency_key"],
        "correlation_key": rule.correlation_key,
        "idempotency_key": rule.idempotency_key,
    })
}
