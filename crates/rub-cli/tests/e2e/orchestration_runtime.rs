use super::*;

/// T437m: orchestration rules should form a distinct cross-session registry surface with lifecycle trace.
#[test]
#[ignore]
#[serial]
fn t437m_orchestration_registry_tracks_cross_session_rules() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-rule.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "workflow",
                    "payload": {
                        "workflow_name": "reply_flow"
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    assert_eq!(
        added["data"]["result"]["rule"]["source"]["session_name"], "source",
        "{added}"
    );
    assert!(
        added["data"]["result"]["rule"]["source"]["tab_target_id"].is_string(),
        "{added}"
    );
    assert_eq!(
        added["data"]["result"]["rule"]["target"]["session_name"], "target",
        "{added}"
    );
    assert!(
        added["data"]["result"]["rule"]["target"]["tab_target_id"].is_string(),
        "{added}"
    );
    assert_eq!(
        added["data"]["runtime"]["addressing_supported"], true,
        "{added}"
    );
    assert_eq!(
        added["data"]["runtime"]["execution_supported"], true,
        "{added}"
    );
    assert_eq!(added["data"]["runtime"]["active_rule_count"], 1, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("orchestration rule id should be present");

    let listed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");
    assert!(
        listed["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["id"].as_u64() == Some(rule_id)),
        "{listed}"
    );

    let trace = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "trace",
                "--last",
                "5",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    assert!(
        trace["data"]["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "registered"
                    && event["rule_id"].as_u64() == Some(rule_id)
                    && event["correlation_key"].is_string()
                    && event["idempotency_key"].is_string()
            }),
        "{trace}"
    );

    cleanup(&home);
}

/// T437n: orchestration registry should project correlation groups and reject duplicate idempotency keys.
#[test]
#[ignore]
#[serial]
fn t437n_orchestration_groups_rules_and_rejects_duplicate_idempotency_keys() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let first_spec_path = format!("{home}/orchestration-rule-a.json");
    std::fs::write(
        &first_spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "workflow",
                    "payload": {
                        "workflow_name": "reply_flow"
                    }
                }
            ],
            "correlation_key": "corr-batch-a",
            "idempotency_key": "idem-batch-a"
        }))
        .unwrap(),
    )
    .unwrap();

    let second_spec_path = format!("{home}/orchestration-rule-b.json");
    std::fs::write(
        &second_spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "readiness",
                "readiness_state": "ready"
            },
            "actions": [
                {
                    "kind": "workflow",
                    "payload": {
                        "workflow_name": "reply_flow"
                    }
                }
            ],
            "correlation_key": "corr-batch-a",
            "idempotency_key": "idem-batch-b"
        }))
        .unwrap(),
    )
    .unwrap();

    let duplicate_spec_path = format!("{home}/orchestration-rule-dup.json");
    std::fs::write(
        &duplicate_spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "workflow",
                    "payload": {
                        "workflow_name": "reply_flow"
                    }
                }
            ],
            "correlation_key": "corr-batch-b",
            "idempotency_key": "idem-batch-a"
        }))
        .unwrap(),
    )
    .unwrap();

    let first = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &first_spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(first["success"], true, "{first}");

    let second = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &second_spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(second["success"], true, "{second}");
    assert_eq!(second["data"]["runtime"]["group_count"], 1, "{second}");
    assert_eq!(
        second["data"]["runtime"]["groups"][0]["correlation_key"], "corr-batch-a",
        "{second}"
    );
    assert_eq!(
        second["data"]["runtime"]["groups"][0]["rule_ids"]
            .as_array()
            .map(|ids| ids.len()),
        Some(2),
        "{second}"
    );
    assert_eq!(
        second["data"]["runtime"]["groups"][0]["active_rule_count"], 2,
        "{second}"
    );

    let duplicate = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &duplicate_spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(duplicate["success"], false, "{duplicate}");
    assert_eq!(duplicate["error"]["code"], "INVALID_INPUT", "{duplicate}");
    assert_eq!(
        duplicate["error"]["context"]["reason"], "duplicate_idempotency_key",
        "{duplicate}"
    );
    assert_eq!(
        duplicate["error"]["context"]["idempotency_key"], "idem-batch-a",
        "{duplicate}"
    );

    let listed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");
    assert_eq!(listed["data"]["runtime"]["group_count"], 1, "{listed}");
    assert_eq!(
        listed["data"]["groups"][0]["rule_ids"]
            .as_array()
            .map(|ids| ids.len()),
        Some(2),
        "{listed}"
    );
    assert_eq!(
        listed["data"]["result"]["items"]
            .as_array()
            .map(|rules| rules.len()),
        Some(2),
        "{listed}"
    );

    cleanup(&home);
}

