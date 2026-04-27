use super::*;

/// T437m/T437n: orchestration registry should preserve lifecycle trace, correlation groups,
/// and duplicate-idempotency rejection within one cross-session browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437m_n_orchestration_registry_and_idempotency_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_standard_site_fixture();

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let registry_spec_path = format!("{home}/orchestration-rule.json");
    std::fs::write(
        &registry_spec_path,
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
        &rub_cmd(home)
            .args([
                "--session",
                "source",
                "orchestration",
                "add",
                "--file",
                &registry_spec_path,
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
        &rub_cmd(home)
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
        &rub_cmd(home)
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
        &rub_cmd(home)
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
        &rub_cmd(home)
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
    let grouped_runtime = second["data"]["runtime"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["correlation_key"] == "corr-batch-a")
        .expect("corr-batch-a group should be present");
    assert_eq!(
        grouped_runtime["rule_ids"].as_array().map(|ids| ids.len()),
        Some(2),
        "{second}"
    );
    assert_eq!(grouped_runtime["active_rule_count"], 2, "{second}");

    let duplicate = parse_json(
        &rub_cmd(home)
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
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");
    let listed_group = listed["data"]["runtime"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["correlation_key"] == "corr-batch-a")
        .expect("corr-batch-a group should remain present");
    assert_eq!(
        listed_group["rule_ids"].as_array().map(|ids| ids.len()),
        Some(2),
        "{listed}"
    );
    assert_eq!(
        listed["data"]["result"]["items"]
            .as_array()
            .map(|rules| rules.len()),
        Some(3),
        "{listed}"
    );

    teardown_and_cleanup(home);
}

/// T437q/T437ac: committed orchestration add should replay under the same command_id without
/// duplicating the live rule after commit, and conflicting spec reuse must fail closed.
#[test]
#[ignore]
#[serial]
fn t437q_orchestration_add_replay_after_commit_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (rt, server) = start_standard_site_fixture();

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");
    let socket_path = registry_socket_path_by_session_id(home, &source_session_id);

    let request = rub_ipc::protocol::IpcRequest::new(
        "orchestration",
        json!({
            "sub": "add",
            "spec": {
                "source": {
                    "session_id": source_session_id.clone(),
                    "tab_index": 0
                },
                "target": {
                    "session_id": target_session_id.clone(),
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
            },
            "paused": false
        }),
        10_000,
    )
    .with_command_id("t437q-orchestration-add-replay")
    .expect("test command_id should be valid");

    let first = send_bound_ipc_request(&rt, &socket_path, &source_session_id, &request);
    assert_eq!(
        first.status,
        rub_ipc::protocol::ResponseStatus::Success,
        "{first:?}"
    );
    assert_eq!(
        first.command_id.as_deref(),
        Some("t437q-orchestration-add-replay"),
        "{first:?}"
    );
    let first_rule_id = first
        .data
        .as_ref()
        .and_then(|data| data["result"]["rule"]["id"].as_u64())
        .expect("first orchestration add should return a rule id");

    let replayed = send_bound_ipc_request(&rt, &socket_path, &source_session_id, &request);
    assert_eq!(
        replayed.status,
        rub_ipc::protocol::ResponseStatus::Success,
        "{replayed:?}"
    );
    assert_eq!(
        replayed.command_id.as_deref(),
        Some("t437q-orchestration-add-replay"),
        "{replayed:?}"
    );
    let replayed_rule_id = replayed
        .data
        .as_ref()
        .and_then(|data| data["result"]["rule"]["id"].as_u64())
        .expect("replayed orchestration add should preserve the committed rule id");
    assert_eq!(replayed_rule_id, first_rule_id, "{replayed:?}");

    let conflicting_request = rub_ipc::protocol::IpcRequest::new(
        "orchestration",
        json!({
            "sub": "add",
            "spec": {
                "source": {
                    "session_id": source_session_id.clone(),
                    "tab_index": 0
                },
                "target": {
                    "session_id": target_session_id.clone(),
                    "tab_index": 0
                },
                "mode": "once",
                "condition": {
                    "kind": "text_present",
                    "text": "Blocked"
                },
                "actions": [
                    {
                        "kind": "workflow",
                        "payload": {
                            "workflow_name": "reply_flow"
                        }
                    }
                ]
            },
            "paused": false
        }),
        10_000,
    )
    .with_command_id("t437q-orchestration-add-replay")
    .expect("test command_id should be valid");

    let conflict =
        send_bound_ipc_request(&rt, &socket_path, &source_session_id, &conflicting_request);
    assert_eq!(
        conflict.status,
        rub_ipc::protocol::ResponseStatus::Error,
        "{conflict:?}"
    );
    assert_eq!(
        conflict.command_id.as_deref(),
        Some("t437q-orchestration-add-replay"),
        "{conflict:?}"
    );
    let conflict_error = conflict
        .error
        .as_ref()
        .expect("conflicting orchestration replay must return an error envelope");
    assert_eq!(
        conflict_error.code,
        rub_core::error::ErrorCode::IpcProtocolError,
        "{conflict:?}"
    );
    assert_eq!(
        conflict_error
            .context
            .as_ref()
            .and_then(|context| context["reason"].as_str()),
        Some("replay_command_id_fingerprint_mismatch"),
        "{conflict:?}"
    );

    let listed = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");
    let items = listed["data"]["result"]["items"]
        .as_array()
        .expect("orchestration list should return items");
    assert_eq!(items.len(), 1, "{listed}");
    assert_eq!(items[0]["id"].as_u64(), Some(first_rule_id), "{listed}");
    assert_eq!(
        listed["data"]["runtime"]["active_rule_count"], 1,
        "{listed}"
    );

    let trace = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "source",
                "orchestration",
                "trace",
                "--last",
                "10",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    let registered_events = trace["data"]["result"]["events"]
        .as_array()
        .expect("orchestration trace should return events")
        .iter()
        .filter(|event| {
            event["kind"] == "registered" && event["rule_id"].as_u64() == Some(first_rule_id)
        })
        .count();
    assert_eq!(
        registered_events, 1,
        "replayed orchestration add must not register a second live rule: {trace}"
    );

    let removed = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "source",
                "orchestration",
                "remove",
                &first_rule_id.to_string(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["success"], true, "{removed}");

    teardown_and_cleanup(home);
}

/// T437o/T437p: orchestration execute should truthfully classify fired vs partial-blocked
/// multi-action pipelines within one cross-session browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437o_p_orchestration_execute_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
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

    let target_url = server.url_for("/orchestration-target-execute");
    let source_url = server.url_for("/orchestration-source-execute");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        wait_for_text_in_session(home, "source", "#status", "Ready", Duration::from_secs(5)),
        "Ready"
    );
    assert_eq!(
        wait_for_text_in_session(home, "target", "#status", "Pending", Duration::from_secs(5)),
        "Pending"
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
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
        &rub_cmd(home)
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
    let rule_id_arg = rule_id.to_string();

    let executed = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "execute", &rule_id_arg])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["execution"]["status"], "fired",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["committed_steps"], 2,
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["total_steps"], 2,
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["steps"][0]["action"]["command"], "type",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["steps"][0]["result"]["interaction"]["semantic_class"],
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
        &rub_cmd(home)
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
    let target_url = server.url_for("/orchestration-target-blocked");
    let source_url = server.url_for("/orchestration-source-blocked");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
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
        &rub_cmd(home)
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
    let rule_id_arg = rule_id.to_string();

    let executed = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "execute", &rule_id_arg])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["execution"]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["committed_steps"], 1,
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["total_steps"], 2,
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["steps"][0]["status"], "committed",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["steps"][1]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["error_code"], "ELEMENT_NOT_FOUND",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["reason"],
        "orchestration_remote_error_response",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["rule"]["status"], "blocked",
        "{executed}"
    );

    let blocked = wait_for_orchestration_rule_result(home, "source", rule_id, "blocked", "blocked");
    let blocked_rule = blocked["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("blocked orchestration rule should remain in runtime projection");
    assert_eq!(blocked_rule["status"], "blocked", "{blocked}");
    assert_eq!(blocked_rule["last_result"]["status"], "blocked", "{blocked}");
    assert_eq!(blocked_rule["last_result"]["committed_steps"], 1, "{blocked}");
    assert_eq!(blocked_rule["last_result"]["total_steps"], 2, "{blocked}");
    assert_eq!(
        blocked_rule["last_result"]["steps"][0]["status"],
        "committed",
        "{blocked}"
    );
    assert_eq!(
        blocked_rule["last_result"]["steps"][1]["status"],
        "blocked",
        "{blocked}"
    );
    assert_eq!(
        blocked_rule["last_result"]["error_code"],
        "ELEMENT_NOT_FOUND",
        "{blocked}"
    );

    let inspected_status = parse_json(
        &rub_cmd(home)
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
        &rub_cmd(home)
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

    teardown_and_cleanup(home);
}

/// T437q/T437r: workflow-managed orchestration and history export should share one browser-backed
/// scenario without inventing a second orchestration engine.
#[test]
#[ignore]
#[serial]
fn t437q_r_pipe_workflow_and_history_export_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_test_server(vec![
        (
            "/orchestration-target",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Target</title></head>
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
            "/orchestration-source",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Source</title></head>
<body>
  <div id="status">Ready</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target");
    let source_url = server.url_for("/orchestration-source");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let workflow_spec = json!({
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
    let workflow_spec_string =
        serde_json::to_string(&workflow_spec).expect("orchestration workflow spec json");

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
                    "spec": workflow_spec_string,
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
        &rub_cmd(home)
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
        actual["data"]["steps"][0]["result"]["result"]["rule"]["id"], 1,
        "{actual}"
    );
    assert_eq!(
        actual["data"]["steps"][1]["result"]["result"]["execution"]["status"], "fired",
        "{actual}"
    );
    assert_eq!(
        actual["data"]["steps"][1]["result"]["result"]["execution"]["committed_steps"], 2,
        "{actual}"
    );

    let inspected = parse_json(
        &rub_cmd(home)
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
        &rub_cmd(home)
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
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");

    let executed = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "execute", "2"])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["execution"]["status"], "fired",
        "{executed}"
    );

    let exported = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "source",
                "history",
                "--export-pipe",
                "--last",
                "5",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    let steps = exported["data"]["result"]["steps"]
        .as_array()
        .expect("history export steps should be an array");
    assert!(
        steps.len() >= 2,
        "expected at least two workflow-captured steps, got {exported}"
    );
    assert!(
        steps.iter().any(|step| {
            step["command"] == "open"
                && step["args"]["url"]
                    .as_str()
                    .is_some_and(|url| url.contains("/orchestration-source"))
        }),
        "{exported}"
    );
    assert!(
        steps
            .iter()
            .any(|step| { step["command"] == "orchestration" && step["args"]["sub"] == "add" }),
        "{exported}"
    );
    let entries = exported["data"]["result"]["entries"]
        .as_array()
        .expect("history export entries should be an array");
    assert!(
        entries
            .iter()
            .filter(|entry| entry["source"]["capture_class"] == "workflow")
            .count()
            >= 2,
        "{exported}"
    );

    teardown_and_cleanup(home);
}

/// T437aa/T437ab: orchestration asset export/replay and embedded watch registration should reuse
/// one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437ab1_orchestration_export_local_persistence_failure_surfaces_committed_top_level_error() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_standard_site_fixture();

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &server.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let spec_path = format!("{home}/orchestration-export-guardrail.json");
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
                    "command": "reload"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(home)
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

    let failing_output_dir = std::env::temp_dir().join(format!(
        "rub-orchestration-export-committed-failure-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&failing_output_dir).unwrap();
    let export_output = rub_cmd(home)
        .args([
            "--session",
            "source",
            "orchestration",
            "export",
            &rule_id,
            "--output",
            failing_output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let export_failure = parse_json(&export_output);
    let _ = std::fs::remove_dir_all(&failing_output_dir);

    assert!(!export_output.status.success(), "{export_failure}");
    assert_eq!(export_failure["success"], false, "{export_failure}");
    assert_eq!(
        export_failure["error"]["context"]["reason"], "post_commit_orchestration_export_failed",
        "{export_failure}"
    );
    assert_eq!(
        export_failure["error"]["context"]["daemon_request_committed"], true,
        "{export_failure}"
    );
    assert_eq!(
        export_failure["error"]["context"]["committed_response_projection"]["result"]["format"],
        "orchestration",
        "{export_failure}"
    );
    assert!(export_failure["data"].is_null(), "{export_failure}");

    teardown_and_cleanup(home);
}

#[test]
#[ignore]
#[serial]
fn t437aa_ab_orchestration_assets_and_embedded_watch_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
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
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
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
            "correlation_key": "asset-corr",
            "idempotency_key": "asset-idem",
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
        &rub_cmd(home)
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
        &rub_cmd(home)
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
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["projection_state"]["truth_level"],
        "local_persistence_projection",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["projection_state"]["projection_authority"],
        "cli.orchestration_export_asset_persistence",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["projection_state"]["durability"],
        "durable",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["surface"],
        "orchestration_rule_identity",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["truth_level"],
        "operator_projection",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["projection_kind"],
        "live_rule_identity",
        "{exported}"
    );

    let failing_output_dir = std::env::temp_dir().join(format!(
        "rub-orchestration-export-followup-failure-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&failing_output_dir).unwrap();
    let post_commit_output = rub_cmd(home)
        .args([
            "--session",
            "source",
            "orchestration",
            "export",
            "1",
            "--output",
            failing_output_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let post_commit_failure = parse_json(&post_commit_output);
    let _ = std::fs::remove_dir_all(&failing_output_dir);
    assert!(
        !post_commit_output.status.success(),
        "{post_commit_failure}"
    );
    assert_eq!(
        post_commit_failure["success"], false,
        "{post_commit_failure}"
    );
    assert_eq!(
        post_commit_failure["error"]["context"]["reason"],
        "post_commit_orchestration_export_failed",
        "{post_commit_failure}"
    );
    assert_eq!(
        post_commit_failure["error"]["context"]["daemon_request_committed"], true,
        "{post_commit_failure}"
    );
    assert_eq!(
        post_commit_failure["error"]["context"]["committed_response_projection"]["result"]["format"],
        "orchestration",
        "{post_commit_failure}"
    );
    assert!(
        post_commit_failure["data"].is_null(),
        "{post_commit_failure}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["projection_authority"],
        "session.orchestration_runtime.rules",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["upstream_truth"],
        "session_orchestration_rule",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["control_role"], "display_only",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["durability"], "best_effort",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["correlation_key"], "asset-corr",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["rule_identity_projection"]["idempotency_key"], "asset-idem",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["source_rule_identity"]["correlation_key"],
        "asset-corr",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["source_rule_identity"]["idempotency_key"],
        "asset-idem",
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
        &rub_cmd(home)
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
    assert_eq!(
        listed_assets["data"]["subject"]["directory_state"]["path_authority"],
        "cli.orchestration_assets.directory",
        "{listed_assets}"
    );
    assert_eq!(
        listed_assets["data"]["result"]["items"][0]["path_state"]["path_authority"],
        "cli.orchestration_assets.item.path",
        "{listed_assets}"
    );

    let removed = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "remove", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["success"], true, "{removed}");

    let readded = parse_json(
        &rub_cmd(home)
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
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "execute", "2"])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["execution"]["status"], "fired",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["committed_steps"], 2,
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(home)
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
        &rub_cmd(home)
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
        registered["data"]["steps"][0]["result"]["result"]["spec_source"]["kind"],
        "workflow_embedded",
        "{registered}"
    );
    assert_eq!(
        registered["data"]["steps"][0]["result"]["result"]["spec_source"]["workflow_source"]["path_state"]
            ["truth_level"],
        "input_path_reference",
        "{registered}"
    );
    assert_eq!(
        registered["data"]["steps"][0]["result"]["result"]["spec_source"]["workflow_source"]["path_state"]
            ["path_authority"],
        "cli.pipe.spec_source.path",
        "{registered}"
    );

    let rule_id = registered["data"]["steps"][0]["result"]["result"]["rule"]["id"]
        .as_u64()
        .expect("embedded watch rule id") as u32;

    let executed = parse_json(
        &rub_cmd(home)
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
    assert_eq!(
        executed["data"]["result"]["execution"]["status"], "fired",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["committed_steps"], 2,
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(home)
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

    teardown_and_cleanup(home);
}

/// T437s/T437y: manual repeat re-arm/cooldown and reactive repeat evidence latching
/// should reuse one browser-backed scenario without mixing their authorities.
#[test]
#[ignore]
#[serial]
fn t437s_y_orchestration_repeat_and_reactive_latch_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (rt, server) = start_test_server(vec![
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
  <div id="status">Waiting</div>
</body>
</html>"#,
        ),
        (
            "/orchestration-manual-repeat-local",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Orchestration Manual Repeat Local</title></head>
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
  <button id="set-ready">Arm</button>
  <button id="set-waiting">Clear</button>
  <div id="status">Ready</div>
  <script>
    document.getElementById('set-ready').addEventListener('click', () => {
      document.getElementById('status').textContent = 'Ready';
    });
    document.getElementById('set-waiting').addEventListener('click', () => {
      document.getElementById('status').textContent = 'Waiting';
    });
  </script>
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

    let source_url = server.url_for("/orchestration-manual-repeat-local");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "repeat", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        wait_for_text_in_session(home, "repeat", "#status", "Pending", Duration::from_secs(5)),
        "Pending"
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let repeat_session_id = session_id_by_name(&sessions, "repeat");

    let spec_path = format!("{home}/orchestration-repeat.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source": {
                "session_id": repeat_session_id,
                "tab_index": 0
            },
            "target": {
                "session_id": repeat_session_id,
                "tab_index": 0
            },
            "mode": "repeat",
            "execution_policy": {
                "cooldown_ms": 1200,
                "max_retries": 0
            },
            "condition": {
                "kind": "network_request",
                "url_pattern": "/manual-only-never-fired"
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
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
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

    let arm_source = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
                "exec",
                "document.getElementById('status').textContent = 'Ready'; 'ok';",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(arm_source["success"], true, "{arm_source}");
    let first = parse_json(
        &rub_cmd(home)
            .args(["--session", "repeat", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(first["success"], true, "{first}");
    assert_eq!(
        first["data"]["result"]["execution"]["status"], "fired",
        "{first}"
    );
    assert_eq!(
        first["data"]["result"]["execution"]["next_status"], "armed",
        "{first}"
    );
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
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
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
        &rub_cmd(home)
            .args(["--session", "repeat", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(second["success"], true, "{second}");
    assert_eq!(
        second["data"]["result"]["execution"]["status"], "blocked",
        "{second}"
    );
    assert_eq!(
        second["data"]["result"]["execution"]["next_status"], "armed",
        "{second}"
    );
    assert_eq!(
        second["data"]["result"]["execution"]["reason"], "orchestration_cooldown_active",
        "{second}"
    );
    assert_eq!(
        second["data"]["result"]["execution"]["committed_steps"], 0,
        "{second}"
    );
    assert_eq!(
        second["data"]["result"]["rule"]["status"], "armed",
        "{second}"
    );

    let inspected_after_second = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
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

    let cooled = wait_for_orchestration_cooldown_to_expire(
        home,
        "repeat",
        rule_id.parse().expect("repeat rule id should stay numeric"),
    );
    assert_eq!(cooled["success"], true, "{cooled}");

    let clear_source = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
                "exec",
                "document.getElementById('status').textContent = 'Pending'; 'ok';",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(clear_source["success"], true, "{clear_source}");

    let rearm_source = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
                "exec",
                "document.getElementById('status').textContent = 'Ready'; 'ok';",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(rearm_source["success"], true, "{rearm_source}");
    let third = parse_json(
        &rub_cmd(home)
            .args(["--session", "repeat", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(third["success"], true, "{third}");
    assert_eq!(
        third["data"]["result"]["execution"]["status"], "fired",
        "{third}"
    );
    assert_eq!(
        third["data"]["result"]["execution"]["next_status"], "armed",
        "{third}"
    );
    assert_eq!(
        third["data"]["result"]["rule"]["status"], "armed",
        "{third}"
    );

    let inspected_after_third = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "repeat",
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
    let source_idle =
        wait_for_session_in_flight_count(&rt, home, &repeat_session_id, 0, Duration::from_secs(5));
    assert_eq!(
        source_idle
            .data
            .as_ref()
            .and_then(|data| data.get("in_flight_count"))
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "{source_idle:?}"
    );

    let manager_url = server.url_for("/orchestration-manager-reactive-repeat-latch");
    let source_url = server.url_for("/orchestration-source-reactive-repeat-latch");
    let target_url = server.url_for("/orchestration-target-reactive-repeat-latch");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        wait_for_text_in_session(
            home,
            "manager",
            "#status",
            "Manager",
            Duration::from_secs(5)
        ),
        "Manager"
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        wait_for_text_in_session(home, "source", "#status", "Ready", Duration::from_secs(5)),
        "Ready"
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        wait_for_text_in_session(home, "target", "#status", "Pending", Duration::from_secs(5)),
        "Pending"
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
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
        &rub_cmd(home)
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

    let first = wait_for_orchestration_rule_result(home, "manager", rule_id, "armed", "fired");
    let first_rule = first["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("reactive repeat rule should exist");
    let source_tab_target_id = first_rule["source"]["tab_target_id"]
        .as_str()
        .expect("reactive repeat rule should expose source.tab_target_id")
        .to_string();
    assert_eq!(first_rule["last_result"]["status"], "fired", "{first}");
    assert_eq!(first_rule["last_result"]["next_status"], "armed", "{first}");
    let first_cooldown_until_ms = first_rule["execution_policy"]["cooldown_until_ms"]
        .as_u64()
        .expect("reactive repeat first fire should publish cooldown_until_ms");
    assert!(
        first_rule["execution_policy"]["cooldown_until_ms"].is_u64(),
        "{first}"
    );

    let target_first = parse_json(
        &rub_cmd(home)
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

    let cooled = wait_for_orchestration_cooldown_to_expire(home, "manager", rule_id);
    let cooled_rule = cooled["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("reactive repeat rule should still exist after cooldown");
    assert_eq!(cooled_rule["status"], "armed", "{cooled}");
    assert_eq!(cooled_rule["last_result"]["status"], "fired", "{cooled}");

    let target_after_cooldown = parse_json(
        &rub_cmd(home)
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
        &rub_cmd(home)
            .args(["--session", "source", "click", "--selector", "#set-waiting"])
            .output()
            .unwrap(),
    );
    assert_eq!(clear_source["success"], true, "{clear_source}");
    assert_eq!(
        wait_for_text_in_session(home, "source", "#status", "Waiting", Duration::from_secs(5)),
        "Waiting"
    );
    let latched = wait_for_orchestration_condition_evidence_summary(
        home,
        "manager",
        rule_id,
        "armed",
        Some("source_tab_text_present:Ready"),
    );
    assert_eq!(latched["success"], true, "{latched}");
    let latched_rule = latched["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("reactive repeat rule should still exist while preserving latched evidence");
    assert_eq!(latched_rule["status"], "armed", "{latched}");
    assert_eq!(
        latched_rule["last_condition_evidence"]["summary"], "source_tab_text_present:Ready",
        "{latched}"
    );
    let source_probe_cleared = wait_for_orchestration_probe_match(
        &rt,
        home,
        &source_session_id,
        &source_tab_target_id,
        None,
        json!({
            "kind": "text_present",
            "text": "Ready",
        }),
        false,
        Duration::from_secs(5),
    );
    assert_eq!(
        source_probe_cleared
            .data
            .as_ref()
            .and_then(|data| data["matched"].as_bool()),
        Some(false),
        "{source_probe_cleared:?}"
    );
    let cleared =
        wait_for_orchestration_condition_evidence_summary(home, "manager", rule_id, "armed", None);
    let cleared_rule = cleared["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("reactive repeat rule should remain present after clearing latched evidence");
    assert!(
        cleared_rule["last_condition_evidence"].is_null(),
        "{cleared}"
    );

    let rearm_source = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "click", "--selector", "#set-ready"])
            .output()
            .unwrap(),
    );
    assert_eq!(rearm_source["success"], true, "{rearm_source}");
    assert_eq!(
        wait_for_text_in_session(home, "source", "#status", "Ready", Duration::from_secs(5)),
        "Ready"
    );
    let refired =
        wait_for_orchestration_cooldown_to_renew(home, "manager", rule_id, first_cooldown_until_ms);
    let refired_rule = refired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("reactive repeat rule should remain present after re-fire");
    assert_eq!(refired_rule["last_result"]["status"], "fired", "{refired}");

    let second_applied = wait_for_text_in_session(
        home,
        "target",
        "#status",
        "Applied:2",
        Duration::from_secs(8),
    );
    assert_eq!(second_applied, "Applied:2");

    let trace = parse_json(
        &rub_cmd(home)
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

    teardown_and_cleanup(home);
}

/// T437t/T437u: orchestration should truthfully distinguish target-tab continuity loss from
/// target takeover automation fences within one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437t_u_orchestration_target_availability_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
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
  <div id="status">Waiting</div>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/orchestration-target-unavailable");
    let source_url = server.url_for("/orchestration-source-unavailable");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
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
        &rub_cmd(home)
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
        &rub_cmd(home)
            .args(["--session", "target", "close"])
            .output()
            .unwrap(),
    );
    assert_eq!(closed["success"], true, "{closed}");

    let listed = wait_for_orchestration_rule_result(home, "source", rule_id, "armed", "degraded");
    assert_eq!(
        listed["data"]["runtime"]["active_rule_count"], 0,
        "{listed}"
    );
    assert_eq!(
        listed["data"]["runtime"]["unavailable_rule_count"], 1,
        "{listed}"
    );
    assert_eq!(
        listed["data"]["runtime"]["groups"][0]["unavailable_rule_count"], 1,
        "{listed}"
    );
    let rule = listed["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(rule_id))
        .expect("degraded orchestration rule should remain in runtime projection");
    assert_eq!(rule["status"], "armed", "{listed}");
    assert_eq!(rule["last_result"]["status"], "degraded", "{listed}");
    assert_eq!(
        rule["last_result"]["error_code"], "SESSION_BUSY",
        "{listed}"
    );
    assert_eq!(
        rule["last_result"]["reason"], "orchestration_remote_error_response",
        "{listed}"
    );
    assert_eq!(
        rule["last_result"]["error_context"]["remote_reason"],
        "session_shutting_down_after_queue_wait",
        "{listed}"
    );
    assert_eq!(
        rule["last_result"]["error_context"]["remote_context"]["reason"],
        "session_shutting_down_after_queue_wait",
        "{listed}"
    );
    assert_eq!(
        rule["unavailable_reason"], "target_session_missing",
        "{listed}"
    );

    let trace = parse_json(
        &rub_cmd(home)
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
                event["kind"] == "degraded"
                    && event["rule_id"].as_u64() == Some(rule_id)
                    && event["error_code"] == "SESSION_BUSY"
                    && event["reason"] == "orchestration_remote_error_response"
                    && event["error_context"]["remote_reason"]
                        == "session_shutting_down_after_queue_wait"
            }),
        "{trace}"
    );
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
        &rub_cmd(home)
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

    let target_takeover_url = server.url_for("/orchestration-target-takeover");
    let source_takeover_url = server.url_for("/orchestration-source-takeover");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_takeover_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args([
                    "--headed",
                    "--session",
                    "target2",
                    "open",
                    &target_takeover_url
                ])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target2");

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
        &rub_cmd(home)
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
        &rub_cmd(home)
            .args(["--headed", "--session", "target2", "takeover", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(takeover["success"], true, "{takeover}");
    assert_eq!(
        takeover["data"]["runtime"]["status"], "active",
        "{takeover}"
    );
    assert_eq!(
        takeover["data"]["runtime"]["automation_paused"], true,
        "{takeover}"
    );

    let source_ready = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "source",
                "exec",
                "document.getElementById('status').textContent = 'Ready'; 'ok';",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(source_ready["success"], true, "{source_ready}");

    let executed = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "orchestration", "execute", &rule_id])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(
        executed["data"]["result"]["execution"]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["error_code"], "AUTOMATION_PAUSED",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["rule"]["status"], "armed",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["steps"][0]["status"], "blocked",
        "{executed}"
    );
    assert_eq!(
        executed["data"]["result"]["execution"]["steps"][0]["error_code"], "AUTOMATION_PAUSED",
        "{executed}"
    );

    let inspected = parse_json(
        &rub_cmd(home)
            .args([
                "--headed",
                "--session",
                "target2",
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

    teardown_and_cleanup(home);
}

/// T437v/T437z: reactive cross-session orchestration should preserve both remote source/target
/// routing and explicit frame routing within one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437v_z_reactive_orchestration_remote_and_frame_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
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

    let manager_url = server.url_for("/orchestration-manager-reactive");
    let source_url = server.url_for("/orchestration-source-reactive");
    let target_url = server.url_for("/orchestration-target-reactive");

    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let source_session_id = session_id_by_name(&sessions, "source");
    let target_session_id = session_id_by_name(&sessions, "target");

    let remote_spec_path = format!("{home}/orchestration-reactive-remote.json");
    std::fs::write(
        &remote_spec_path,
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

    let remote_added = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &remote_spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(remote_added["success"], true, "{remote_added}");
    let remote_rule_id = remote_added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration rule id should be present");

    let remote_listed = wait_for_orchestration_status(home, "manager", remote_rule_id, "fired");
    assert_eq!(
        remote_listed["data"]["runtime"]["last_rule_id"], remote_rule_id,
        "{remote_listed}"
    );
    assert_eq!(
        remote_listed["data"]["runtime"]["last_rule_result"]["status"], "fired",
        "{remote_listed}"
    );
    assert_eq!(
        remote_listed["data"]["result"]["items"][0]["last_condition_evidence"]["summary"],
        "source_tab_text_present:Ready",
        "{remote_listed}"
    );

    let remote_inspected = parse_json(
        &rub_cmd(home)
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
    assert_eq!(remote_inspected["success"], true, "{remote_inspected}");
    assert_eq!(
        remote_inspected["data"]["result"]["value"], "Applied",
        "{remote_inspected}"
    );

    let remote_trace = parse_json(
        &rub_cmd(home)
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
    assert_eq!(remote_trace["success"], true, "{remote_trace}");
    assert!(
        remote_trace["data"]["result"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "fired"
                    && event["rule_id"].as_u64() == Some(remote_rule_id)
                    && event["evidence"]["summary"] == "source_tab_text_present:Ready"
            }),
        "{remote_trace}"
    );

    let manager_frames_url = server.url_for("/orchestration-manager-reactive-frames");
    let source_frames_url = server.url_for("/orchestration-source-reactive-frames");
    let target_frames_url = server.url_for("/orchestration-target-reactive-frames");
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "manager", "open", &manager_frames_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_frames_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_frames_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let source_frames = parse_json(
        &rub_cmd(home)
            .args(["--session", "source", "frames"])
            .output()
            .unwrap(),
    );
    assert_eq!(source_frames["success"], true, "{source_frames}");
    let source_frame_id = frame_id_by_name(&source_frames, "source-frame");

    let target_frames = parse_json(
        &rub_cmd(home)
            .args(["--session", "target", "frames"])
            .output()
            .unwrap(),
    );
    assert_eq!(target_frames["success"], true, "{target_frames}");
    let target_frame_id = frame_id_by_name(&target_frames, "target-frame");

    let frames_spec_path = format!("{home}/orchestration-reactive-frames.json");
    std::fs::write(
        &frames_spec_path,
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
        &rub_cmd(home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &frames_spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let rule_id = added["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("reactive orchestration frame rule id should be present");

    let fired = wait_for_orchestration_status(home, "manager", rule_id, "fired");
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
        &rub_cmd(home)
            .args(["--session", "target", "frame", "--name", "target-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        switch_target_frame["success"], true,
        "{switch_target_frame}"
    );

    let inspected = parse_json(
        &rub_cmd(home)
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

    teardown_and_cleanup(home);
}

/// T437w/T437x: reactive cross-session orchestration workflow source-vars success and blocked
/// paths should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437w_x_reactive_orchestration_workflow_source_vars_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
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
            &rub_cmd(home)
                .args(["--session", "manager", "open", &manager_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "open", &source_url])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(home)
                .args(["--session", "target", "open", &target_url])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
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
        &rub_cmd(home)
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

    let fired = wait_for_orchestration_status(home, "manager", rule_id, "fired");
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
        rule["last_result"]["steps"][0]["action"]["workflow_path_state"]["path_authority"],
        "automation.action.workflow_path",
        "{fired}"
    );
    assert_eq!(
        rule["last_result"]["steps"][0]["action"]["workflow_path_state"]["upstream_truth"],
        "orchestration_action_payload.workflow_name",
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
        &rub_cmd(home)
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

    let reopened_target = parse_json(
        &rub_cmd(home)
            .args(["--session", "target", "open", &target_url])
            .output()
            .unwrap(),
    );
    assert_eq!(reopened_target["success"], true, "{reopened_target}");

    let blocked_workflow_path =
        PathBuf::from(&home).join("workflows/orchestration_reply_missing_source_var.json");
    std::fs::write(
        &blocked_workflow_path,
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

    let blocked_spec_path = format!("{home}/orchestration-reactive-workflow-vars-blocked.json");
    std::fs::write(
        &blocked_spec_path,
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

    let added_blocked = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                "manager",
                "orchestration",
                "add",
                "--file",
                &blocked_spec_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added_blocked["success"], true, "{added_blocked}");
    let blocked_rule_id = added_blocked["data"]["result"]["rule"]["id"]
        .as_u64()
        .expect("blocked reactive orchestration rule id should be present");

    let blocked =
        wait_for_orchestration_rule_result(home, "manager", blocked_rule_id, "armed", "blocked");
    let blocked_rule = blocked["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(blocked_rule_id))
        .expect("blocked orchestration rule should remain in runtime projection");
    assert_eq!(blocked_rule["status"], "armed", "{blocked}");
    assert_eq!(
        blocked_rule["last_result"]["status"], "blocked",
        "{blocked}"
    );
    assert_eq!(
        blocked_rule["last_result"]["error_code"], "ELEMENT_NOT_FOUND",
        "{blocked}"
    );
    assert_eq!(
        blocked_rule["last_result"]["steps"][0]["action"]["kind"], "workflow",
        "{blocked}"
    );
    assert_eq!(
        blocked_rule["last_result"]["steps"][0]["action"]["workflow_name"],
        "orchestration_reply_missing_source_var",
        "{blocked}"
    );
    assert_eq!(
        blocked_rule["last_result"]["steps"][0]["action"]["source_vars"],
        json!(["reply_name"]),
        "{blocked}"
    );

    let blocked_target = parse_json(
        &rub_cmd(home)
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
    assert_eq!(blocked_target["success"], true, "{blocked_target}");
    assert_eq!(
        blocked_target["data"]["result"]["value"], "Pending",
        "{blocked_target}"
    );

    teardown_and_cleanup(home);
}
