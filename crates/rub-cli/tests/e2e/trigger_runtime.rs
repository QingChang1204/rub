use super::*;

/// T437: same-session cross-tab triggers should fire a canonical action on the bound target tab.
#[test]
#[ignore]
#[serial]
fn t437_trigger_text_present_fires_cross_tab_click() {
    let home = unique_home();
    prepare_home(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Target Page</title></head>
<body>
  <button id="continue" onclick="
    document.body.dataset.triggered = 'yes';
    document.getElementById('result').textContent = 'Triggered';
  ">Continue</button>
  <div id="result">Pending</div>
</body>
</html>"#,
        ),
        (
            "/trigger-source",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Source Page</title></head>
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
    ]);
    let target_url = server.url_for("/trigger-target");
    let source_url = server.url_for("/trigger-source");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &target_url]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let tabs = wait_for_tabs_count(&home, 2);
    let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
    let target_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Target Page"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("target tab should exist")
        .to_string();
    let source_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Source Page"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("source tab should exist")
        .to_string();

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let spec_path = format!("{home}/trigger-fire.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {"kind": "text_present", "text": "Ready"},
            "action": {
                "kind": "browser_command",
                "command": "click",
                "payload": {"selector": "#continue"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let added = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"]
        .as_u64()
        .expect("trigger id should be present");

    let fired = wait_for_trigger_status(&home, trigger_id, "fired");
    let trigger = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("fired trigger should remain in runtime projection");
    assert_eq!(
        trigger["last_condition_evidence"]["summary"], "source_tab_text_present:Ready",
        "{fired}"
    );
    assert_eq!(trigger["last_action_result"]["status"], "fired", "{fired}");
    assert_eq!(
        trigger["last_action_result"]["result"]["interaction"]["semantic_class"], "activate",
        "{fired}"
    );
    assert_eq!(
        fired["data"]["runtime"]["last_trigger_result"]["status"], "fired",
        "{fired}"
    );

    let tabs_after = parse_json(&rub_cmd(&home).arg("tabs").output().unwrap());
    let active = tabs_after["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["active"] == true)
        .expect("one tab should remain active after firing");
    assert_eq!(active["title"], "Trigger Target Page", "{tabs_after}");

    let inspected = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "text", "--selector", "#result"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Triggered",
        "{inspected}"
    );

    cleanup(&home);
}

/// T437b: blocked target actions should leave explainable trigger evidence and result projection.
#[test]
#[ignore]
#[serial]
fn t437b_trigger_records_blocked_outcome_when_target_action_fails() {
    let home = unique_home();
    prepare_home(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-missing",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Missing Target</title></head>
<body>
  <div id="result">Pending</div>
</body>
</html>"#,
        ),
        (
            "/trigger-source-blocked",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Source Blocked</title></head>
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
    ]);

    let target_url = server.url_for("/trigger-target-missing");
    let source_url = server.url_for("/trigger-source-blocked");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &target_url]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let tabs = wait_for_tabs_count(&home, 2);
    let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
    let target_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Missing Target"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("target tab should exist")
        .to_string();
    let source_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Source Blocked"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("source tab should exist")
        .to_string();

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let spec_path = format!("{home}/trigger-blocked.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "action": {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#continue"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"]
        .as_u64()
        .expect("trigger id should be present");

    let blocked = wait_for_trigger_status(&home, trigger_id, "blocked");
    let trigger = blocked["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("blocked trigger should remain in runtime projection");
    assert_eq!(
        trigger["last_condition_evidence"]["summary"], "source_tab_text_present:Ready",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["status"], "blocked",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["kind"], "browser_command",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["command"], "click",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["error_code"], "ELEMENT_NOT_FOUND",
        "{blocked}"
    );
    assert!(
        trigger["last_action_result"]["reason"].is_null(),
        "{blocked}"
    );
    assert!(
        trigger["last_action_result"]["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("ELEMENT_NOT_FOUND"),
        "{blocked}"
    );
    assert_eq!(
        blocked["data"]["runtime"]["last_trigger_result"]["status"], "blocked",
        "{blocked}"
    );

    cleanup(&home);
}

/// T437c: resuming a paused network trigger should ignore stale request evidence and only fire on a new request.
#[test]
#[ignore]
#[serial]
fn t437c_trigger_resume_ignores_stale_network_evidence_and_fires_on_new_request() {
    let home = unique_home();
    prepare_home(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-network",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Network Target</title></head>
<body>
  <button id="continue" onclick="
    window.__count = (window.__count || 0) + 1;
    document.getElementById('count').textContent = String(window.__count);
  ">Continue</button>
  <div id="count">0</div>
</body>
</html>"#,
        ),
        (
            "/trigger-source-network",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Network Source</title></head>
<body>
  <div id="status">idle</div>
  <script>
    window.fireNextTrigger = () =>
      fetch('/api/trigger-next')
        .then(() => { document.getElementById('status').textContent = 'next'; });
    setTimeout(() => {
      fetch('/api/trigger-initial')
        .then(() => { document.getElementById('status').textContent = 'initial'; });
    }, 900);
  </script>
</body>
</html>"#,
        ),
        (
            "/api/trigger-initial",
            "application/json",
            r#"{"phase":"initial"}"#,
        ),
        (
            "/api/trigger-next",
            "application/json",
            r#"{"phase":"next"}"#,
        ),
    ]);

    let target_url = server.url_for("/trigger-target-network");
    let source_url = server.url_for("/trigger-source-network");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &target_url]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let tabs = wait_for_tabs_count(&home, 2);
    let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
    let target_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Network Target"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("target tab should exist")
        .to_string();
    let source_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Network Source"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("source tab should exist")
        .to_string();

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let spec_path = format!("{home}/trigger-network.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {
                "kind": "network_request",
                "url_pattern": "/api/trigger-",
                "method": "GET",
                "status_code": 200
            },
            "action": {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#continue"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"]
        .as_u64()
        .expect("trigger id should be present");

    let paused = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "pause", &trigger_id.to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(paused["success"], true, "{paused}");
    assert_eq!(
        paused["data"]["result"]["trigger"]["status"], "paused",
        "{paused}"
    );

    let waited = parse_json(
        &rub_cmd(&home)
            .args([
                "inspect",
                "network",
                "--wait",
                "--match",
                "/api/trigger-initial",
                "--method",
                "GET",
                "--lifecycle",
                "terminal",
                "--timeout",
                "10000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");
    assert_eq!(waited["data"]["result"]["matched"], true, "{waited}");

    let switched_target = parse_json(
        &rub_cmd(&home)
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let count_before = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "text", "--selector", "#count"])
            .output()
            .unwrap(),
    );
    assert_eq!(count_before["success"], true, "{count_before}");
    assert_eq!(
        count_before["data"]["result"]["value"], "0",
        "{count_before}"
    );

    let switched_source = parse_json(
        &rub_cmd(&home)
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_source["success"], true, "{switched_source}");

    let resumed = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "resume", &trigger_id.to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(
        resumed["data"]["result"]["trigger"]["status"], "armed",
        "{resumed}"
    );

    std::thread::sleep(Duration::from_millis(1200));
    let still_armed = parse_json(&rub_cmd(&home).args(["trigger", "list"]).output().unwrap());
    let trigger = still_armed["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("trigger should remain present");
    assert_eq!(trigger["status"], "armed", "{still_armed}");

    let refired = parse_json(
        &rub_cmd(&home)
            .args(["exec", "window.fireNextTrigger(); null"])
            .output()
            .unwrap(),
    );
    assert_eq!(refired["success"], true, "{refired}");

    let fired = wait_for_trigger_status(&home, trigger_id, "fired");
    let fired_trigger = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("fired trigger should remain in runtime projection");
    assert!(
        fired_trigger["last_condition_evidence"]["summary"]
            .as_str()
            .unwrap_or_default()
            .starts_with("network_request_matched:"),
        "{fired}"
    );
    assert_eq!(
        fired_trigger["last_action_result"]["status"], "fired",
        "{fired}"
    );

    let switched_target_after = parse_json(
        &rub_cmd(&home)
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(
        switched_target_after["success"], true,
        "{switched_target_after}"
    );
    let count_after = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "text", "--selector", "#count"])
            .output()
            .unwrap(),
    );
    assert_eq!(count_after["success"], true, "{count_after}");
    assert_eq!(count_after["data"]["result"]["value"], "1", "{count_after}");

    cleanup(&home);
}