/// T437o: orchestration execute should commit a multi-action browser-command pipeline on the target session.
#[test]
#[ignore]
#[serial]
fn t437o_orchestration_execute_commits_multi_action_pipeline_across_sessions() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-execute",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Execute Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-execute",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Execute Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-execute");
    let source_url = server.url_for("/orchestration-source-execute");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-execute.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "type",
                    "payload": {
                        "selector": "#name",
                        "text": "Ada orchestration",
                        "clear": true
                    }
                },
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "Ada orchestration",
                            "timeout_ms": 5000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("orchestration rule id should be present")
        .to_string();

    let executed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(executed["data"]["result"]["status"], "fired", "{executed}");
    assert_eq!(
        executed["data"]["result"]["committed_steps"], 2,
        "{executed}"
    );
    assert_eq!(executed["data"]["result"]["total_steps"], 2, "{executed}");
    assert_eq!(
        executed["data"]["result"]["steps"][0]["action"]["command"], "type",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["steps"][0]["result"]["interaction"]["semantic_class"],
        "set_value",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["rule"]["status"], "fired",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["runtime"]["last_rule_result"]["status"], "fired",
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Ada orchestration",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437p: orchestration execute should stop on the first failing step and classify partial failure truthfully.
#[test]
#[ignore]
#[serial]
fn t437p_orchestration_execute_reports_partial_blocked_failure() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-blocked",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Blocked Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-blocked",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Blocked Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-blocked");
    let source_url = server.url_for("/orchestration-source-blocked");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-blocked.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "type",
                    "payload": {
                        "selector": "#name",
                        "text": "Partial orchestration",
                        "clear": true
                    }
                },
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#missing"
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("orchestration rule id should be present")
        .to_string();

    let executed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["committed_steps"], 1,
        "{executed}"
    );
    assert_eq!(executed["data"]["result"]["total_steps"], 2, "{executed}");
    assert_eq!(
        executed["data"]["result"]["steps"][0]["status"], "committed",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["steps"][1]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["error_code"], "ELEMENT_NOT_FOUND",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["rule"]["status"], "blocked",
        "{executed}"
    );

    let inspected_status = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected_status["success"], true, "{inspected_status}");
    assert_eq!(
        inspected_status["data"]["result"]["value"], "Pending",
        "{inspected_status}"
    );

    let inspected_value = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "value",
                "--selector",
                "#name",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected_value["success"], true, "{inspected_value}");
    assert_eq!(
        inspected_value["data"]["result"]["value"], "Partial orchestration",
        "{inspected_value}"
    );

    cleanup(&home);
}

