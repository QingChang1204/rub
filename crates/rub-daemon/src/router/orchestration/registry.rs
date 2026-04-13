use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    OrchestrationExecutionPolicyInfo, OrchestrationRegistrationSpec, OrchestrationRuleInfo,
    OrchestrationRuleStatus,
};
use uuid::Uuid;

use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::session::SessionState;

use super::DaemonRouter;
use super::addressing::resolve_orchestration_address;
use super::command::{OrchestrationAddArgs, OrchestrationIdArgs, OrchestrationTraceArgs};
use super::execution::ensure_orchestration_addressing_available;
use super::projection::{
    orchestration_payload, orchestration_registry_subject, orchestration_rule_identity_projection,
    orchestration_rule_subject,
};
use super::rule::{
    orchestration_rule_to_registration_spec, validate_orchestration_registration_spec,
};
use crate::router::request_args::parse_json_spec_value;

pub(super) async fn cmd_orchestration_add(
    router: &DaemonRouter,
    args: OrchestrationAddArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let mut spec = parse_json_spec_value::<OrchestrationRegistrationSpec>(
        args.spec.into_value(),
        "orchestration add",
    )?;
    validate_orchestration_registration_spec(&mut spec)?;

    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    ensure_orchestration_addressing_available(&runtime)?;
    let source = resolve_orchestration_address(
        router,
        state,
        &runtime.known_sessions,
        &spec.source,
        "source",
    )
    .await?;
    let target = resolve_orchestration_address(
        router,
        state,
        &runtime.known_sessions,
        &spec.target,
        "target",
    )
    .await?;

    let correlation_key = spec
        .correlation_key
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let idempotency_key = spec
        .idempotency_key
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let rule = state
        .register_orchestration_rule(OrchestrationRuleInfo {
            id: 0,
            status: if args.paused {
                OrchestrationRuleStatus::Paused
            } else {
                OrchestrationRuleStatus::Armed
            },
            source,
            target,
            mode: spec.mode,
            execution_policy: OrchestrationExecutionPolicyInfo {
                cooldown_ms: spec.execution_policy.cooldown_ms,
                max_retries: spec.execution_policy.max_retries,
                cooldown_until_ms: None,
            },
            condition: spec.condition,
            actions: spec.actions,
            correlation_key: correlation_key.clone(),
            idempotency_key: idempotency_key.clone(),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        })
        .await
        .map_err(|existing_rule_id| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Orchestration idempotency_key '{idempotency_key}' is already registered on rule {existing_rule_id}"
                ),
                serde_json::json!({
                    "reason": "duplicate_idempotency_key",
                    "idempotency_key": idempotency_key,
                    "existing_rule_id": existing_rule_id,
                    "correlation_key": correlation_key,
                }),
            )
        })?;
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == rule.id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(rule.id),
        serde_json::json!({
            "rule": rule,
            "spec_source": args.spec_source.unwrap_or_else(|| serde_json::json!({ "kind": "inline" })),
        }),
        &runtime,
    ))
}

pub(super) async fn cmd_orchestration_list(
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    Ok(orchestration_payload(
        orchestration_registry_subject(),
        serde_json::json!({
            "items": runtime.rules.clone(),
        }),
        &runtime,
    ))
}

pub(super) async fn cmd_orchestration_trace(
    args: OrchestrationTraceArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let last = usize::try_from(args.last).unwrap_or(usize::MAX);
    let runtime = state.orchestration_runtime().await;
    let trace = state.orchestration_trace(last).await;
    Ok(orchestration_payload(
        serde_json::json!({
            "kind": "orchestration_trace",
            "last": last,
        }),
        serde_json::to_value(trace).map_err(RubError::from)?,
        &runtime,
    ))
}

pub(super) async fn cmd_orchestration_remove(
    args: OrchestrationIdArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let id = args.id;
    let removed = state.remove_orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "removed": removed,
        }),
        &runtime,
    ))
}

pub(super) async fn cmd_orchestration_export(
    id: u32,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::ElementNotFound,
                format!("Orchestration rule {id} not found"),
                serde_json::json!({
                "reason": "orchestration_rule_not_found",
                "id": id,
                }),
            )
        })?;
    let spec = orchestration_rule_to_registration_spec(&rule);
    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "format": "orchestration",
            "rule": rule,
            "spec": spec,
            "rule_identity_projection": orchestration_rule_identity_projection(&rule),
        }),
        &runtime,
    ))
}
