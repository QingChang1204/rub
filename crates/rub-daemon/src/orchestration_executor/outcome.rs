use std::time::{SystemTime, UNIX_EPOCH};

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationMode, OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
    OrchestrationStepResultInfo, OrchestrationStepStatus,
};

pub(crate) struct OrchestrationFailureInput {
    pub(crate) rule_id: u32,
    pub(crate) retained_status: OrchestrationRuleStatus,
    pub(crate) total_steps: u32,
    pub(crate) failed_step_index: u32,
    pub(crate) committed_steps: Vec<OrchestrationStepResultInfo>,
    pub(crate) failed_action: Option<rub_core::model::OrchestrationActionExecutionInfo>,
    pub(crate) failed_attempts: u32,
    pub(crate) error: ErrorEnvelope,
}

pub(crate) fn orchestration_failure_result(
    input: OrchestrationFailureInput,
) -> OrchestrationResultInfo {
    let OrchestrationFailureInput {
        rule_id,
        retained_status,
        total_steps,
        failed_step_index,
        mut committed_steps,
        failed_action,
        failed_attempts,
        error,
    } = input;
    let status = classify_orchestration_error_status(error.code);
    let step_status = match status {
        OrchestrationRuleStatus::Blocked => OrchestrationStepStatus::Blocked,
        OrchestrationRuleStatus::Degraded => OrchestrationStepStatus::Degraded,
        OrchestrationRuleStatus::Armed
        | OrchestrationRuleStatus::Paused
        | OrchestrationRuleStatus::Fired
        | OrchestrationRuleStatus::Expired => OrchestrationStepStatus::Blocked,
    };
    let reason = error
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    committed_steps.push(OrchestrationStepResultInfo {
        step_index: failed_step_index,
        status: step_status,
        summary: format!(
            "orchestration step {} failed: {}: {}",
            failed_step_index + 1,
            error.code,
            error.message
        ),
        attempts: failed_attempts,
        action: failed_action,
        result: None,
        error_code: Some(error.code),
        reason: reason.clone(),
    });
    OrchestrationResultInfo {
        rule_id,
        status,
        next_status: if failed_step_index == 0 {
            retained_status
        } else {
            status
        },
        summary: format!(
            "orchestration rule {} {} after committing {}/{} action(s)",
            rule_id,
            match status {
                OrchestrationRuleStatus::Blocked => "blocked",
                OrchestrationRuleStatus::Degraded => "degraded",
                OrchestrationRuleStatus::Armed
                | OrchestrationRuleStatus::Paused
                | OrchestrationRuleStatus::Fired
                | OrchestrationRuleStatus::Expired => "stopped",
            },
            failed_step_index,
            total_steps
        ),
        committed_steps: failed_step_index,
        total_steps,
        steps: committed_steps,
        cooldown_until_ms: None,
        error_code: Some(error.code),
        reason,
    }
}

pub(super) fn successful_next_status(rule: &OrchestrationRuleInfo) -> OrchestrationRuleStatus {
    match rule.mode {
        OrchestrationMode::Once => OrchestrationRuleStatus::Fired,
        OrchestrationMode::Repeat => OrchestrationRuleStatus::Armed,
    }
}

pub(super) fn successful_cooldown_until_ms(rule: &OrchestrationRuleInfo) -> Option<u64> {
    match rule.mode {
        OrchestrationMode::Once => None,
        OrchestrationMode::Repeat if rule.execution_policy.cooldown_ms == 0 => None,
        OrchestrationMode::Repeat => {
            Some(current_time_ms().saturating_add(rule.execution_policy.cooldown_ms))
        }
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn classify_orchestration_error_status(code: ErrorCode) -> OrchestrationRuleStatus {
    match code {
        ErrorCode::InvalidInput
        | ErrorCode::ElementNotFound
        | ErrorCode::ElementNotInteractable
        | ErrorCode::StaleSnapshot
        | ErrorCode::StaleIndex
        | ErrorCode::WaitTimeout
        | ErrorCode::NoMatchingOption
        | ErrorCode::FileNotFound
        | ErrorCode::AutomationPaused => OrchestrationRuleStatus::Blocked,
        _ => OrchestrationRuleStatus::Degraded,
    }
}
