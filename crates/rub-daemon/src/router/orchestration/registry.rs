use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationRegistrationSpec,
    OrchestrationRuleInfo, OrchestrationRuleStatus,
};
use rub_ipc::protocol::IpcRequest;

use crate::router::RouterFenceDisposition;
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::scheduler_policy::AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL;
use crate::session::SessionState;

use super::DaemonRouter;
use super::addressing::resolve_orchestration_address;
use super::command::{OrchestrationAddArgs, OrchestrationIdArgs, OrchestrationTraceArgs};
use super::execution::{
    capture_orchestration_network_request_baseline, ensure_orchestration_addressing_available,
};
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
    deadline: crate::router::TransactionDeadline,
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
        Some(deadline),
    )
    .await?;
    let target = resolve_orchestration_address(
        router,
        state,
        &runtime.known_sessions,
        &spec.target,
        "target",
        Some(deadline),
    )
    .await?;

    let default_correlation_key =
        default_orchestration_registration_key("correlation", &source, &target, &spec, args.paused);
    let default_idempotency_key =
        default_orchestration_registration_key("idempotency", &source, &target, &spec, args.paused);
    let correlation_key = spec.correlation_key.unwrap_or(default_correlation_key);
    let idempotency_key = spec.idempotency_key.unwrap_or(default_idempotency_key);
    let network_baseline = if !args.paused
        && matches!(
            spec.condition.kind,
            rub_core::model::TriggerConditionKind::NetworkRequest
        ) {
        Some(
            capture_orchestration_network_request_baseline(
                &runtime,
                state,
                &source,
                &spec.condition,
                deadline,
            )
            .await?,
        )
    } else {
        None
    };
    let rule = state
        .register_orchestration_rule_with_network_baseline(OrchestrationRuleInfo {
            id: 0,
            status: if args.paused {
                OrchestrationRuleStatus::Paused
            } else {
                OrchestrationRuleStatus::Armed
            },
            lifecycle_generation: 1,
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
        }, network_baseline)
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
    router: &DaemonRouter,
    args: OrchestrationIdArgs,
    deadline: crate::router::TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_orchestration_remove_with_router_fence_disposition(
        router,
        args,
        deadline,
        state,
        RouterFenceDisposition::ReuseCurrentTransaction,
    )
    .await
}

async fn cmd_orchestration_remove_with_router_fence_disposition(
    router: &DaemonRouter,
    args: OrchestrationIdArgs,
    deadline: crate::router::TransactionDeadline,
    state: &Arc<SessionState>,
    router_fence_disposition: RouterFenceDisposition,
) -> Result<serde_json::Value, RubError> {
    let id = args.id;
    let queue_wait_budget = std::time::Duration::from_millis(deadline.remaining_ms());
    let _active_execution_fence = router
        .begin_automation_transaction_if_needed(
            state,
            "orchestration_rule_remove",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
            router_fence_disposition,
        )
        .await
        .map_err(RubError::Domain)?;
    let removed = state.remove_orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
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
    source: &OrchestrationAddressInfo,
    target: &OrchestrationAddressInfo,
    spec: &OrchestrationRegistrationSpec,
    paused: bool,
) -> String {
    let identity = orchestration_registration_request_identity(source, target, spec, paused);
    format!("orchestration_{kind}\u{1f}{identity}")
}

fn orchestration_registration_request_identity(
    source: &OrchestrationAddressInfo,
    target: &OrchestrationAddressInfo,
    spec: &OrchestrationRegistrationSpec,
    paused: bool,
) -> String {
    let request = IpcRequest::new(
        "orchestration.add",
        serde_json::json!({
            "paused": paused,
            "resolved": {
                "source": source,
                "target": target,
            },
            "spec": {
                "mode": spec.mode,
                "execution_policy": spec.execution_policy,
                "condition": spec.condition,
                "actions": spec.actions,
            },
        }),
        0,
    );
    crate::router::transaction::replay_request_fingerprint(&request)
}

#[cfg(test)]
mod tests {
    use super::{
        cmd_orchestration_remove_with_router_fence_disposition,
        default_orchestration_registration_key, orchestration_registration_request_identity,
    };
    use crate::router::{DaemonRouter, RouterFenceDisposition, TransactionDeadline};
    use crate::session::SessionState;
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationRegistrationSpec,
        OrchestrationRuleInfo, OrchestrationRuleStatus,
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn parse_registration_spec(json: &str) -> OrchestrationRegistrationSpec {
        serde_json::from_str(json).expect("registration spec should parse")
    }

    fn test_router() -> DaemonRouter {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                managed_profile_ephemeral: false,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        DaemonRouter::new(adapter)
    }

    fn resolved_address(
        session_id: &str,
        session_name: &str,
        tab_index: u32,
        tab_target_id: &str,
    ) -> OrchestrationAddressInfo {
        OrchestrationAddressInfo {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            tab_index: Some(tab_index),
            tab_target_id: Some(tab_target_id.to_string()),
            frame_id: None,
        }
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

        let source = resolved_address("sess-source", "source", 0, "SOURCE_TAB");
        let target = resolved_address("sess-target", "target", 1, "TARGET_TAB");
        assert_eq!(
            orchestration_registration_request_identity(&source, &target, &spec_a, false),
            orchestration_registration_request_identity(&source, &target, &spec_b, false)
        );
        assert_eq!(
            default_orchestration_registration_key("correlation", &source, &target, &spec_a, false),
            default_orchestration_registration_key("correlation", &source, &target, &spec_b, false)
        );
        assert_eq!(
            default_orchestration_registration_key("idempotency", &source, &target, &spec_a, false),
            default_orchestration_registration_key("idempotency", &source, &target, &spec_b, false)
        );
    }