/// T437q: named workflow assets should be able to register and execute orchestration rules
/// through the canonical pipe surface without inventing a second orchestration engine.
#[test]
#[ignore]
#[serial]
fn t437q_pipe_workflow_can_manage_orchestration_rules() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-workflow",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Workflow Orchestration Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-workflow",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Workflow Orchestration Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-workflow");
    let source_url = server.url_for("/orchestration-source-workflow");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let orchestration_spec = json!({
        "source": {
            "session_id": source_session_id,
            "tab_index": 0
        },
        "target": {
            "session_id": target_session_id,
            "tab_index": 0
        },
        "mode": "once",
        "condition": {
            "kind": "text_present",
            "text": "Ready"
        },
        "actions": [
            {
                "kind": "browser_command",
                "command": "type",
                "payload": {
                    "selector": "#name",
                    "text": "Workflow orchestration",
                    "clear": true
                }
            },
            {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#apply",
                    "wait_after": {
                        "text": "Workflow orchestration",
                        "timeout_ms": 5000
                    }
                }
            }
        ]
    });
    let orchestration_spec_string =
        serde_json::to_string(&orchestration_spec).expect("orchestration spec json");

    let rub_paths = RubPaths::new(Path::new(&home));
    std::fs::create_dir_all(rub_paths.workflows_dir()).unwrap();
    let workflow_path = rub_paths.workflows_dir().join("orchestration_manage.json");
    std::fs::write(
        &workflow_path,
        serde_json::to_vec_pretty(&json!([
            {
                "command": "orchestration",
                "args": {
                    "sub": "add",
                    "spec": orchestration_spec_string,
                    "spec_source": {
                        "kind": "workflow_asset",
                        "path": workflow_path.display().to_string()
                    }
                }
            },
            {
                "command": "orchestration",
                "args": {
                    "sub": "execute",
                    "id": 1
                }
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let actual = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "pipe",
                "--workflow",
                "orchestration_manage",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(actual["success"], true, "{actual}");
    assert_eq!(
        actual["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{actual}"
    );
    assert_eq!(
        actual["data"]["steps"][0]["action"]["command"], "orchestration",
        "{actual}"
    );
    assert_eq!(
        actual["data"]["steps"][0]["result"]["rule"]["id"], 1,
        "{actual}"
    );
    assert_eq!(
        actual["data"]["steps"][1]["result"]["result"]["status"], "fired",
        "{actual}"
    );
    assert_eq!(
        actual["data"]["steps"][1]["result"]["result"]["committed_steps"], 2,
        "{actual}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Workflow orchestration",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437r: history export should preserve replayable orchestration mutation steps while skipping
/// orchestration observation subcommands by default.
#[test]
#[ignore]
#[serial]
fn t437r_history_export_preserves_replayable_orchestration_steps() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-export",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Export Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-export",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Export Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-export");
    let source_url = server.url_for("/orchestration-source-export");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-export.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "type",
                    "payload": {
                        "selector": "#name",
                        "text": "Replayable orchestration",
                        "clear": true
                    }
                },
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "Replayable orchestration",
                            "timeout_ms": 5000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");

    let listed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");

    let executed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(executed["data"]["result"]["status"], "fired", "{executed}");

    let exported = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "history",
                "--export-pipe",
                "--last",
                "3",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(
        exported["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{exported}"
    );
    assert_eq!(exported["data"]["skipped"]["observation"], 1, "{exported}");
    assert_eq!(
        exported["data"]["steps"][0]["command"], "orchestration",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][0]["args"]["sub"], "add",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][0]["source"]["capture_class"], "workflow",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][1]["command"], "orchestration",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][1]["args"]["sub"], "execute",
        "{exported}"
    );
    assert_eq!(exported["data"]["steps"][1]["args"]["id"], 1, "{exported}");
    assert_eq!(
        exported["data"]["steps"][1]["source"]["capture_class"], "workflow",
        "{exported}"
    );

    cleanup(&home);
}

/// T437aa: orchestration rules should export as reusable named assets and replay through the
/// canonical orchestration registry surface without preserving live-only identity fields.
#[test]
#[ignore]
#[serial]
fn t437aa_orchestration_assets_export_and_replay_named_rules() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-asset",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Asset Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-asset",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Asset Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-asset");
    let source_url = server.url_for("/orchestration-source-asset");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-asset-spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "execution_policy": {
                "max_retries": 2
            },
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "type",
                    "payload": {
                        "selector": "#name",
                        "text": "Named asset orchestration",
                        "clear": true
                    }
                },
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "Named asset orchestration",
                            "timeout_ms": 5000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    assert_eq!(added["data"]["result"]["rule"]["id"], 1, "{added}");

    let exported = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "export",
                "1",
                "--save-as",
                "named_reply_rule",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["asset_name"], "named_reply_rule",
        "{exported}"
    );
    let saved_to = exported["data"]["result"]["persisted_artifacts"][0]["path"]
        .as_str()
        .expect("named orchestration asset path");
    let saved_spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(saved_to).expect("exported asset should exist"))
            .expect("exported asset should be valid json");
    assert!(saved_spec.get("correlation_key").is_none(), "{saved_spec}");
    assert!(saved_spec.get("idempotency_key").is_none(), "{saved_spec}");
    assert_eq!(
        saved_spec["execution_policy"]["max_retries"], 2,
        "{exported}"
    );

    let listed_assets = parse_json(
        &rub_cmd(&home)
            .args(["orchestration", "list-assets"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed_assets["success"], true, "{listed_assets}");
    assert_eq!(
        listed_assets["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1,
        "{listed_assets}"
    );
    assert_eq!(
        listed_assets["data"]["result"]["items"][0]["name"], "named_reply_rule",
        "{listed_assets}"
    );

    let removed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "remove", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["success"], true, "{removed}");

    let readded = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--asset",
                "named_reply_rule",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(readded["success"], true, "{readded}");
    assert_eq!(readded["data"]["result"]["rule"]["id"], 2, "{readded}");

    let executed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", "2"])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(executed["data"]["result"]["status"], "fired", "{executed}");
    assert_eq!(
        executed["data"]["result"]["committed_steps"], 2,
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Named asset orchestration",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437ab: workflow assets should be able to embed reactive orchestration blocks through a
/// top-level `watch` alias without inventing a second orchestration engine.
#[test]
#[ignore]
#[serial]
fn t437ab_pipe_workflow_embedded_watch_block_registers_orchestration_rules() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-embedded-watch",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Embedded Watch Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-embedded-watch",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Embedded Watch Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-embedded-watch");
    let source_url = server.url_for("/orchestration-source-embedded-watch");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let orchestration_spec = json!({
        "source": {
            "session_id": source_session_id,
            "tab_index": 0
        },
        "target": {
            "session_id": target_session_id,
            "tab_index": 0
        },
        "mode": "once",
        "condition": {
            "kind": "text_present",
            "text": "Ready"
        },
        "actions": [
            {
                "kind": "browser_command",
                "command": "type",
                "payload": {
                    "selector": "#name",
                    "text": "Embedded watch orchestration",
                    "clear": true
                }
            },
            {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#apply",
                    "wait_after": {
                        "text": "Embedded watch orchestration",
                        "timeout_ms": 5000
                    }
                }
            }
        ]
    });

    let rub_paths = RubPaths::new(Path::new(&home));
    std::fs::create_dir_all(rub_paths.workflows_dir()).unwrap();
    let workflow_path = rub_paths.workflows_dir().join("embedded_watch_rule.json");
    std::fs::write(
        &workflow_path,
        serde_json::to_vec_pretty(&json!({
            "steps": [],
            "orchestrations": [
                {
                    "label": "embedded watch rule",
                    "spec": orchestration_spec,
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let registered = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "pipe",
                "--workflow",
                "embedded_watch_rule",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(registered["success"], true, "{registered}");
    assert_eq!(
        registered["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1,
        "{registered}"
    );
    assert_eq!(
        registered["data"]["steps"][0]["action"]["command"], "orchestration",
        "{registered}"
    );
    assert_eq!(
        registered["data"]["steps"][0]["action"]["label"], "embedded watch rule",
        "{registered}"
    );
    assert_eq!(
        registered["data"]["steps"][0]["result"]["spec_source"]["kind"], "workflow_embedded",
        "{registered}"
    );

    let rule_id = registered["data"]["steps"][0]["result"]["rule"]["id"]
        .as_u64()
        .expect("embedded watch rule id") as u32;

    let executed = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "execute",
                &rule_id.to_string(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(executed["data"]["result"]["status"], "fired", "{executed}");
    assert_eq!(
        executed["data"]["result"]["committed_steps"], 2,
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Embedded watch orchestration",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437s: repeat-mode orchestration rules should re-arm after success and block
/// during an active cooldown window instead of behaving like terminal once rules.
#[test]
#[ignore]
#[serial]
fn t437s_orchestration_repeat_mode_rearms_and_enforces_cooldown() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-repeat",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Repeat Target</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    let applyCount = 0;
    document.getElementById('apply').addEventListener('click', () => {
      applyCount += 1;
      const value = document.getElementById('name').value || 'Pending';
      document.getElementById('status').textContent = `${value}:${applyCount}`;
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-repeat",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Repeat Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-repeat");
    let source_url = server.url_for("/orchestration-source-repeat");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-repeat.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "repeat",
            "execution_policy": {
                "cooldown_ms": 1200,
                "max_retries": 0
            },
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "type",
                    "payload": {
                        "selector": "#name",
                        "text": "Repeat orchestration",
                        "clear": true
                    }
                },
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "Repeat orchestration",
                            "timeout_ms": 5000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("repeat orchestration rule id should be present")
        .to_string();

    let first = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(first["success"], true, "{first}");
    assert_eq!(first["data"]["result"]["status"], "fired", "{first}");
    assert_eq!(first["data"]["result"]["next_status"], "armed", "{first}");
    assert_eq!(
        first["data"]["result"]["rule"]["status"], "armed",
        "{first}"
    );
    assert!(
        first["data"]["result"]["rule"]["execution_policy"]["cooldown_until_ms"].is_u64(),
        "{first}"
    );
    assert_eq!(
        first["data"]["runtime"]["cooldown_rule_count"], 1,
        "{first}"
    );

    let inspected_after_first = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspected_after_first["success"], true,
        "{inspected_after_first}"
    );
    assert_eq!(
        inspected_after_first["data"]["result"]["value"], "Repeat orchestration:1",
        "{inspected_after_first}"
    );

    let second = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(second["success"], true, "{second}");
    assert_eq!(second["data"]["result"]["status"], "blocked", "{second}");
    assert_eq!(second["data"]["result"]["next_status"], "armed", "{second}");
    assert_eq!(
        second["data"]["result"]["reason"], "orchestration_cooldown_active",
        "{second}"
    );
    assert_eq!(second["data"]["result"]["committed_steps"], 0, "{second}");
    assert_eq!(
        second["data"]["result"]["rule"]["status"], "armed",
        "{second}"
    );

    let inspected_after_second = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspected_after_second["data"]["result"]["value"], "Repeat orchestration:1",
        "{inspected_after_second}"
    );

    std::thread::sleep(std::time::Duration::from_millis(1_300));

    let third = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(third["success"], true, "{third}");
    assert_eq!(third["data"]["result"]["status"], "fired", "{third}");
    assert_eq!(third["data"]["result"]["next_status"], "armed", "{third}");
    assert_eq!(
        third["data"]["result"]["rule"]["status"], "armed",
        "{third}"
    );

    let inspected_after_third = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        inspected_after_third["success"], true,
        "{inspected_after_third}"
    );
    assert_eq!(
        inspected_after_third["data"]["result"]["value"], "Repeat orchestration:2",
        "{inspected_after_third}"
    );

    cleanup(&home);
}

/// T437t: orchestration rules should become unavailable when the bound target session closes,
/// and manual execution must reject before attempting any cross-session dispatch.
#[test]
#[ignore]
#[serial]
fn t437t_orchestration_rule_becomes_unavailable_when_target_session_closes() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-unavailable",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Unavailable</title></head>
<body>
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent = 'Applied';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-unavailable",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Unavailable</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-unavailable");
    let source_url = server.url_for("/orchestration-source-unavailable");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-target-unavailable.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply"
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("orchestration rule id should be present");

    let closed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "target", "close"])
            .output()
            .unwrap(),
    );
    assert_eq!(closed["success"], true, "{closed}");

    let listed =
        wait_for_orchestration_unavailable_reason(&home, rule_id, "target_session_missing");
    assert_eq!(listed["data"]["active_rule_count"], 0, "{listed}");
    assert_eq!(listed["data"]["unavailable_rule_count"], 1, "{listed}");
    assert_eq!(
        listed["data"]["groups"][0]["unavailable_rule_count"], 1,
        "{listed}"
    );

    let trace = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "trace",
                "--last",
                "5",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    assert!(
        trace["data"]["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "unavailable"
                    && event["rule_id"].as_u64() == Some(rule_id)
                    && event["unavailable_reason"] == "target_session_missing"
            }),
        "{trace}"
    );

    let executed = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "execute",
                &rule_id.to_string(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], false, "{executed}");
    assert_eq!(executed["error"]["code"], "INVALID_INPUT", "{executed}");
    assert_eq!(
        executed["error"]["context"]["reason"], "orchestration_rule_unavailable",
        "{executed}"
    );
    assert_eq!(
        executed["error"]["context"]["unavailable_reason"], "target_session_missing",
        "{executed}"
    );

    cleanup(&home);
}

