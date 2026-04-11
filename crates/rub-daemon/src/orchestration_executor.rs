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
    rub_home: &'a Path,
}

pub(crate) async fn execute_orchestration_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
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
mod tests {
    use super::{
        OrchestrationFailureInput, RemoteDispatchContract, action_requires_source_materialization,
        bind_orchestration_daemon_authority, classify_orchestration_error_status,
        decode_orchestration_success_payload_field, decode_orchestration_success_result_items,
        dispatch_remote_orchestration_request, orchestration_action_execution_info,
        orchestration_failure_result, resolve_orchestration_workflow_spec,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::{
        OrchestrationRuleStatus, OrchestrationStepStatus, TabInfo, TriggerActionKind,
        TriggerActionSpec,
    };
    use rub_ipc::protocol::{IpcRequest, IpcResponse};

    #[test]
    fn classify_orchestration_error_status_preserves_blocked_vs_degraded_boundary() {
        assert_eq!(
            classify_orchestration_error_status(ErrorCode::ElementNotFound),
            OrchestrationRuleStatus::Blocked
        );
        assert_eq!(
            classify_orchestration_error_status(ErrorCode::DaemonNotRunning),
            OrchestrationRuleStatus::Degraded
        );
    }

    #[test]
    fn orchestration_failure_result_blocks_rearm_after_partial_commit() {
        let result = orchestration_failure_result(OrchestrationFailureInput {
            rule_id: 7,
            retained_status: OrchestrationRuleStatus::Armed,
            total_steps: 3,
            failed_step_index: 1,
            committed_steps: vec![rub_core::model::OrchestrationStepResultInfo {
                step_index: 0,
                status: OrchestrationStepStatus::Committed,
                summary: "step 1 committed".to_string(),
                attempts: 1,
                action: None,
                result: None,
                error_code: None,
                reason: None,
            }],
            failed_action: None,
            failed_attempts: 1,
            error: ErrorEnvelope::new(ErrorCode::ElementNotFound, "missing element"),
        });
        assert_eq!(result.status, OrchestrationRuleStatus::Blocked);
        assert_eq!(result.next_status, OrchestrationRuleStatus::Blocked);
        assert_eq!(result.committed_steps, 1);
        assert_eq!(result.total_steps, 3);
        assert_eq!(result.steps.len(), 2);
        assert_eq!(result.steps[1].status, OrchestrationStepStatus::Blocked);
        assert_eq!(result.steps[1].attempts, 1);
    }

    #[test]
    fn orchestration_remote_request_binds_remote_daemon_authority() {
        let session = crate::orchestration_runtime::projected_orchestration_session(
            "daemon-b".to_string(),
            "remote".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            None,
        );

        let request = bind_orchestration_daemon_authority(
            IpcRequest::new("tabs", serde_json::json!({}), 1_000),
            &session,
            "target",
        )
        .expect("binding should succeed");

        assert_eq!(request.daemon_session_id.as_deref(), Some("daemon-b"));
    }

    #[test]
    fn decode_orchestration_success_result_items_reads_wrapped_result_items() {
        let session = crate::orchestration_runtime::projected_orchestration_session(
            "daemon-b".to_string(),
            "remote".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            None,
        );
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "subject": { "kind": "tab_registry" },
                "result": {
                    "items": [{
                        "index": 0,
                        "target_id": "tab-a",
                        "url": "https://example.test",
                        "title": "Example",
                        "active": true
                    }]
                }
            }),
        );

        let tabs = decode_orchestration_success_result_items::<TabInfo>(
            response,
            &session,
            "orchestration_target_tabs_payload_missing",
            "missing tabs payload",
            "orchestration_target_tabs_payload_invalid",
            "orchestration target tabs payload",
        )
        .expect("wrapped result.items payload should decode");

        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].target_id, "tab-a");
        assert!(tabs[0].active);
    }

    #[test]
    fn decode_orchestration_success_result_items_fails_closed_when_result_items_missing() {
        let session = crate::orchestration_runtime::projected_orchestration_session(
            "daemon-b".to_string(),
            "remote".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            None,
        );
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "subject": { "kind": "tab_registry" },
                "tabs": []
            }),
        );

        let error = decode_orchestration_success_result_items::<TabInfo>(
            response,
            &session,
            "orchestration_target_tabs_payload_missing",
            "missing tabs payload",
            "orchestration_target_tabs_payload_invalid",
            "orchestration target tabs payload",
        )
        .expect_err("missing result.items should fail closed");

        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_target_tabs_payload_missing")
        );
    }

    #[test]
    fn decode_orchestration_success_payload_field_reads_named_runtime_payload() {
        let session = crate::orchestration_runtime::projected_orchestration_session(
            "daemon-b".to_string(),
            "remote".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            None,
        );
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "subject": { "kind": "runtime_surface", "surface": "summary" },
                "runtime_projection_state": { "projection_kind": "live_runtime_projection" },
                "runtime": {
                    "integration_runtime": { "status": "active" }
                }
            }),
        );

        let runtime = decode_orchestration_success_payload_field::<serde_json::Value>(
            response,
            &session,
            "runtime",
            "orchestration_target_runtime_summary_payload_missing",
            "missing runtime payload",
            "orchestration_target_runtime_summary_payload_invalid",
            "orchestration target runtime summary",
        )
        .expect("named runtime payload should decode");

        assert_eq!(runtime["integration_runtime"]["status"], "active");
    }

    #[tokio::test]
    async fn remote_dispatch_unreachable_context_keeps_socket_path_state() {
        let session = crate::orchestration_runtime::projected_orchestration_session(
            "daemon-b".to_string(),
            "remote".to_string(),
            42,
            format!(
                "/tmp/rub-missing-{}-{}.sock",
                std::process::id(),
                "dispatch"
            ),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            Some("/tmp/rub-profile".to_string()),
        );

        let error = dispatch_remote_orchestration_request(
            &session,
            "target",
            IpcRequest::new("tabs", serde_json::json!({}), 1_000),
            RemoteDispatchContract {
                dispatch_subject: "target command",
                unreachable_reason: "orchestration_target_session_unreachable",
                transport_failure_reason: "orchestration_target_dispatch_transport_failed",
                protocol_failure_reason: "orchestration_target_dispatch_protocol_failed",
                missing_error_message: "remote dispatch returned an error without an envelope",
            },
        )
        .await
        .expect_err("missing socket should fail closed");

        assert_eq!(error.code, ErrorCode::DaemonNotRunning);
        assert_eq!(
            error.context.as_ref().unwrap()["socket_path_state"]["path_authority"],
            "session.orchestration_runtime.known_sessions.socket_path"
        );
        assert_eq!(
            error.context.as_ref().unwrap()["user_data_dir_state"]["path_kind"],
            "managed_user_data_directory"
        );
    }

    #[test]
    fn resolve_orchestration_workflow_spec_marks_named_workflow_asset_path() {
        let home = std::env::temp_dir().join(format!(
            "rub-orchestration-workflow-spec-{}",
            std::process::id()
        ));
        let workflows = home.join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        let workflow_path = workflows.join("reply_flow.json");
        std::fs::write(&workflow_path, r#"[{"command":"doctor","args":{}}]"#).unwrap();

        let (_, spec_source) = resolve_orchestration_workflow_spec(
            &serde_json::json!({
                "workflow_name": "reply_flow",
            })
            .as_object()
            .unwrap()
            .clone(),
            &home,
        )
        .expect("named workflow should resolve");

        assert_eq!(spec_source["path"], workflow_path.display().to_string());
        assert_eq!(
            spec_source["path_state"]["path_authority"],
            "orchestration.workflow.spec_source.path"
        );
        assert_eq!(
            spec_source["path_state"]["upstream_truth"],
            "orchestration_workflow_payload.workflow_name"
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn orchestration_action_execution_info_marks_workflow_asset_path() {
        let home = std::env::temp_dir().join(format!(
            "rub-orchestration-action-info-{}",
            std::process::id()
        ));
        let workflows = home.join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("reply_flow.json"),
            r#"[{"command":"doctor","args":{}}]"#,
        )
        .unwrap();

        let info = orchestration_action_execution_info(
            &TriggerActionSpec {
                kind: TriggerActionKind::Workflow,
                command: None,
                payload: Some(serde_json::json!({
                    "workflow_name": "reply_flow",
                })),
            },
            &home,
        )
        .expect("action info should resolve named workflow");

        assert_eq!(info.workflow_name.as_deref(), Some("reply_flow"));
        assert_eq!(
            info.workflow_path_state
                .as_ref()
                .map(|state| state.path_authority.as_str()),
            Some("automation.action.workflow_path")
        );
        assert_eq!(
            info.workflow_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("orchestration_action_payload.workflow_name")
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn workflow_source_vars_require_source_materialization_but_plain_actions_do_not() {
        assert!(action_requires_source_materialization(&TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(serde_json::json!({
                "source_vars": {
                    "greeting": {
                        "kind": "text",
                        "selector": "#hero"
                    }
                }
            })),
        }));
        assert!(!action_requires_source_materialization(
            &TriggerActionSpec {
                kind: TriggerActionKind::Workflow,
                command: None,
                payload: Some(serde_json::json!({
                    "vars": {
                        "name": "rub"
                    }
                })),
            }
        ));
        assert!(!action_requires_source_materialization(
            &TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(serde_json::json!({ "selector": "#submit" })),
            }
        ));
    }
}