    #[test]
    fn omitted_registration_keys_absorb_resolved_tab_authority() {
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
        let source_a = resolved_address("sess-source", "source", 0, "SOURCE_TAB_A");
        let source_b = resolved_address("sess-source", "source", 1, "SOURCE_TAB_B");
        let target = resolved_address("sess-target", "target", 4, "TARGET_TAB");

        assert_ne!(
            default_orchestration_registration_key("correlation", &source_a, &target, &spec, false),
            default_orchestration_registration_key("correlation", &source_b, &target, &spec, false)
        );
        assert_ne!(
            default_orchestration_registration_key("idempotency", &source_a, &target, &spec, false),
            default_orchestration_registration_key("idempotency", &source_b, &target, &spec, false)
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
            lifecycle_generation: 1,
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
        let source = resolved_address("sess-source", "source", 0, "SOURCE_TAB");
        let target = resolved_address("sess-target", "target", 1, "TARGET_TAB");
        let correlation_key =
            default_orchestration_registration_key("correlation", &source, &target, &spec, false);
        let idempotency_key =
            default_orchestration_registration_key("idempotency", &source, &target, &spec, false);

        let mut resolved_spec = spec.clone();
        resolved_spec.source.tab_index = source.tab_index;
        resolved_spec.source.tab_target_id = source.tab_target_id.clone();
        resolved_spec.target.tab_index = target.tab_index;
        resolved_spec.target.tab_target_id = target.tab_target_id.clone();

        let first = state
            .register_orchestration_rule(rule_from_spec(
                &resolved_spec,
                correlation_key.clone(),
                idempotency_key.clone(),
            ))
            .await
            .expect("first rule should register");
        let duplicate = state
            .register_orchestration_rule(rule_from_spec(
                &resolved_spec,
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

    #[tokio::test]
    async fn orchestration_remove_waits_for_active_execution_fence() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-orchestration-remove-fence"),
            None,
        ));
        let rule = state
            .register_orchestration_rule(OrchestrationRuleInfo {
                id: 0,
                status: OrchestrationRuleStatus::Armed,
                lifecycle_generation: 1,
                source: resolved_address("sess-source", "source", 0, "SOURCE_TAB"),
                target: resolved_address("sess-target", "target", 1, "TARGET_TAB"),
                mode: rub_core::model::OrchestrationMode::Once,
                execution_policy: OrchestrationExecutionPolicyInfo::default(),
                condition: rub_core::model::TriggerConditionSpec {
                    kind: rub_core::model::TriggerConditionKind::TextPresent,
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
                actions: Vec::new(),
                correlation_key: "corr".to_string(),
                idempotency_key: "idem".to_string(),
                unavailable_reason: None,
                last_condition_evidence: None,
                last_result: None,
            })
            .await
            .expect("rule should register");
        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_rule_remove_fence",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("held transaction should acquire");

        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));
        let error = cmd_orchestration_remove_with_router_fence_disposition(
            &router,
            super::OrchestrationIdArgs {
                _sub: "remove".to_string(),
                id: rule.id,
            },
            deadline,
            &state,
            RouterFenceDisposition::Acquire,
        )
        .await
        .expect_err("remove must fail closed while active execution fence is held");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, rub_core::error::ErrorCode::IpcTimeout);
        assert!(state.orchestration_rule(rule.id).await.is_some());
        drop(held);
    }

    #[tokio::test]
    async fn orchestration_remove_reuses_outer_router_transaction_without_queue_reentry() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-orchestration-remove-reuse-fence"),
            None,
        ));
        let rule = state
            .register_orchestration_rule(OrchestrationRuleInfo {
                id: 0,
                status: OrchestrationRuleStatus::Armed,
                lifecycle_generation: 1,
                source: resolved_address("sess-source", "source", 0, "SOURCE_TAB"),
                target: resolved_address("sess-target", "target", 1, "TARGET_TAB"),
                mode: rub_core::model::OrchestrationMode::Once,
                execution_policy: OrchestrationExecutionPolicyInfo::default(),
                condition: rub_core::model::TriggerConditionSpec {
                    kind: rub_core::model::TriggerConditionKind::TextPresent,
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
                actions: Vec::new(),
                correlation_key: "corr".to_string(),
                idempotency_key: "idem".to_string(),
                unavailable_reason: None,
                last_condition_evidence: None,
                last_result: None,
            })
            .await
            .expect("rule should register");
        let _outer_transaction = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "held_rule_remove_outer_transaction",
                Duration::from_secs(1),
                Duration::from_millis(5),
            )
            .await
            .expect("outer transaction should acquire");

        let payload = cmd_orchestration_remove_with_router_fence_disposition(
            &router,
            super::OrchestrationIdArgs {
                _sub: "remove".to_string(),
                id: rule.id,
            },
            TransactionDeadline::new(500),
            &state,
            RouterFenceDisposition::ReuseCurrentTransaction,
        )
        .await
        .expect("remove should reuse the current router transaction instead of queue reentry timing out");

        assert_eq!(
            payload["result"]["removed"]["id"],
            serde_json::json!(rule.id)
        );
        assert!(state.orchestration_rule(rule.id).await.is_none());
    }
}