/// T437d: missing target tabs should degrade the trigger projection instead of firing silently.
#[test]
#[ignore]
#[serial]
fn t437d_trigger_reports_target_missing_and_does_not_fire() {
    let home = unique_home();
    prepare_home(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-gone",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Target Gone</title></head>
<body>
  <button id="continue" onclick="document.body.dataset.triggered='yes'">Continue</button>
</body>
</html>"#,
        ),
        (
            "/trigger-source-gone",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Source Gone</title></head>
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
    ]);

    let target_url = server.url_for("/trigger-target-gone");
    let source_url = server.url_for("/trigger-source-gone");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &target_url]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let tabs = wait_for_tabs_count(&home, 2);
    let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
    let target_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Target Gone"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("target tab should exist")
        .to_string();
    let source_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Source Gone"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("source tab should exist")
        .to_string();

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let spec_path = format!("{home}/trigger-target-missing.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "action": {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#continue"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"]
        .as_u64()
        .expect("trigger id should be present");

    let switched_target = parse_json(
        &rub_cmd(&home)
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");

    let closed = parse_json(&rub_cmd(&home).args(["close-tab"]).output().unwrap());
    assert_eq!(closed["success"], true, "{closed}");
    assert_eq!(closed["data"]["result"]["remaining_tabs"], 1, "{closed}");
    assert_eq!(
        closed["data"]["result"]["active_tab"]["title"], "Trigger Source Gone",
        "{closed}"
    );

    let degraded = wait_for_trigger_unavailable_reason(&home, trigger_id, "target_tab_missing");
    assert_eq!(
        degraded["data"]["runtime"]["status"], "degraded",
        "{degraded}"
    );
    assert_eq!(
        degraded["data"]["runtime"]["degraded_count"], 1,
        "{degraded}"
    );
    let trigger = degraded["data"]["runtime"]["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("trigger should remain in runtime projection");
    assert_eq!(trigger["status"], "armed", "{degraded}");
    assert_eq!(
        trigger["unavailable_reason"], "target_tab_missing",
        "{degraded}"
    );

    std::thread::sleep(Duration::from_millis(1500));
    let still_degraded = parse_json(&rub_cmd(&home).args(["trigger", "list"]).output().unwrap());
    let trigger = still_degraded["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("trigger should remain present");
    assert_eq!(trigger["status"], "armed", "{still_degraded}");
    assert_eq!(
        trigger["unavailable_reason"], "target_tab_missing",
        "{still_degraded}"
    );
    assert!(trigger["last_action_result"].is_null(), "{still_degraded}");

    cleanup(&home);
}

/// T437e: `trigger trace` should expose recent trigger lifecycle and outcome events.
#[test]
#[ignore]
#[serial]
fn t437e_trigger_trace_projects_recent_lifecycle_and_outcome_events() {
    let home = unique_home();
    prepare_home(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-trace",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Trace Target</title></head>
<body>
  <button id="continue" onclick="document.getElementById('result').textContent='Triggered'">Continue</button>
  <div id="result">Pending</div>
</body>
</html>"#,
        ),
        (
            "/trigger-source-trace",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Trace Source</title></head>
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
    ]);

    let target_url = server.url_for("/trigger-target-trace");
    let source_url = server.url_for("/trigger-source-trace");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &target_url]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let tabs = wait_for_tabs_count(&home, 2);
    let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
    let target_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Trace Target"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("target tab should exist")
        .to_string();
    let source_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Trace Source"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("source tab should exist")
        .to_string();

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let spec_path = format!("{home}/trigger-trace.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "action": {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#continue"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"]
        .as_u64()
        .expect("trigger id should be present");

    let fired = wait_for_trigger_status(&home, trigger_id, "fired");
    assert_eq!(
        fired["data"]["runtime"]["last_trigger_result"]["status"], "fired",
        "{fired}"
    );

    let trace = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "trace", "--last", "5"])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    let events = trace["data"]["result"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{trace}");
    assert_eq!(events[0]["kind"], "registered", "{trace}");
    assert_eq!(events[0]["trigger_id"], trigger_id, "{trace}");
    assert_eq!(events[1]["kind"], "fired", "{trace}");
    assert_eq!(events[1]["trigger_id"], trigger_id, "{trace}");
    assert_eq!(events[1]["result"]["status"], "fired", "{trace}");
    assert_eq!(
        trace["data"]["runtime"]["last_trigger_result"]["status"], "fired",
        "{trace}"
    );

    cleanup(&home);
}

/// T437f: stale selected-frame continuity failures should degrade the trigger instead of acting on the target tab.
#[test]
#[ignore]
#[serial]
fn t437f_trigger_degrades_when_target_selected_frame_becomes_stale() {
    let home = unique_home();
    prepare_home(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-stale-frame",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Target Stale Frame</title></head>
<body>
  <button id="continue" onclick="
    document.body.dataset.triggered = 'yes';
    document.getElementById('result').textContent = 'Triggered';
  ">Continue</button>
  <div id="result">Pending</div>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/trigger-target-stale-frame-child"
    title="Child Frame"
  ></iframe>
  <script>
    setTimeout(() => {
      document.getElementById('child-frame')?.remove();
    }, 500);
  </script>
</body>
</html>"#,
        ),
        (
            "/trigger-target-stale-frame-child",
            "text/html",
            r#"<!DOCTYPE html><html><body><button>Inside Frame</button></body></html>"#,
        ),
        (
            "/trigger-source-stale-frame",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Trigger Source Stale Frame</title></head>
<body>
  <div id="status">Waiting</div>
  <script>
    setTimeout(() => {
      document.getElementById('status').textContent = 'Ready';
    }, 2000);
  </script>
</body>
</html>"#,
        ),
    ]);

    let target_url = server.url_for("/trigger-target-stale-frame");
    let source_url = server.url_for("/trigger-source-stale-frame");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &target_url]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let tabs = wait_for_tabs_count(&home, 2);
    let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
    let target_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Target Stale Frame"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("target tab should exist")
        .to_string();
    let source_index = tabs
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Trigger Source Stale Frame"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("source tab should exist")
        .to_string();

    let switched_target = parse_json(
        &rub_cmd(&home)
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");

    let selected = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(selected["success"], true, "{selected}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args([
                "exec",
                "new Promise((resolve) => setTimeout(() => resolve('done'), 900))",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let frame_runtime = parse_json(&rub_cmd(&home).args(["runtime", "frame"]).output().unwrap());
    assert_eq!(frame_runtime["success"], true, "{frame_runtime}");
    assert_eq!(
        frame_runtime["data"]["runtime"]["status"], "stale",
        "{frame_runtime}"
    );
    assert_eq!(
        frame_runtime["data"]["runtime"]["degraded_reason"], "selected_frame_not_found",
        "{frame_runtime}"
    );

    let spec_path = format!("{home}/trigger-stale-frame.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {
                "kind": "text_present",
                "text": "Ready"
            },
            "action": {
                "kind": "browser_command",
                "command": "click",
                "payload": {
                    "selector": "#continue"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let added = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"]
        .as_u64()
        .expect("trigger id should be present");

    let degraded = wait_for_trigger_status(&home, trigger_id, "degraded");
    let trigger = degraded["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .expect("degraded trigger should remain in runtime projection");
    assert_eq!(
        trigger["last_condition_evidence"]["summary"], "source_tab_text_present:Ready",
        "{degraded}"
    );
    assert_eq!(
        trigger["last_action_result"]["status"], "degraded",
        "{degraded}"
    );
    assert_eq!(
        trigger["last_action_result"]["error_code"], "BROWSER_CRASHED",
        "{degraded}"
    );
    assert_eq!(
        trigger["last_action_result"]["reason"], "continuity_frame_unavailable",
        "{degraded}"
    );
    assert!(
        trigger["last_action_result"]["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("frame context became unavailable"),
        "{degraded}"
    );
    assert_eq!(
        degraded["data"]["runtime"]["last_trigger_result"]["status"], "degraded",
        "{degraded}"
    );

    let trace = parse_json(
        &rub_cmd(&home)
            .args(["trigger", "trace", "--last", "5"])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    let events = trace["data"]["result"]["events"]
        .as_array()
        .expect("trigger trace should expose events");
    assert!(
        events.iter().any(|event| {
            event["kind"] == "degraded"
                && event["trigger_id"].as_u64() == Some(trigger_id)
                && event["result"]["reason"] == "continuity_frame_unavailable"
        }),
        "{trace}"
    );

    let tabs_after = parse_json(&rub_cmd(&home).arg("tabs").output().unwrap());
    let active = tabs_after["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["active"] == true)
        .expect("one tab should remain active after degraded firing");
    assert_eq!(
        active["title"], "Trigger Target Stale Frame",
        "{tabs_after}"
    );

    let reset_frame = parse_json(&rub_cmd(&home).args(["frame", "--top"]).output().unwrap());
    assert_eq!(reset_frame["success"], true, "{reset_frame}");

    let inspected = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "text", "--selector", "#result"])
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

/// T437g-T437h: workflow-backed trigger flows should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437g_h_trigger_workflow_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home().to_string();
    std::fs::create_dir_all(PathBuf::from(&home).join("workflows")).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-workflow",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source</title></head><body><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1200);</script></body></html>"#,
        ),
        (
            "/trigger-target-workflow-vars",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Vars Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-vars",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Vars Source</title></head><body><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1200);</script></body></html>"#,
        ),
        (
            "/trigger-target-workflow-source-vars",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-source-vars",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Source</title></head><body><div id="question">Answer from source tab</div><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1200);</script></body></html>"#,
        ),
        (
            "/trigger-target-workflow-storage",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Storage Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-storage",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Storage Source</title></head><body><div id="status">Waiting</div><script>localStorage.removeItem('reply_state');</script></body></html>"#,
        ),
        (
            "/trigger-target-workflow-source-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Blocked Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-source-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Blocked Source</title></head><body><div id="status">Waiting</div></body></html>"#,
        ),
        (
            "/trigger-target-removed",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Removed Target</title></head><body><button id="continue" onclick="document.body.dataset.triggered='yes';document.getElementById('result').textContent='Triggered';">Continue</button><div id="result">Pending</div></body></html>"#,
        ),
        (
            "/trigger-source-removed",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Removed Source</title></head><body><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1800);</script></body></html>"#,
        ),
    ]);

    let write_json_file = |name: &str, value: serde_json::Value| -> String {
        let path = PathBuf::from(&home).join(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        path.to_string_lossy().into_owned()
    };

    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "Ada from trigger", "clear": true}},
            {"command": "click", "args": {"selector": "#apply", "wait_after": {"text": "Ada from trigger", "timeout_ms": 5000}}}
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_with_vars.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "{{reply_name}}", "clear": true}},
            {"command": "click", "args": {"selector": "#apply", "wait_after": {"text": "{{reply_name}}", "timeout_ms": 5000}}}
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_with_source_vars.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "{{reply_name}}", "clear": true}},
            {"command": "click", "args": {"selector": "#apply", "wait_after": {"text": "{{reply_name}}", "timeout_ms": 5000}}}
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_from_storage.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "Storage triggered", "clear": true}},
            {"command": "click", "args": {"selector": "#apply", "wait_after": {"text": "Storage triggered", "timeout_ms": 5000}}}
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_missing_source_var.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "{{reply_name}}", "clear": true}}
        ]))
        .unwrap(),
    )
    .unwrap();

    let mut expected_tabs = 0_u64;
    let mut bootstrap = true;
    let open_pair = |expected_tabs_ref: &mut u64,
                     bootstrap_ref: &mut bool,
                     target_url: &str,
                     source_url: &str,
                     target_title: &str,
                     source_title: &str|
     -> (String, String) {
        if *bootstrap_ref {
            let opened = parse_json(&session.cmd().args(["open", target_url]).output().unwrap());
            assert_eq!(opened["success"], true, "{opened}");
            let source_opened = parse_json(
                &session
                    .cmd()
                    .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                    .output()
                    .unwrap(),
            );
            assert_eq!(source_opened["success"], true, "{source_opened}");
            *bootstrap_ref = false;
        } else {
            let opened = parse_json(
                &session
                    .cmd()
                    .args([
                        "exec",
                        &format!(
                            "window.open('{target_url}', '_blank'); window.open('{source_url}', '_blank'); null"
                        ),
                    ])
                    .output()
                    .unwrap(),
            );
            assert_eq!(opened["success"], true, "{opened}");
        }
        *expected_tabs_ref += 2;
        let tabs = wait_for_tabs_count(&home, *expected_tabs_ref);
        let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
        let target_index = tabs
            .iter()
            .find(|tab| tab["title"].as_str() == Some(target_title))
            .and_then(|tab| tab["index"].as_u64())
            .expect("target tab should exist")
            .to_string();
        let source_index = tabs
            .iter()
            .find(|tab| tab["title"].as_str() == Some(source_title))
            .and_then(|tab| tab["index"].as_u64())
            .expect("source tab should exist")
            .to_string();
        (target_index, source_index)
    };
    let _reset_tabs = |expected_tabs_ref: &mut u64, bootstrap_ref: &mut bool| {
        let closed = parse_json(&session.cmd().args(["close", "--all"]).output().unwrap());
        assert_eq!(closed["success"], true, "{closed}");
        *expected_tabs_ref = 0;
        *bootstrap_ref = true;
    };

    let (target_index, source_index) = open_pair(
        &mut expected_tabs,
        &mut bootstrap,
        &server.url_for("/trigger-target-workflow"),
        &server.url_for("/trigger-source-workflow"),
        "Trigger Workflow Target",
        "Trigger Workflow Source",
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let spec_path = write_json_file(
        "trigger-workflow.json",
        json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {"kind": "text_present", "text": "Ready"},
            "action": {"kind": "workflow", "payload": {"workflow_name": "reply_flow"}}
        }),
    );
    let added = parse_json(
        &session
            .cmd()
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"].as_u64().unwrap();
    let mut fired = serde_json::Value::Null;
    for _ in 0..120 {
        let out = parse_json(&session.cmd().args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .find(|item| item["id"].as_u64() == Some(trigger_id))
            })
            && trigger["status"].as_str() == Some("fired")
        {
            fired = out;
            break;
        }
        fired = out;
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_ne!(fired, serde_json::Value::Null);
    let trigger = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .unwrap();
    assert_eq!(trigger["status"], "fired", "{fired}");
    assert_eq!(trigger["last_action_result"]["status"], "fired", "{fired}");
    assert!(
        trigger["last_action_result"]["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("workflow 'reply_flow'"),
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["workflow_path_state"]["path_authority"],
        "automation.action.workflow_path",
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["workflow_path_state"]["upstream_truth"],
        "trigger_action_payload.workflow_name",
        "{fired}"
    );
    let switched_target = parse_json(
        &session
            .cmd()
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Ada from trigger",
        "{inspected}"
    );

    let (target_index, source_index) = open_pair(
        &mut expected_tabs,
        &mut bootstrap,
        &server.url_for("/trigger-target-workflow-vars"),
        &server.url_for("/trigger-source-workflow-vars"),
        "Trigger Workflow Vars Target",
        "Trigger Workflow Vars Source",
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let spec_path = write_json_file(
        "trigger-workflow-vars.json",
        json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {"kind": "text_present", "text": "Ready"},
            "action": {"kind": "workflow", "payload": {"workflow_name": "reply_flow_with_vars", "vars": {"reply_name": "Grace from trigger"}}}
        }),
    );
    let added = parse_json(
        &session
            .cmd()
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"].as_u64().unwrap();
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let seeded = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "localStorage.setItem('reply_state','ready'); 'seeded'",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(seeded["success"], true, "{seeded}");
    let mut fired = serde_json::Value::Null;
    for _ in 0..120 {
        let out = parse_json(&session.cmd().args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .find(|item| item["id"].as_u64() == Some(trigger_id))
            })
            && trigger["status"].as_str() == Some("fired")
        {
            fired = out;
            break;
        }
        fired = out;
        std::thread::sleep(Duration::from_millis(100));
    }
    let trigger = fired["data"]["result"]["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        })
        .unwrap_or_else(|| panic!("storage trigger should remain visible: {fired}"));
    assert_eq!(trigger["status"], "fired", "{fired}");
    let trigger = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .unwrap();
    assert_eq!(trigger["last_action_result"]["status"], "fired", "{fired}");
    assert_eq!(
        trigger["last_action_result"]["action"]["kind"], "workflow",
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["workflow_name"], "reply_flow_with_vars",
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["vars"],
        json!(["reply_name"]),
        "{fired}"
    );
    let switched_target = parse_json(
        &session
            .cmd()
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Grace from trigger",
        "{inspected}"
    );
}

/// T437i-T437l: source vars, storage-backed workflows, blocked source vars, and removed-trigger fences should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t437i_l_trigger_source_vars_storage_blocked_and_removed_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home().to_string();
    std::fs::create_dir_all(PathBuf::from(&home).join("workflows")).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/trigger-target-workflow-source-vars",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-source-vars",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Source</title></head><body><div id="question">Answer from source tab</div><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1200);</script></body></html>"#,
        ),
        (
            "/trigger-target-workflow-storage",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Storage Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-storage",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Storage Source</title></head><body><div id="status">Waiting</div><script>localStorage.removeItem('reply_state');</script></body></html>"#,
        ),
        (
            "/trigger-target-workflow-source-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Blocked Target</title></head><body><input id="name" value="" /><button id="apply">Apply</button><div id="status">Pending</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'Pending';});</script></body></html>"#,
        ),
        (
            "/trigger-source-workflow-source-vars-blocked",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Workflow Source Vars Blocked Source</title></head><body><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1200);</script></body></html>"#,
        ),
        (
            "/trigger-target-removed",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Removed Target</title></head><body><button id="continue" onclick="document.body.dataset.triggered='yes';document.getElementById('result').textContent='Triggered';">Continue</button><div id="result">Pending</div></body></html>"#,
        ),
        (
            "/trigger-source-removed",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Trigger Removed Source</title></head><body><div id="status">Waiting</div><script>setTimeout(()=>{document.getElementById('status').textContent='Ready';},1800);</script></body></html>"#,
        ),
    ]);

    let write_json_file = |name: &str, value: serde_json::Value| -> String {
        let path = PathBuf::from(&home).join(name);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        path.to_string_lossy().into_owned()
    };

    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_with_source_vars.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "{{reply_name}}", "clear": true}},
            {"command": "click", "args": {"selector": "#apply", "wait_after": {"text": "{{reply_name}}", "timeout_ms": 5000}}}
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_from_storage.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "Storage triggered", "clear": true}},
            {"command": "click", "args": {"selector": "#apply", "wait_after": {"text": "Storage triggered", "timeout_ms": 5000}}}
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        PathBuf::from(&home).join("workflows/reply_flow_missing_source_var.json"),
        serde_json::to_vec_pretty(&json!([
            {"command": "type", "args": {"selector": "#name", "text": "{{reply_name}}", "clear": true}}
        ]))
        .unwrap(),
    )
    .unwrap();

    let mut expected_tabs = 0_u64;
    let mut bootstrap = true;
    let open_pair = |expected_tabs_ref: &mut u64,
                     bootstrap_ref: &mut bool,
                     target_url: &str,
                     source_url: &str,
                     target_title: &str,
                     source_title: &str|
     -> (String, String) {
        if *bootstrap_ref {
            let opened = parse_json(&session.cmd().args(["open", target_url]).output().unwrap());
            assert_eq!(opened["success"], true, "{opened}");
            let source_opened = parse_json(
                &session
                    .cmd()
                    .args(["exec", &format!("window.open('{source_url}', '_blank')")])
                    .output()
                    .unwrap(),
            );
            assert_eq!(source_opened["success"], true, "{source_opened}");
            *bootstrap_ref = false;
        } else {
            let opened = parse_json(
                &session
                    .cmd()
                    .args([
                        "exec",
                        &format!(
                            "window.open('{target_url}', '_blank'); window.open('{source_url}', '_blank'); null"
                        ),
                    ])
                    .output()
                    .unwrap(),
            );
            assert_eq!(opened["success"], true, "{opened}");
        }
        *expected_tabs_ref += 2;
        let tabs = wait_for_tabs_count(&home, *expected_tabs_ref);
        let tabs = tabs["data"]["result"]["items"].as_array().unwrap();
        let target_index = tabs
            .iter()
            .find(|tab| tab["title"].as_str() == Some(target_title))
            .and_then(|tab| tab["index"].as_u64())
            .expect("target tab should exist")
            .to_string();
        let source_index = tabs
            .iter()
            .find(|tab| tab["title"].as_str() == Some(source_title))
            .and_then(|tab| tab["index"].as_u64())
            .expect("source tab should exist")
            .to_string();
        (target_index, source_index)
    };
    let reset_tabs = |expected_tabs_ref: &mut u64, bootstrap_ref: &mut bool| {
        let closed = parse_json(&session.cmd().args(["close", "--all"]).output().unwrap());
        assert_eq!(closed["success"], true, "{closed}");
        *expected_tabs_ref = 0;
        *bootstrap_ref = true;
    };

    let (target_index, source_index) = open_pair(
        &mut expected_tabs,
        &mut bootstrap,
        &server.url_for("/trigger-target-removed"),
        &server.url_for("/trigger-source-removed"),
        "Trigger Removed Target",
        "Trigger Removed Source",
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let spec_path = write_json_file(
        "trigger-removed.json",
        json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {"kind": "text_present", "text": "Ready"},
            "action": {"kind": "browser_command", "command": "click", "payload": {"selector": "#continue"}}
        }),
    );
    let added = parse_json(
        &session
            .cmd()
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"].as_u64().unwrap();
    let removed = parse_json(
        &session
            .cmd()
            .args(["trigger", "remove", &trigger_id.to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["success"], true, "{removed}");
    assert_eq!(
        removed["data"]["result"]["removed"]["id"], trigger_id,
        "{removed}"
    );
    std::thread::sleep(Duration::from_millis(2600));
    let listed = parse_json(&session.cmd().args(["trigger", "list"]).output().unwrap());
    assert_eq!(listed["success"], true, "{listed}");
    assert!(
        listed["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"].as_u64() != Some(trigger_id)),
        "{listed}"
    );
    assert!(
        listed["data"]["runtime"]["last_trigger_result"].is_null(),
        "{listed}"
    );
    let trace = parse_json(
        &session
            .cmd()
            .args(["trigger", "trace", "--last", "5"])
            .output()
            .unwrap(),
    );
    assert_eq!(trace["success"], true, "{trace}");
    let events = trace["data"]["result"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{trace}");
    assert_eq!(events[0]["kind"], "registered", "{trace}");
    assert_eq!(events[0]["trigger_id"], trigger_id, "{trace}");
    assert_eq!(events[1]["kind"], "removed", "{trace}");
    assert_eq!(events[1]["trigger_id"], trigger_id, "{trace}");
    assert!(
        events
            .iter()
            .all(|event| event["kind"].as_str() != Some("fired")),
        "{trace}"
    );
    let switched_target = parse_json(
        &session
            .cmd()
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#result"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Pending",
        "{inspected}"
    );
    reset_tabs(&mut expected_tabs, &mut bootstrap);

    let (target_index, source_index) = open_pair(
        &mut expected_tabs,
        &mut bootstrap,
        &server.url_for("/trigger-target-workflow-source-vars"),
        &server.url_for("/trigger-source-workflow-source-vars"),
        "Trigger Workflow Source Vars Target",
        "Trigger Workflow Source Vars Source",
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let spec_path = write_json_file(
        "trigger-workflow-source-vars.json",
        json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {"kind": "text_present", "text": "Ready"},
            "action": {
                "kind": "workflow",
                "payload": {
                    "workflow_name": "reply_flow_with_source_vars",
                    "source_vars": {
                        "reply_name": {"kind": "text", "selector": "#question"}
                    }
                }
            }
        }),
    );
    let added = parse_json(
        &session
            .cmd()
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"].as_u64().unwrap();
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let seeded = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "localStorage.setItem('reply_state','ready'); 'seeded'",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(seeded["success"], true, "{seeded}");
    let mut fired = serde_json::Value::Null;
    for _ in 0..120 {
        let out = parse_json(&session.cmd().args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .find(|item| item["id"].as_u64() == Some(trigger_id))
            })
            && trigger["status"].as_str() == Some("fired")
        {
            fired = out;
            break;
        }
        fired = out;
        std::thread::sleep(Duration::from_millis(100));
    }
    let trigger = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .unwrap();
    assert_eq!(trigger["status"], "fired", "{fired}");
    assert_eq!(trigger["last_action_result"]["status"], "fired", "{fired}");
    assert_eq!(
        trigger["last_action_result"]["action"]["kind"], "workflow",
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["workflow_name"], "reply_flow_with_source_vars",
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["source_vars"],
        json!(["reply_name"]),
        "{fired}"
    );
    let switched_target = parse_json(
        &session
            .cmd()
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Answer from source tab",
        "{inspected}"
    );
    reset_tabs(&mut expected_tabs, &mut bootstrap);

    let (target_index, source_index) = open_pair(
        &mut expected_tabs,
        &mut bootstrap,
        &server.url_for("/trigger-target-workflow-storage"),
        &server.url_for("/trigger-source-workflow-storage"),
        "Trigger Workflow Storage Target",
        "Trigger Workflow Storage Source",
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let spec_path = write_json_file(
        "trigger-workflow-storage.json",
        json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {
                "kind": "storage_value",
                "storage_area": "local",
                "key": "reply_state",
                "value": "ready"
            },
            "action": {"kind": "workflow", "payload": {"workflow_name": "reply_flow_from_storage"}}
        }),
    );
    let added = parse_json(
        &session
            .cmd()
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"].as_u64().unwrap();
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let seeded = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "localStorage.setItem('reply_state','ready'); 'seeded'",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(seeded["success"], true, "{seeded}");
    let mut fired = serde_json::Value::Null;
    for _ in 0..120 {
        let out = parse_json(&session.cmd().args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .find(|item| item["id"].as_u64() == Some(trigger_id))
            })
            && trigger["status"].as_str() == Some("fired")
        {
            fired = out;
            break;
        }
        fired = out;
        std::thread::sleep(Duration::from_millis(100));
    }
    let trigger = fired["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .unwrap();
    assert_eq!(trigger["status"], "fired", "{fired}");
    assert_eq!(trigger["last_action_result"]["status"], "fired", "{fired}");
    assert_eq!(
        trigger["last_condition_evidence"]["summary"], "source_tab_storage_matched:reply_state",
        "{fired}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["workflow_name"], "reply_flow_from_storage",
        "{fired}"
    );
    let switched_target = parse_json(
        &session
            .cmd()
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Storage triggered",
        "{inspected}"
    );
    reset_tabs(&mut expected_tabs, &mut bootstrap);

    let (target_index, source_index) = open_pair(
        &mut expected_tabs,
        &mut bootstrap,
        &server.url_for("/trigger-target-workflow-source-vars-blocked"),
        &server.url_for("/trigger-source-workflow-source-vars-blocked"),
        "Trigger Workflow Source Vars Blocked Target",
        "Trigger Workflow Source Vars Blocked Source",
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let spec_path = write_json_file(
        "trigger-workflow-source-vars-blocked.json",
        json!({
            "source_tab": source_index.parse::<u64>().unwrap(),
            "target_tab": target_index.parse::<u64>().unwrap(),
            "mode": "once",
            "condition": {"kind": "text_present", "text": "Ready"},
            "action": {
                "kind": "workflow",
                "payload": {
                    "workflow_name": "reply_flow_missing_source_var",
                    "source_vars": {
                        "reply_name": {"kind": "text", "selector": "#missing-question"}
                    }
                }
            }
        }),
    );
    let added = parse_json(
        &session
            .cmd()
            .args(["trigger", "add", "--file", &spec_path])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    let trigger_id = added["data"]["result"]["trigger"]["id"].as_u64().unwrap();
    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &source_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    let seeded = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "document.getElementById('status').textContent='Ready'; 'seeded'",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(seeded["success"], true, "{seeded}");
    let mut blocked = serde_json::Value::Null;
    for _ in 0..120 {
        let out = parse_json(&session.cmd().args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .find(|item| item["id"].as_u64() == Some(trigger_id))
            })
            && trigger["last_action_result"]["status"].as_str() == Some("blocked")
        {
            blocked = out;
            break;
        }
        blocked = out;
        std::thread::sleep(Duration::from_millis(100));
    }
    let trigger = blocked["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_u64() == Some(trigger_id))
        .unwrap();
    assert_eq!(
        trigger["last_action_result"]["status"], "blocked",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["error_code"], "ELEMENT_NOT_FOUND",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["kind"], "workflow",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["workflow_name"], "reply_flow_missing_source_var",
        "{blocked}"
    );
    assert_eq!(
        trigger["last_action_result"]["action"]["source_vars"],
        json!(["reply_name"]),
        "{blocked}"
    );
    let switched_target = parse_json(
        &session
            .cmd()
            .args(["switch", &target_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_target["success"], true, "{switched_target}");
    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["value"], "Pending",
        "{inspected}"
    );
}
