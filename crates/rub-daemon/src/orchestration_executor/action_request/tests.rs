use super::orchestration_source_materialization_wait_budget_ms;
use super::*;
use rub_core::error::ErrorCode;

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
