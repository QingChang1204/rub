use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::{
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
    OrchestrationSessionInfo, TriggerConditionKind, TriggerEvidenceInfo,
};

use crate::orchestration_executor::execute_orchestration_rule;
use crate::orchestration_probe::{
    dispatch_remote_orchestration_probe, evaluate_orchestration_probe_for_tab,
};
use crate::orchestration_worker::orchestration_evidence_key;
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::session::SessionState;

use super::DaemonRouter;
use super::projection::{orchestration_payload, orchestration_rule_subject};
use super::rule::{
    blocked_cooldown_result, orchestration_rule_in_cooldown, orchestration_status_name,
};

pub(super) async fn cmd_orchestration_execute(
    router: &DaemonRouter,
    id: u32,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    ensure_orchestration_execution_available(&runtime)?;
    let rule = state.orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
    if !matches!(rule.status, OrchestrationRuleStatus::Armed) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Orchestration rule id {id} must be 'armed' before execute; current status is '{}'",
                orchestration_status_name(rule.status),
            ),
        ));
    }
    if rule.unavailable_reason.is_some() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is currently unavailable"),
            serde_json::json!({
                "reason": "orchestration_rule_unavailable",
                "unavailable_reason": rule.unavailable_reason,
            }),
        ));
    }
    let execution_evidence =
        capture_manual_orchestration_condition_evidence(router, state, &runtime, &rule).await?;
    if orchestration_rule_in_cooldown(&rule) {
        let result = blocked_cooldown_result(&rule);
        let rule = state
            .record_orchestration_outcome(
                id,
                execution_evidence
                    .clone()
                    .or_else(|| rule.last_condition_evidence.clone()),
                result.clone(),
            )
            .await
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Orchestration rule id {id} disappeared while recording cooldown outcome"
                ))
            })?;
        refresh_orchestration_runtime(state).await;
        let runtime = state.orchestration_runtime().await;
        let rule = runtime
            .rules
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .unwrap_or(rule);

        return Ok(orchestration_payload(
            orchestration_rule_subject(id),
            serde_json::json!({
                "rule": rule,
                "execution": result,
            }),
            &runtime,
        ));
    }

    let manual_command_identity_key = execution_evidence.as_ref().map(orchestration_evidence_key);
    let runtime = state.orchestration_runtime().await;
    let result = execute_orchestration_rule(
        router,
        state,
        &runtime,
        &rule,
        manual_command_identity_key.as_deref(),
    )
    .await;
    let rule = state
        .record_orchestration_outcome(id, execution_evidence, result.clone())
        .await
        .ok_or_else(|| {
            RubError::Internal(format!(
                "Orchestration rule id {id} disappeared while recording execution outcome"
            ))
        })?;
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "rule": rule,
            "execution": result,
        }),
        &runtime,
    ))
}

async fn capture_manual_orchestration_condition_evidence(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> Result<Option<TriggerEvidenceInfo>, RubError> {
    if matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest) {
        return Ok(None);
    }
    let tab_target_id = rule.source.tab_target_id.as_deref().ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Orchestration source address is missing source.tab_target_id",
            serde_json::json!({
                "reason": "orchestration_source_tab_target_missing",
                "source_session_id": rule.source.session_id,
                "source_session_name": rule.source.session_name,
            }),
        )
    })?;
    let probe = if rule.source.session_id == state.session_id {
        evaluate_orchestration_probe_for_tab(
            &router.browser_port(),
            state,
            tab_target_id,
            rule.source.frame_id.as_deref(),
            &rule.condition,
            u64::MAX,
            0,
        )
        .await?
    } else {
        let source_session =
            resolve_orchestration_source_session(runtime, rule).map_err(RubError::Domain)?;
        dispatch_remote_orchestration_probe(
            source_session,
            tab_target_id,
            rule.source.frame_id.as_deref(),
            &rule.condition,
            u64::MAX,
            0,
        )
        .await
        .map_err(RubError::Domain)?
    };
    if let Some(reason) = probe.degraded_reason {
        return Err(RubError::domain_with_context(
            ErrorCode::BrowserCrashed,
            "Manual orchestration execute cannot derive authoritative condition evidence because probe evidence was dropped",
            serde_json::json!({
                "reason": "runtime_observatory_not_authoritative",
                "degraded_reason": reason,
            }),
        ));
    }
    Ok(if probe.matched { probe.evidence } else { None })
}

fn resolve_orchestration_source_session<'a>(
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> Result<&'a OrchestrationSessionInfo, ErrorEnvelope> {
    runtime
        .known_sessions
        .iter()
        .find(|session| session.session_id == rule.source.session_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Source session '{}' is not available for orchestration condition evaluation",
                    rule.source.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_session_missing",
                "source_session_id": rule.source.session_id,
                "source_session_name": rule.source.session_name,
            }))
        })
}

pub(super) fn ensure_orchestration_addressing_available(
    runtime: &OrchestrationRuntimeInfo,
) -> Result<(), RubError> {
    if runtime.addressing_supported {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "Orchestration session addressing is temporarily unavailable because the live registry authority is degraded",
        serde_json::json!({
            "reason": "orchestration_registry_degraded",
            "degraded_reason": runtime.degraded_reason,
            "addressing_supported": runtime.addressing_supported,
            "execution_supported": runtime.execution_supported,
        }),
    ))
}

pub(super) fn ensure_orchestration_execution_available(
    runtime: &OrchestrationRuntimeInfo,
) -> Result<(), RubError> {
    if runtime.execution_supported {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "Orchestration execution is temporarily unavailable because the live registry authority is degraded",
        serde_json::json!({
            "reason": "orchestration_registry_degraded",
            "degraded_reason": runtime.degraded_reason,
            "addressing_supported": runtime.addressing_supported,
            "execution_supported": runtime.execution_supported,
        }),
    ))
}

pub(super) async fn update_orchestration_status(
    id: u32,
    state: &Arc<SessionState>,
    next_status: OrchestrationRuleStatus,
) -> Result<serde_json::Value, RubError> {
    let current = state
        .orchestration_rules()
        .await
        .into_iter()
        .find(|rule| rule.id == id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Orchestration rule id {id} is not present in the current registry"),
            )
        })?;

    let rule = match (current.status, next_status) {
        (OrchestrationRuleStatus::Armed, OrchestrationRuleStatus::Paused)
        | (OrchestrationRuleStatus::Paused, OrchestrationRuleStatus::Armed) => state
            .set_orchestration_rule_status(id, next_status)
            .await
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Orchestration rule id {id} disappeared while applying status update"
                ))
            })?,
        (status, requested) if status == requested => current,
        _ => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Orchestration rule id {id} cannot transition from '{}' to '{}'",
                    orchestration_status_name(current.status),
                    orchestration_status_name(next_status),
                ),
            ));
        }
    };

    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "rule": rule,
        }),
        &runtime,
    ))
}