/// T437u: orchestration execution should truthfully block when the target session is under
/// human takeover, instead of bypassing the target session's automation pause fence.
#[test]
#[ignore]
#[serial]
fn t437u_orchestration_execute_is_blocked_when_target_takeover_is_active() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target-takeover",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Takeover</title></head>
<body>
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent = 'Applied';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-takeover",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Takeover</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-takeover");
    let source_url = server.url_for("/orchestration-source-takeover");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--headed", "--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-target-takeover.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply"
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("orchestration rule id should be present")
        .to_string();

    let takeover = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "--session", "target", "takeover", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(takeover["success"], true, "{takeover}");
    assert_eq!(
        takeover["data"]["runtime"]["status"], "active",
        "{takeover}"
    );
    assert_eq!(takeover["data"]["automation_paused"], true, "{takeover}");

    let executed = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["error_code"], "AUTOMATION_PAUSED",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["rule"]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["steps"][0]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["steps"][0]["error_code"], "AUTOMATION_PAUSED",
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--headed",
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Pending",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437v: reactive orchestration should be able to probe a remote source session and
/// fire a canonical action on a remote target session through a manager-hosted registry.
#[test]
#[ignore]
#[serial]
fn t437v_reactive_orchestration_fires_across_remote_source_and_target_sessions() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-manager-reactive",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Manager Reactive</title></head>
<body>
  <div id="status">Manager</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-reactive",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Reactive</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-target-reactive",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Reactive</title></head>
