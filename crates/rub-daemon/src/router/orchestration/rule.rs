use std::time::{SystemTime, UNIX_EPOCH};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationAddressSpec, OrchestrationRegistrationSpec,
    OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
};

pub(super) fn validate_orchestration_registration_spec(
    spec: &mut OrchestrationRegistrationSpec,
) -> Result<(), RubError> {
    normalize_optional_key(&mut spec.source.frame_id, "source.frame_id")?;
    normalize_optional_key(&mut spec.target.frame_id, "target.frame_id")?;
    validate_orchestration_address(&spec.source, "source")?;
    validate_orchestration_address(&spec.target, "target")?;
    super::super::triggers::validate_trigger_condition(&spec.condition)?;
    if spec.actions.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "orchestration requires at least one action",
        ));
    }
    for action in &mut spec.actions {
        super::super::triggers::validate_trigger_action(action)?;
    }
    validate_orchestration_execution_policy(spec)?;
    normalize_optional_key(&mut spec.correlation_key, "correlation_key")?;
    normalize_optional_key(&mut spec.idempotency_key, "idempotency_key")?;
    Ok(())
}

fn validate_orchestration_execution_policy(
    spec: &OrchestrationRegistrationSpec,
) -> Result<(), RubError> {
    if spec.execution_policy.max_retries > 3 {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "orchestration execution_policy.max_retries must be between 0 and 3",
            serde_json::json!({
                "reason": "orchestration_retry_budget_invalid",
                "max_retries": spec.execution_policy.max_retries,
                "max_supported_retries": 3,
            }),
        ));
    }
    Ok(())
}

fn validate_orchestration_address(
    address: &OrchestrationAddressSpec,
    role: &str,
) -> Result<(), RubError> {
    let session_id = address.session_id.trim();
    if session_id.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("orchestration requires non-empty {role}.session_id"),
        ));
    }
    if address.tab_index.is_some() && address.tab_target_id.is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "orchestration {role} address must not set both {role}.tab_index and {role}.tab_target_id"
            ),
        ));
    }
    if let Some(frame_id) = address.frame_id.as_ref() {
        let trimmed = frame_id.trim();
        if trimmed.is_empty() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("orchestration {role}.frame_id must be non-empty when provided"),
            ));
        }
        if address.tab_index.is_none() && address.tab_target_id.is_none() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "orchestration {role}.frame_id requires {role}.tab_index or {role}.tab_target_id"
                ),
            ));
        }
    }
    Ok(())
}

fn normalize_optional_key(key: &mut Option<String>, field: &str) -> Result<(), RubError> {
    if let Some(value) = key {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("orchestration requires non-empty {field}"),
            ));
        }
        *value = trimmed.to_string();
    }
    Ok(())
}

pub(super) fn orchestration_status_name(status: OrchestrationRuleStatus) -> &'static str {
    match status {
        OrchestrationRuleStatus::Armed => "armed",
        OrchestrationRuleStatus::Paused => "paused",
        OrchestrationRuleStatus::Fired => "fired",
        OrchestrationRuleStatus::Blocked => "blocked",
        OrchestrationRuleStatus::Degraded => "degraded",
        OrchestrationRuleStatus::Expired => "expired",
    }
}

pub(super) fn orchestration_rule_in_cooldown(rule: &OrchestrationRuleInfo) -> bool {
    rule.execution_policy
        .cooldown_until_ms
        .map(|until| until > current_time_ms())
        .unwrap_or(false)
}

pub(super) fn blocked_cooldown_result(rule: &OrchestrationRuleInfo) -> OrchestrationResultInfo {
    OrchestrationResultInfo {
        rule_id: rule.id,
        status: OrchestrationRuleStatus::Blocked,
        next_status: OrchestrationRuleStatus::Armed,
        summary: format!(
            "orchestration rule {} is cooling down before the next repeat execution window",
            rule.id
        ),
        committed_steps: 0,
        total_steps: rule.actions.len() as u32,
        steps: Vec::new(),
        cooldown_until_ms: rule.execution_policy.cooldown_until_ms,
        error_code: Some(ErrorCode::WaitTimeout),
        reason: Some("orchestration_cooldown_active".to_string()),
        error_context: None,
    }
}

pub(super) fn orchestration_rule_to_registration_spec(
    rule: &OrchestrationRuleInfo,
) -> OrchestrationRegistrationSpec {
    OrchestrationRegistrationSpec {
        source: orchestration_address_info_to_spec(&rule.source),
        target: orchestration_address_info_to_spec(&rule.target),
        mode: rule.mode,
        execution_policy: rub_core::model::OrchestrationExecutionPolicySpec {
            cooldown_ms: rule.execution_policy.cooldown_ms,
            max_retries: rule.execution_policy.max_retries,
        },
        condition: rule.condition.clone(),
        actions: rule.actions.clone(),
        correlation_key: None,
        idempotency_key: None,
    }
}

