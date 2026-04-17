use std::path::Path;
use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationActionExecutionInfo, OrchestrationResultInfo, OrchestrationRuleInfo,
    OrchestrationRuleStatus, OrchestrationRuntimeInfo,
};
use tracing::{info, warn};

use crate::router::DaemonRouter;
use crate::session::SessionState;
use uuid::Uuid;

mod action_request;
mod dispatch;
mod outcome;
mod protocol;
mod retry;
pub(crate) mod target;

#[cfg(test)]
use action_request::orchestration_action_execution_info;
#[cfg(test)]
use action_request::orchestration_step_command_id;
#[cfg(test)]
use action_request::resolve_orchestration_workflow_spec;
#[cfg(test)]
use dispatch::action_requires_source_materialization;
use dispatch::dispatch_orchestration_action;
pub(crate) use outcome::{
    OrchestrationFailureInput, classify_orchestration_error_status, orchestration_failure_result,
};
use outcome::{successful_cooldown_until_ms, successful_next_status};
pub(crate) use protocol::{
    RemoteDispatchContract, bind_orchestration_daemon_authority,
    decode_orchestration_success_payload, decode_orchestration_success_payload_field,
    decode_orchestration_success_result_items, dispatch_remote_orchestration_request,
    ensure_orchestration_success_response,
};
use target::resolve_target_session;

const ORCHESTRATION_ACTION_BASE_TIMEOUT_MS: u64 = 30_000;
const ORCHESTRATION_TRANSIENT_RETRY_LIMIT: u32 = 3;
const ORCHESTRATION_TRANSIENT_RETRY_DELAY_MS: u64 = 100;

struct OrchestrationActionFailure {
    action: Option<OrchestrationActionExecutionInfo>,
    error: ErrorEnvelope,
    attempts: u32,
}

#[derive(Clone, Copy)]
struct OrchestrationExecutionContext<'a> {
    router: &'a DaemonRouter,
    state: &'a Arc<SessionState>,
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &'a OrchestrationRuleInfo,
    execution_id: &'a str,
    command_identity_key: Option<&'a str>,
    rub_home: &'a Path,
}

pub(crate) async fn execute_orchestration_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    command_identity_key: Option<&str>,
) -> OrchestrationResultInfo {
    let total_steps = rule.actions.len() as u32;
    let mut steps = Vec::new();
    let execution_id = Uuid::now_v7().to_string();
    info!(
        rule_id = rule.id,
        total_steps,
        execution_id = execution_id.as_str(),
        "orchestration_rule.start"
    );
    let context = OrchestrationExecutionContext {
        router,
        state,
        runtime,
        rule,
        execution_id: &execution_id,
        command_identity_key,
        rub_home: &state.rub_home,
    };
    let target_session = match resolve_target_session(runtime, rule) {
        Ok(session) => session,
        Err(error) => {
            warn!(
                rule_id = rule.id,
                error_code = %error.code,
                reason = error.context.as_ref()
                    .and_then(|c| c.get("reason")).and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
                "orchestration_rule.target_resolve_failed"
            );
            return orchestration_failure_result(OrchestrationFailureInput {
                rule_id: rule.id,
                retained_status: rule.status,
                total_steps,
                failed_step_index: 0,
                committed_steps: steps,
                failed_action: None,
                failed_attempts: 1,
                error,
            });
        }
    };

    for (step_index, action) in rule.actions.iter().enumerate() {
        let step_index = step_index as u32;
        match dispatch_orchestration_action(context, target_session, step_index, action).await {
            Ok(step) => steps.push(step),
            Err(failure) => {
                warn!(
                    rule_id = rule.id,
                    step_index,
                    failed_attempts = failure.attempts,
                    error_code = %failure.error.code,
                    reason = failure.error.context.as_ref()
                        .and_then(|c| c.get("reason")).and_then(|v| v.as_str())
                        .unwrap_or("unknown"),
                    "orchestration_rule.step_failed"
                );
                return orchestration_failure_result(OrchestrationFailureInput {
                    rule_id: rule.id,
                    retained_status: rule.status,
                    total_steps,
                    failed_step_index: step_index,
                    committed_steps: steps,
                    failed_action: failure.action,
                    failed_attempts: failure.attempts,
                    error: failure.error,
                });
            }
        }
    }

    info!(
        rule_id = rule.id,
        total_steps,
        execution_id = execution_id.as_str(),
        "orchestration_rule.committed"
    );
    OrchestrationResultInfo {
        rule_id: rule.id,
        status: OrchestrationRuleStatus::Fired,
        next_status: successful_next_status(rule),
        summary: format!(
            "orchestration rule {} committed {}/{} action(s)",
            rule.id, total_steps, total_steps
        ),
        committed_steps: total_steps,
        total_steps,
        steps,
        cooldown_until_ms: successful_cooldown_until_ms(rule),
        error_code: None,
        reason: None,
    }
}

#[cfg(test)]
mod tests;
