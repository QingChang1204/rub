use super::{
    OrchestrationFailureInput, RemoteDispatchContract, action_requires_source_materialization,
    bind_orchestration_daemon_authority, classify_orchestration_error_status,
    decode_orchestration_success_payload_field, decode_orchestration_success_result_items,
    dispatch_remote_orchestration_request, orchestration_action_execution_info,
    orchestration_failure_result, orchestration_step_command_id,
    resolve_orchestration_workflow_spec,
};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationStepStatus, TabInfo,
    TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
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

fn sample_rule() -> OrchestrationRuleInfo {
    OrchestrationRuleInfo {
        id: 7,
        status: OrchestrationRuleStatus::Armed,
        lifecycle_generation: 1,
        source: OrchestrationAddressInfo {
            session_id: "source".to_string(),
            session_name: "Source".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("source-tab".to_string()),
            frame_id: None,
        },
        target: OrchestrationAddressInfo {
            session_id: "target".to_string(),
            session_name: "Target".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("target-tab".to_string()),
            frame_id: None,
        },
        mode: OrchestrationMode::Repeat,
        execution_policy: OrchestrationExecutionPolicyInfo::default(),
        condition: TriggerConditionSpec {
            kind: TriggerConditionKind::TextPresent,
            locator: None,
            text: Some("ready".to_string()),
            url_pattern: None,
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        },
        actions: Vec::new(),
        correlation_key: "corr-demo".to_string(),
        idempotency_key: "idem-demo".to_string(),
        unavailable_reason: None,
        last_condition_evidence: None,
        last_result: None,
    }
}

#[test]
fn orchestration_step_command_id_is_stable_across_execution_attempts_when_identity_key_present() {
    let rule = sample_rule();
    let first = orchestration_step_command_id(&rule, Some("evidence-key"), "exec-a", 1);
    let second = orchestration_step_command_id(&rule, Some("evidence-key"), "exec-b", 1);

    assert_eq!(first, second);
}

#[test]
fn orchestration_step_command_id_changes_when_evidence_identity_changes() {
    let rule = sample_rule();
    let first = orchestration_step_command_id(&rule, Some("evidence-a"), "exec-a", 1);
    let second = orchestration_step_command_id(&rule, Some("evidence-b"), "exec-a", 1);

    assert_ne!(first, second);
}

#[test]
fn orchestration_step_command_id_falls_back_to_execution_attempt_without_identity_key() {
    let rule = sample_rule();
    let first = orchestration_step_command_id(&rule, None, "exec-a", 1);
    let second = orchestration_step_command_id(&rule, None, "exec-b", 1);

    assert_ne!(first, second);
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