<body>
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent = 'Applied';
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let manager_url = server.url_for("/orchestration-manager-reactive");
    let source_url = server.url_for("/orchestration-source-reactive");
    let target_url = server.url_for("/orchestration-target-reactive");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-reactive-remote.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "Applied",
                            "timeout_ms": 5000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration rule id should be present");

    let listed = wait_for_orchestration_status(&home, "manager", rule_id, "fired");
    assert_eq!(
        listed["data"]["runtime"]["last_rule_id"], rule_id,
        "{listed}"
    );
    assert_eq!(
        listed["data"]["runtime"]["last_rule_result"]["status"], "fired",
        "{listed}"
    );
    assert_eq!(
        listed["data"]["result"]["items"][0]["last_condition_evidence"]["summary"],
        "source_tab_text_present:Ready",
        "{listed}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Applied",
        "{inspected}"
    );

    let trace = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "trace",
                "--last",
                "5",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    assert!(
        trace["data"]["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "fired"
                    && event["rule_id"].as_u64() == Some(rule_id)
                    && event["evidence"]["summary"] == "source_tab_text_present:Ready"
            }),
        "{trace}"
    );

    cleanup(&home);
}

/// T437z: reactive orchestration should support explicit source/target frame routing across sessions
/// without mutating the remote sessions' selected-frame authority.
#[test]
#[ignore]
#[serial]
fn t437z_reactive_orchestration_routes_source_and_target_frames_across_sessions() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-manager-reactive-frames",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Manager Reactive Frames</title></head>
<body>
  <div id="status">Manager</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-reactive-frames",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Reactive Frames</title></head>