fn orchestration_address_info_to_spec(
    address: &OrchestrationAddressInfo,
) -> OrchestrationAddressSpec {
    OrchestrationAddressSpec {
        session_id: address.session_id.clone(),
        tab_index: if address.tab_target_id.is_some() {
            None
        } else {
            address.tab_index
        },
        tab_target_id: address.tab_target_id.clone(),
        frame_id: address.frame_id.clone(),
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        orchestration_rule_to_registration_spec, validate_orchestration_registration_spec,
    };
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationAddressSpec, OrchestrationExecutionPolicyInfo,
        OrchestrationExecutionPolicySpec, OrchestrationMode, OrchestrationRegistrationSpec,
        OrchestrationRuleInfo, OrchestrationRuleStatus, TriggerActionKind, TriggerActionSpec,
        TriggerConditionKind, TriggerConditionSpec,
    };
    use serde_json::json;

    fn spec() -> OrchestrationRegistrationSpec {
        OrchestrationRegistrationSpec {
            source: OrchestrationAddressSpec {
                session_id: "sess-source".to_string(),
                tab_index: None,
                tab_target_id: None,
                frame_id: None,
            },
            target: OrchestrationAddressSpec {
                session_id: "sess-target".to_string(),
                tab_index: Some(1),
                tab_target_id: None,
                frame_id: None,
            },
            mode: OrchestrationMode::Once,
            execution_policy: OrchestrationExecutionPolicySpec::default(),
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
                kind: TriggerActionKind::Workflow,
                command: None,
                payload: Some(json!({ "workflow_name": "reply_flow" })),
            }],
            correlation_key: None,
            idempotency_key: None,
        }
    }

    #[test]
    fn orchestration_registration_accepts_session_scoped_rule() {
        let mut spec = spec();
        validate_orchestration_registration_spec(&mut spec)
            .expect("orchestration registration should validate");
    }

    #[test]
    fn orchestration_registration_rejects_duplicate_tab_selectors() {
        let mut spec = spec();
        spec.target.tab_target_id = Some("page-target-2".to_string());
        let error = validate_orchestration_registration_spec(&mut spec)
            .expect_err("duplicate tab selectors should fail");
        assert!(error.to_string().contains("tab_index"));
        assert!(error.to_string().contains("tab_target_id"));
    }

    #[test]
    fn orchestration_registration_rejects_empty_actions() {
        let mut spec = spec();
        spec.actions.clear();
        let error = validate_orchestration_registration_spec(&mut spec)
            .expect_err("empty actions should fail");
        assert!(error.to_string().contains("at least one action"));
    }

    #[test]
    fn orchestration_registration_accepts_bounded_retry_policy() {
        let mut spec = spec();
        spec.execution_policy.max_retries = 3;
        validate_orchestration_registration_spec(&mut spec)
            .expect("bounded retry policy should be accepted");
    }

    #[test]
    fn orchestration_registration_rejects_retry_policy_above_supported_budget() {
        let mut spec = spec();
        spec.execution_policy.max_retries = 4;
        let error = validate_orchestration_registration_spec(&mut spec)
            .expect_err("retry policy above budget should fail");
        assert!(error.to_string().contains("max_retries"));
    }

    #[test]
    fn orchestration_export_projection_strips_live_identity_fields() {
        let exported = orchestration_rule_to_registration_spec(&OrchestrationRuleInfo {
            id: 9,
            status: OrchestrationRuleStatus::Armed,
            lifecycle_generation: 1,
            source: OrchestrationAddressInfo {
                session_id: "sess-source".to_string(),
                session_name: "source".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("SOURCE_TAB".to_string()),
                frame_id: Some("SOURCE_FRAME".to_string()),
            },
            target: OrchestrationAddressInfo {
                session_id: "sess-target".to_string(),
                session_name: "target".to_string(),
                tab_index: Some(1),
                tab_target_id: Some("TARGET_TAB".to_string()),
                frame_id: None,
            },
            mode: OrchestrationMode::Repeat,
            execution_policy: OrchestrationExecutionPolicyInfo {
                cooldown_ms: 500,
                max_retries: 2,
                cooldown_until_ms: Some(1234),
            },
            condition: spec().condition,
            actions: spec().actions,
            correlation_key: "corr".to_string(),
            idempotency_key: "idem".to_string(),
            unavailable_reason: Some("target_session_missing".to_string()),
            last_condition_evidence: None,
            last_result: None,
        });

        assert_eq!(exported.source.session_id, "sess-source");
        assert_eq!(exported.source.tab_index, None);
        assert_eq!(exported.source.tab_target_id.as_deref(), Some("SOURCE_TAB"));
        assert_eq!(exported.source.frame_id.as_deref(), Some("SOURCE_FRAME"));
        assert_eq!(exported.target.tab_index, None);
        assert_eq!(exported.target.tab_target_id.as_deref(), Some("TARGET_TAB"));
        assert_eq!(exported.execution_policy.cooldown_ms, 500);
        assert_eq!(exported.execution_policy.max_retries, 2);
        assert!(exported.correlation_key.is_none());
        assert!(exported.idempotency_key.is_none());
    }

    #[test]
    fn orchestration_registration_rejects_frame_binding_without_tab_binding() {
        let mut spec = spec();
        spec.source.frame_id = Some("child-frame".to_string());
        let error = validate_orchestration_registration_spec(&mut spec)
            .expect_err("frame binding without tab binding should fail");
        assert!(error.to_string().contains("source.frame_id"));
        assert!(error.to_string().contains("source.tab_index"));
    }

    #[test]
    fn orchestration_registration_rejects_kind_irrelevant_condition_field() {
        let mut spec = spec();
        spec.condition.url_pattern = Some("/ready".to_string());

        let error = validate_orchestration_registration_spec(&mut spec)
            .expect_err("kind-irrelevant known condition field must fail closed");
        assert!(error.to_string().contains("condition.url_pattern"));
    }
}
