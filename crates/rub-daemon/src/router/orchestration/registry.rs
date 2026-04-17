use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    OrchestrationExecutionPolicyInfo, OrchestrationRegistrationSpec, OrchestrationRuleInfo,
    OrchestrationRuleStatus,
};
use rub_ipc::protocol::IpcRequest;

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

    let default_correlation_key =
        default_orchestration_registration_key("correlation", &spec, args.paused);
    let default_idempotency_key =
        default_orchestration_registration_key("idempotency", &spec, args.paused);
    let correlation_key = spec.correlation_key.unwrap_or(default_correlation_key);
    let idempotency_key = spec.idempotency_key.unwrap_or(default_idempotency_key);
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

fn default_orchestration_registration_key(
    kind: &str,
    spec: &OrchestrationRegistrationSpec,
    paused: bool,
) -> String {
    let identity = orchestration_registration_request_identity(spec, paused);
    format!("orchestration_{kind}\u{1f}{identity}")
}

fn orchestration_registration_request_identity(
    spec: &OrchestrationRegistrationSpec,
    paused: bool,
) -> String {
    let request = IpcRequest::new(
        "orchestration.add",
        serde_json::json!({
            "paused": paused,
            "spec": spec,
        }),
        0,
    );
    crate::router::transaction::replay_request_fingerprint(&request)
}

#[cfg(test)]
mod tests {
    use super::{
        default_orchestration_registration_key, orchestration_registration_request_identity,
    };
    use crate::session::SessionState;
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationRegistrationSpec,
        OrchestrationRuleInfo, OrchestrationRuleStatus,
    };
    use std::path::PathBuf;

    fn parse_registration_spec(json: &str) -> OrchestrationRegistrationSpec {
        serde_json::from_str(json).expect("registration spec should parse")
    }

    #[test]
    fn omitted_registration_keys_derive_stable_identity_across_object_key_order() {
        let spec_a = parse_registration_spec(
            r#"{
                "source": { "session_id": "sess-source" },
                "target": { "session_id": "sess-target" },
                "condition": { "kind": "text_present", "text": "Ready" },
                "actions": [
                    {
                        "kind": "browser_command",
                        "command": "pipe",
                        "payload": {
                            "timeout_ms": 1000,
                            "steps": [
                                { "command": "state", "args": { "format": "json", "verbose": true } }
                            ]
                        }
                    }
                ]
            }"#,
        );
        let spec_b = parse_registration_spec(
            r#"{
                "source": { "session_id": "sess-source" },
                "target": { "session_id": "sess-target" },
                "actions": [
                    {
                        "payload": {
                            "steps": [
                                { "args": { "verbose": true, "format": "json" }, "command": "state" }
                            ],
                            "timeout_ms": 1000
                        },
                        "command": "pipe",
                        "kind": "browser_command"
                    }
                ],
                "condition": { "text": "Ready", "kind": "text_present" }
            }"#,
        );

        assert_eq!(
            orchestration_registration_request_identity(&spec_a, false),
            orchestration_registration_request_identity(&spec_b, false)
        );
        assert_eq!(
            default_orchestration_registration_key("correlation", &spec_a, false),
            default_orchestration_registration_key("correlation", &spec_b, false)
        );
        assert_eq!(
            default_orchestration_registration_key("idempotency", &spec_a, false),
            default_orchestration_registration_key("idempotency", &spec_b, false)
        );
    }

    fn rule_from_spec(
        spec: &OrchestrationRegistrationSpec,
        correlation_key: String,
        idempotency_key: String,
    ) -> OrchestrationRuleInfo {
        OrchestrationRuleInfo {
            id: 0,
            status: OrchestrationRuleStatus::Armed,
            source: OrchestrationAddressInfo {
                session_id: spec.source.session_id.clone(),
                session_name: "source".to_string(),
                tab_index: spec.source.tab_index,
                tab_target_id: spec.source.tab_target_id.clone(),
                frame_id: spec.source.frame_id.clone(),
            },
            target: OrchestrationAddressInfo {
                session_id: spec.target.session_id.clone(),
                session_name: "target".to_string(),
                tab_index: spec.target.tab_index,
                tab_target_id: spec.target.tab_target_id.clone(),
                frame_id: spec.target.frame_id.clone(),
            },
            mode: spec.mode,
            execution_policy: OrchestrationExecutionPolicyInfo::default(),
            condition: spec.condition.clone(),
            actions: spec.actions.clone(),
            correlation_key,
            idempotency_key,
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        }
    }

    #[tokio::test]
    async fn stable_omitted_registration_identity_prevents_duplicate_live_rule_on_retry() {
        let state = SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-orchestration-add-stable-id"),
            None,
        );
        let spec = parse_registration_spec(
            r##"{
                "source": { "session_id": "sess-source" },
                "target": { "session_id": "sess-target" },
                "condition": { "kind": "text_present", "text": "Ready" },
                "actions": [
                    {
                        "kind": "browser_command",
                        "command": "click",
                        "payload": { "selector": "#continue" }
                    }
                ]
            }"##,
        );
        let correlation_key = default_orchestration_registration_key("correlation", &spec, false);
        let idempotency_key = default_orchestration_registration_key("idempotency", &spec, false);

        let first = state
            .register_orchestration_rule(rule_from_spec(
                &spec,
                correlation_key.clone(),
                idempotency_key.clone(),
            ))
            .await
            .expect("first rule should register");
        let duplicate = state
            .register_orchestration_rule(rule_from_spec(
                &spec,
                correlation_key.clone(),
                idempotency_key.clone(),
            ))
            .await
            .expect_err("stable omitted registration identity should reject duplicate retry");

        assert_eq!(first.id, 1);
        assert_eq!(duplicate, 1);
        let runtime = state.orchestration_runtime().await;
        assert_eq!(runtime.rules.len(), 1);
        assert_eq!(runtime.rules[0].idempotency_key, idempotency_key);
        assert_eq!(runtime.rules[0].correlation_key, correlation_key);
    }
}