<body>
  <div id="top-status">Waiting</div>
  <iframe
    id="source-frame"
    name="source-frame"
    src="/orchestration-source-reactive-frames-child"
    style="width:100%;height:160px;border:0"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-reactive-frames-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Reactive Frames Child</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-target-reactive-frames",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Reactive Frames</title></head>
<body>
  <div id="top-status">Top Pending</div>
  <iframe
    id="target-frame"
    name="target-frame"
    src="/orchestration-target-reactive-frames-child"
    style="width:100%;height:220px;border:0"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/orchestration-target-reactive-frames-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Reactive Frames Child</title></head>
<body>
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent = 'Applied';
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let manager_url = server.url_for("/orchestration-manager-reactive-frames");
    let source_url = server.url_for("/orchestration-source-reactive-frames");
    let target_url = server.url_for("/orchestration-target-reactive-frames");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let source_frames = parse_json(
        &rub_cmd(&home)
            .args(["--session", "source", "frames"])
            .output()
            .unwrap(),
    );
    assert_eq!(source_frames["success"], true, "{source_frames}");
    let source_frame_id = frame_id_by_name(&source_frames, "source-frame");

    let target_frames = parse_json(
        &rub_cmd(&home)
            .args(["--session", "target", "frames"])
            .output()
            .unwrap(),
    );
    assert_eq!(target_frames["success"], true, "{target_frames}");
    let target_frame_id = frame_id_by_name(&target_frames, "target-frame");

    let spec_path = format!("{home}/orchestration-reactive-frames.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0,
                "frame_id": source_frame_id
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0,
                "frame_id": target_frame_id
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply"
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration frame rule id should be present");

    let fired = wait_for_orchestration_status(&home, "manager", rule_id, "fired");
    let rule = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("fired orchestration frame rule should remain in runtime projection");
    assert_eq!(rule["last_result"]["status"], "fired", "{fired}");
    assert_eq!(
        rule["last_condition_evidence"]["summary"], "source_tab_text_present:Ready",
        "{fired}"
    );
    assert_eq!(rule["source"]["frame_id"], source_frame_id, "{fired}");
    assert_eq!(rule["target"]["frame_id"], target_frame_id, "{fired}");

    let switch_target_frame = parse_json(
        &rub_cmd(&home)
            .args(["--session", "target", "frame", "--name", "target-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        switch_target_frame["success"], true,
        "{switch_target_frame}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Applied",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437w: reactive cross-session orchestration should support workflow payload.source_vars via source-side live read authority.
#[test]
#[ignore]
#[serial]
fn t437w_reactive_orchestration_workflow_supports_source_derived_vars() {
    let home = unique_home();
    cleanup(&home);
    std::fs::create_dir_all(PathBuf::from(&home).join("workflows")).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-manager-reactive-workflow-vars",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Manager Reactive Workflow Vars</title></head>
<body>
  <div id="status">Manager</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-reactive-workflow-vars",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Reactive Workflow Vars</title></head>
<body>
  <div id="question">Answer from remote source session</div>
  <div id="status">Waiting</div>
  <script>
    setTimeout(() => {
      document.getElementById('status').textContent = 'Ready';
    }, 1200);
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-target-reactive-workflow-vars",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Reactive Workflow Vars</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let manager_url = server.url_for("/orchestration-manager-reactive-workflow-vars");
    let source_url = server.url_for("/orchestration-source-reactive-workflow-vars");
    let target_url = server.url_for("/orchestration-target-reactive-workflow-vars");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let workflow_path = PathBuf::from(&home).join("workflows/orchestration_reply_from_source.json");
    std::fs::write(
        &workflow_path,
        serde_json::to_vec_pretty(&json!([
            {
                "command": "type",
                "args": {
                    "selector": "#name",
                    "text": "{{reply_name}}",
                    "clear": true
                }
            },
            {
                "command": "click",
                "args": {
                    "selector": "#apply",
                    "wait_after": {
                        "text": "{{reply_name}}",
                        "timeout_ms": 5000
                    }
                }
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let spec_path = format!("{home}/orchestration-reactive-workflow-vars.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "workflow",
                    "payload": {
                        "workflow_name": "orchestration_reply_from_source",
                        "source_vars": {
                            "reply_name": {
                                "kind": "text",
                                "selector": "#question"
                            }
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration rule id should be present");

    let fired = wait_for_orchestration_status(&home, "manager", rule_id, "fired");
    let rule = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("fired orchestration rule should remain in runtime projection");
    assert_eq!(rule["last_result"]["status"], "fired", "{fired}");
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["kind"], "workflow",
        "{fired}"
    );
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["workflow_name"],
        "orchestration_reply_from_source",
        "{fired}"
    );
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["source_vars"],
        json!(["reply_name"]),
        "{fired}"
    );
    assert_eq!(
        rule["last_condition_evidence"]["summary"], "source_tab_text_present:Ready",
        "{fired}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Answer from remote source session",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437x: source-side workflow binding failures should block reactive orchestration before touching the target session.
#[test]
#[ignore]
#[serial]
fn t437x_reactive_orchestration_workflow_source_var_failure_is_blocked() {
    let home = unique_home();
    cleanup(&home);
    std::fs::create_dir_all(PathBuf::from(&home).join("workflows")).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-manager-reactive-workflow-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Manager Reactive Workflow Vars Blocked</title></head>
<body>
  <div id="status">Manager</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-reactive-workflow-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Reactive Workflow Vars Blocked</title></head>
<body>
  <div id="status">Waiting</div>
  <script>
    setTimeout(() => {
      document.getElementById('status').textContent = 'Ready';
    }, 1200);
  </script>
</body>
</html>"#,
        ),
        (
            "/orchestration-target-reactive-workflow-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Reactive Workflow Vars Blocked</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'Pending';
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let manager_url = server.url_for("/orchestration-manager-reactive-workflow-vars-blocked");
    let source_url = server.url_for("/orchestration-source-reactive-workflow-vars-blocked");
    let target_url = server.url_for("/orchestration-target-reactive-workflow-vars-blocked");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let workflow_path =
        PathBuf::from(&home).join("workflows/orchestration_reply_missing_source_var.json");
    std::fs::write(
        &workflow_path,
        serde_json::to_vec_pretty(&json!([
            {
                "command": "type",
                "args": {
                    "selector": "#name",
                    "text": "{{reply_name}}",
                    "clear": true
                }
            },
            {
                "command": "click",
                "args": {
                    "selector": "#apply"
                }
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let spec_path = format!("{home}/orchestration-reactive-workflow-vars-blocked.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "workflow",
                    "payload": {
                        "workflow_name": "orchestration_reply_missing_source_var",
                        "source_vars": {
                            "reply_name": {
                                "kind": "text",
                                "selector": "#missing-question"
                            }
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration rule id should be present");

    let blocked = wait_for_orchestration_status(&home, "manager", rule_id, "blocked");
    let rule = blocked["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("blocked orchestration rule should remain in runtime projection");
    assert_eq!(rule["last_result"]["status"], "blocked", "{blocked}");
    assert_eq!(
        rule["last_result"]["error_code"], "ELEMENT_NOT_FOUND",
        "{blocked}"
    );
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["kind"], "workflow",
        "{blocked}"
    );
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["workflow_name"],
        "orchestration_reply_missing_source_var",
        "{blocked}"
    );
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["source_vars"],
        json!(["reply_name"]),
        "{blocked}"
    );

    let inspected = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Pending",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437y: reactive repeat orchestration should not replay the same latched evidence after cooldown;
/// it should fire again only after the source condition clears and matches again.
#[test]
#[ignore]
#[serial]
fn t437y_reactive_repeat_orchestration_latches_evidence_until_condition_clears() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-manager-reactive-repeat-latch",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Manager Reactive Repeat Latch</title></head>
<body>
  <div id="status">Manager</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-source-reactive-repeat-latch",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source Reactive Repeat Latch</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-target-reactive-repeat-latch",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target Reactive Repeat Latch</title></head>
<body>
  <button id="apply">Apply</button>
  <div id="status">Pending</div>
  <script>
    let applyCount = 0;
    document.getElementById('apply').addEventListener('click', () => {
      applyCount += 1;
      document.getElementById('status').textContent = `Applied:${applyCount}`;
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let manager_url = server.url_for("/orchestration-manager-reactive-repeat-latch");
    let source_url = server.url_for("/orchestration-source-reactive-repeat-latch");
    let target_url = server.url_for("/orchestration-target-reactive-repeat-latch");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-reactive-repeat-latch.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": source_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": target_session_id,
                "tab_index": 0
            },
            "mode": "repeat",
            "execution_policy": {
                "cooldown_ms": 1200,
                "max_retries": 0
            },
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "actions": [
                {
                    "kind": "browser_command",
                    "command": "click",
                    "payload": {
                        "selector": "#apply",
                        "wait_after": {
                            "text": "Applied",
                            "timeout_ms": 5000
                        }
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration rule id should be present");

    let first = wait_for_orchestration_rule_result(&home, "manager", rule_id, "armed", "fired");
    let first_rule = first["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("reactive repeat rule should exist");
    assert_eq!(first_rule["last_result"]["status"], "fired", "{first}");
    assert_eq!(first_rule["last_result"]["next_status"], "armed", "{first}");
    assert!(
        first_rule["execution_policy"]["cooldown_until_ms"].is_u64(),
        "{first}"
    );

    let target_first = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(target_first["success"], true, "{target_first}");
    assert_eq!(
        target_first["data"]["result"]["value"], "Applied:1",
        "{target_first}"
    );

    std::thread::sleep(Duration::from_millis(2200));

    let target_after_cooldown = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "target",
                "inspect",
                "text",
                "--selector",
                "#status",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(
        target_after_cooldown["success"], true,
        "{target_after_cooldown}"
    );
    assert_eq!(
        target_after_cooldown["data"]["result"]["value"], "Applied:1",
        "{target_after_cooldown}"
    );

    let clear_source = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "exec",
                "document.getElementById('status').textContent = 'Waiting'; 'ok';",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(clear_source["success"], true, "{clear_source}");
    std::thread::sleep(Duration::from_millis(700));

    let rearm_source = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "source",
                "exec",
                "document.getElementById('status').textContent = 'Ready'; 'ok';",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(rearm_source["success"], true, "{rearm_source}");

    let second_applied = wait_for_text_in_session(
        &home,
        "target",
        "#status",
        "Applied:2",
        Duration::from_secs(8),
    );
    assert_eq!(second_applied, "Applied:2");

    let trace = parse_json(
        &rub_cmd(&home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "trace",
                "--last",
                "10",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    let fired_count = trace["data"]["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "fired" && event["rule_id"].as_u64() == Some(rule_id))
        .count();
    assert_eq!(fired_count, 2, "{trace}");

    cleanup(&home);
}
