use std::path::Path;
use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationActionExecutionInfo, OrchestrationResultInfo, OrchestrationRuleInfo,
    OrchestrationRuleStatus, OrchestrationRuntimeInfo,
};
use tracing::{info, warn};

use crate::router::timeout_projection::{
    record_orchestration_partial_commit_timeout_projection,
    record_orchestration_pending_step_timeout_projection_with_recovery,
};
use crate::router::{DaemonRouter, RouterFenceDisposition, TransactionDeadline};
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
use action_request::orchestration_request_meta;
use action_request::orchestration_step_command_id;
#[cfg(test)]
use action_request::resolve_orchestration_workflow_spec;
#[cfg(test)]
use action_request::resolve_source_session;
#[cfg(test)]
use dispatch::action_requires_source_materialization;
use dispatch::dispatch_orchestration_action;
pub(crate) use outcome::{
    OrchestrationFailureInput, classify_orchestration_error_status, orchestration_failure_result,
};
use outcome::{successful_cooldown_until_ms, successful_next_status};
#[cfg(test)]
pub(crate) use protocol::bind_orchestration_daemon_authority;
#[cfg(test)]
pub(crate) use protocol::queue_remote_orchestration_connection_for_test;
pub(crate) use protocol::{
    RemoteDispatchContract, bind_live_orchestration_phase_command_id,
    bounded_orchestration_timeout_ms, decode_orchestration_success_payload,
    decode_orchestration_success_payload_field, decode_orchestration_success_result_items,
    dispatch_remote_orchestration_request, ensure_orchestration_success_response,
    run_orchestration_future_with_outer_deadline,
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
    outer_deadline: Option<TransactionDeadline>,
    execution_id: &'a str,
    command_identity_key: Option<&'a str>,
    rub_home: &'a Path,
    router_fence_disposition: RouterFenceDisposition,
}

pub(crate) async fn execute_orchestration_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    command_identity_key: Option<&str>,
    outer_deadline: Option<TransactionDeadline>,
    router_fence_disposition: RouterFenceDisposition,
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
        outer_deadline,
        execution_id: &execution_id,
        command_identity_key,
        rub_home: &state.rub_home,
        router_fence_disposition,
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
        let step_command_id = orchestration_step_command_id(
            rule,
            context.command_identity_key,
            context.execution_id,
            step_index,
        );
        let target_dispatch_command_id = format!(
            "orchestration_target_dispatch:{}:{step_command_id}",
            target_session.session_id
        );
        record_orchestration_pending_step_timeout_projection_with_recovery(
            rule.id,
            total_steps,
            &steps,
            step_index,
            Some(serde_json::json!({
                "step_index": step_index,
                "target_session_id": target_session.session_id.as_str(),
                "target_session_name": target_session.session_name.as_str(),
                "target_command_id": target_dispatch_command_id,
                "inner_command_id": step_command_id,
                "recovery_authority": "target_session_replay",
                "fallback_authority": "spent_without_cached_response",
            })),
        );
        match dispatch_orchestration_action(context, target_session, step_index, action).await {
            Ok(step) => {
                steps.push(step);
                record_orchestration_partial_commit_timeout_projection(
                    rule.id,
                    total_steps,
                    &steps,
                );
            }
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
        error_context: None,
    }
}

pub(crate) fn orchestration_non_authoritative_evidence_error(
    message: impl Into<String>,
    degraded_reason: Option<String>,
    context: serde_json::Value,
) -> ErrorEnvelope {
    let mut context = context.as_object().cloned().unwrap_or_default();
    context.insert(
        "reason".to_string(),
        serde_json::json!("runtime_observatory_not_authoritative"),
    );
    context.insert(
        "degraded_reason".to_string(),
        degraded_reason.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    ErrorEnvelope::new(ErrorCode::SessionBusy, message)
        .with_context(serde_json::Value::Object(context))
}

#[cfg(test)]
mod tests;
