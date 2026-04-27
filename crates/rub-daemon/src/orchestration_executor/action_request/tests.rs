use super::orchestration_source_materialization_wait_budget_ms;
use super::workflow::dispatch_to_source_session_for_workflow_bindings;
use super::workflow::resolve_orchestration_workflow_source_bindings;
use super::*;
use crate::orchestration_runtime::projected_orchestration_session;
use crate::router::{DaemonRouter, TransactionDeadline};
use crate::session::SessionState;
use rub_core::error::ErrorCode;
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo, TriggerConditionKind,
    TriggerConditionSpec,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

#[test]
fn source_materialization_wait_budget_tracks_declared_workflow_timeout() {
    let home = std::env::temp_dir().join(format!(
        "rub-orchestration-timeout-budget-{}",
        uuid::Uuid::now_v7()
    ));
    let workflows = home.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("delayed_rule.json"),
        serde_json::to_string(&serde_json::json!({
            "steps": [
                {
                    "command": "click",
                    "args": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "ready",
                            "timeout_ms": 12_000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let action = TriggerActionSpec {
        kind: TriggerActionKind::Workflow,
        command: None,
        payload: Some(serde_json::json!({
            "workflow_name": "delayed_rule",
            "source_vars": {
                "greeting": {
                    "kind": "text",
                    "selector": "#hero"
                }
            }
        })),
    };

    let budget = orchestration_source_materialization_wait_budget_ms(&action, &home)
        .expect("static workflow wait budget should project");

    assert_eq!(budget, ORCHESTRATION_ACTION_BASE_TIMEOUT_MS + 12_000);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn source_materialization_wait_budget_rejects_timeout_sensitive_source_var_paths() {
    let home = std::env::temp_dir().join(format!(
        "rub-orchestration-timeout-authority-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(home.join("workflows")).unwrap();

    let action = TriggerActionSpec {
        kind: TriggerActionKind::Workflow,
        command: None,
        payload: Some(serde_json::json!({
            "steps": [
                {
                    "command": "wait",
                    "args": {
                        "timeout_ms": "{{dynamic_timeout}}"
                    }
                }
            ],
            "source_vars": {
                "dynamic_timeout": {
                    "kind": "text",
                    "selector": "#hero"
                }
            }
        })),
    };

    let error = orchestration_source_materialization_wait_budget_ms(&action, &home)
        .expect_err("timeout-sensitive source vars should fail closed");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    let context = error.context.expect("timeout authority context");
    assert_eq!(
        context["reason"],
        "orchestration_source_materialization_timeout_authority_ambiguous"
    );
    assert_eq!(context["path"], "$[0].args.timeout_ms");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn source_materialization_wait_budget_rejects_dynamic_commands() {
    let home = std::env::temp_dir().join(format!(
        "rub-orchestration-timeout-command-authority-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(home.join("workflows")).unwrap();

    let action = TriggerActionSpec {
        kind: TriggerActionKind::Workflow,
        command: None,
        payload: Some(serde_json::json!({
            "steps": [
                {
                    "command": "{{dynamic_command}}",
                    "args": {
                        "timeout_ms": 5_000
                    }
                }
            ],
            "source_vars": {
                "dynamic_command": {
                    "kind": "text",
                    "selector": "#hero"
                }
            }
        })),
    };

    let error = orchestration_source_materialization_wait_budget_ms(&action, &home)
        .expect_err("dynamic commands should fail closed");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    let context = error.context.expect("timeout authority context");
    assert_eq!(
        context["reason"],
        "orchestration_source_materialization_timeout_authority_ambiguous"
    );
    assert_eq!(context["path"], "$[0].command");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn source_materialization_wait_budget_rejects_structured_nested_fill_spec_timeout_authority() {
    let home = std::env::temp_dir().join(format!(
        "rub-orchestration-timeout-structured-fill-spec-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(home.join("workflows")).unwrap();

    let action = TriggerActionSpec {
        kind: TriggerActionKind::Workflow,
        command: None,
        payload: Some(serde_json::json!({
            "steps": [
                {
                    "command": "fill",
                    "args": {
                        "spec": [
                            {
                                "selector": "#email",
                                "value": "alice@example.com",
                                "wait_after": {
                                    "timeout_ms": SOURCE_MATERIALIZATION_TIMEOUT_SENTINEL
                                }
                            }
                        ]
                    }
                }
            ]
        })),
    };

    let error = orchestration_source_materialization_wait_budget_ms(&action, &home)
        .expect_err("structured nested fill spec should fail closed");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    let context = error.context.expect("timeout authority context");
    assert_eq!(
        context["reason"],
        "orchestration_source_materialization_timeout_authority_ambiguous"
    );
    assert_eq!(context["path"], "$[0].args.spec[0].wait_after.timeout_ms");

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn source_var_dispatch_fails_closed_when_outer_deadline_is_exhausted() {
    let session = projected_orchestration_session(
        "sess-source".to_string(),
        "source".to_string(),
        42,
        "/tmp/rub-nonexistent-source.sock".to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        rub_core::model::OrchestrationSessionAvailability::Addressable,
        None,
    );
    let outer_deadline = TransactionDeadline::new(5);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let error = dispatch_to_source_session_for_workflow_bindings(
        &session,
        "tab-target",
        None,
        &serde_json::Map::new(),
        Some(outer_deadline),
    )
    .await
    .expect_err("expired outer deadline should fail closed before remote dispatch");

    assert_eq!(error.code, ErrorCode::IpcTimeout);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("orchestration_source_var_timeout_budget_exhausted")
    );
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

fn local_source_rule() -> OrchestrationRuleInfo {
    OrchestrationRuleInfo {
        id: 7,
        status: OrchestrationRuleStatus::Armed,
        lifecycle_generation: 1,
        source: OrchestrationAddressInfo {
            session_id: "sess-local".to_string(),
            session_name: "default".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("tab-source".to_string()),
            frame_id: None,
        },
        target: OrchestrationAddressInfo {
            session_id: "sess-target".to_string(),
            session_name: "target".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("tab-target".to_string()),
            frame_id: None,
        },
        mode: OrchestrationMode::Once,
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
        correlation_key: "corr-local".to_string(),
        idempotency_key: "idem-local".to_string(),
        unavailable_reason: None,
        last_condition_evidence: None,
        last_result: None,
    }
}

#[tokio::test]
async fn local_source_var_resolution_fails_closed_when_outer_deadline_is_exhausted() {
    let router = test_router();
    let state = Arc::new(SessionState::new_with_id(
        "default",
        "sess-local",
        PathBuf::from("/tmp/rub-local-source-vars-timeout"),
        None,
    ));
    let deadline = TransactionDeadline::new(1);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let error = resolve_orchestration_workflow_source_bindings(
        &router,
        &state,
        &OrchestrationRuntimeInfo::default(),
        &local_source_rule(),
        serde_json::json!({
            "source_vars": {
                "greeting": {
                    "kind": "text",
                    "selector": "#hero"
                }
            }
        })
        .as_object()
        .expect("payload object"),
        Some(deadline),
    )
    .await
    .expect_err("expired outer deadline should fail closed before local source reads");

    assert_eq!(error.code, ErrorCode::IpcTimeout);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("orchestration_source_var_timeout_budget_exhausted")
    );
}
