use super::*;

// ============================================================
// SC-001 + high-frequency integration browser-backed chain
// ============================================================

#[test]
#[ignore]
#[serial]
fn t056_131_standard_flow_and_runtime_contract_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let normal_path = format!("{}/normal.png", session.home());
    let full_path = format!("{}/full.png", session.home());

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Example Domain</title></head>
<body>
  <main>
    <h1>Example Domain</h1>
    <p>This domain is for use in illustrative examples in documents.</p>
    <button id="advance" onclick="document.body.dataset.clicked='yes'">Advance</button>
  </main>
</body>
</html>"#,
        ),
        (
            "/workflow",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input name="custname" value="" />
</body>
</html>"#,
        ),
        (
            "/html",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Scroll Fixture</title></head>
<body>
  <article>
    <h1>Scroll Fixture</h1>
    <p>This page is intentionally tall for scrolling tests.</p>
  </article>
  <div style="height: 2600px"></div>
</body>
</html>"#,
        ),
        ("/status/404", "text/plain", "404 Not Found"),
    ]);

    let workflow_open = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/workflow")])
            .output()
            .unwrap(),
    );
    assert_eq!(workflow_open["success"], true, "Step 1: navigate");

    let state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(state["success"], true, "Step 2: inspect");
    let snap = state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    assert!(
        state["data"]["result"]["snapshot"]["total_count"]
            .as_u64()
            .unwrap()
            > 0
    );

    let typed = parse_json(
        &session
            .cmd()
            .args(["type", "--index", "0", "E2E Test", "--snapshot", snap])
            .output()
            .unwrap(),
    );
    assert_eq!(typed["success"], true, "Step 3: type");

    let exec_result = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "document.querySelector('input[name=custname]').value",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_result["success"], true, "Step 4: exec");
    assert_eq!(exec_result["data"]["result"], "E2E Test");

    let state_again = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(state_again["success"], true, "Step 5: verify state");

    let open_404 = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/status/404")])
            .output()
            .unwrap(),
    );
    assert_eq!(open_404["stdout_schema_version"], "3.0");
    assert_eq!(open_404["command"], "open");

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let state1 = parse_json(&session.cmd().arg("state").output().unwrap());
    let epoch1 = state1["data"]["result"]["snapshot"]["dom_epoch"]
        .as_u64()
        .unwrap_or(0);
    assert!(epoch1 >= 1, "epoch should be >= 1 after open: {epoch1}");

    let exec_side_effect = parse_json(
        &session
            .cmd()
            .args(["exec", "document.title = 'modified'"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_side_effect["success"], true, "exec should succeed");

    let state2 = parse_json(&session.cmd().arg("state").output().unwrap());
    let epoch2 = state2["data"]["result"]["snapshot"]["dom_epoch"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        epoch2 > epoch1,
        "epoch should increment after exec: {epoch1} -> {epoch2}"
    );

    let invalid_js = parse_json(
        &session
            .cmd()
            .args(["exec", "(() => { throw new Error('invalid-js'); })()"])
            .output()
            .unwrap(),
    );
    assert_eq!(invalid_js["success"], false, "{invalid_js}");
    assert_eq!(invalid_js["error"]["code"], "JS_EVAL_ERROR");

    session
        .cmd()
        .args(["open", &server.url_for("/html")])
        .output()
        .unwrap();
    let scroll = parse_json(
        &session
            .cmd()
            .args(["scroll", "down", "--amount", "500"])
            .output()
            .unwrap(),
    );
    assert_eq!(scroll["success"], true);
    assert_eq!(scroll["data"]["subject"]["kind"], "viewport");
    assert_eq!(scroll["data"]["result"]["direction"], "down");
    assert!(scroll["data"]["result"]["position"]["y"].is_number());
    assert!(scroll["data"]["result"]["position"]["x"].is_number());
    assert!(scroll["data"]["result"]["position"]["at_bottom"].is_boolean());

    let scroll_y = parse_json(
        &session
            .cmd()
            .args(["exec", "window.scrollY"])
            .output()
            .unwrap(),
    );
    assert!(
        scroll_y["data"]["result"].as_f64().unwrap_or(0.0) > 0.0,
        "scrollY should be > 0 after scroll down"
    );

    let screenshot = parse_json(
        &session
            .cmd()
            .args(["screenshot", "--path", &normal_path])
            .output()
            .unwrap(),
    );
    assert_eq!(screenshot["success"], true);
    assert_eq!(
        screenshot["data"]["result"]["artifact"]["artifact_state"]["truth_level"],
        "command_artifact"
    );
    assert_eq!(
        screenshot["data"]["result"]["artifact"]["artifact_state"]["artifact_authority"],
        "router.screenshot_artifact"
    );
    assert_eq!(
        screenshot["data"]["result"]["artifact"]["artifact_state"]["upstream_truth"],
        "page_screenshot_result"
    );
    assert_eq!(
        screenshot["data"]["result"]["artifact"]["artifact_state"]["durability"],
        "durable"
    );
    let normal_size = screenshot["data"]["result"]["artifact"]["size_bytes"]
        .as_u64()
        .unwrap();

    let screenshot_full = parse_json(
        &session
            .cmd()
            .args(["screenshot", "--path", &full_path, "--full"])
            .output()
            .unwrap(),
    );
    assert_eq!(screenshot_full["success"], true);
    assert_eq!(
        screenshot_full["data"]["result"]["artifact"]["artifact_state"]["truth_level"],
        "command_artifact"
    );
    let full_size = screenshot_full["data"]["result"]["artifact"]["size_bytes"]
        .as_u64()
        .unwrap();
    assert!(
        full_size >= normal_size,
        "full page screenshot ({full_size}) should be >= normal ({normal_size})"
    );

    let sessions = parse_json(&session.cmd().arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true);
    assert_eq!(sessions["command"], "sessions");
    assert_eq!(sessions["stdout_schema_version"], "3.0");
    let session_items = sessions["data"]["result"]["items"].as_array().unwrap();
    assert!(!session_items.is_empty());
    let first_session = &session_items[0];
    assert_eq!(
        sessions["data"]["subject"]["rub_home_state"]["path_authority"],
        "cli.sessions.subject.rub_home"
    );
    assert!(first_session["id"].as_str().is_some());
    assert!(first_session["name"].as_str().is_some());
    assert!(first_session["pid"].is_number());
    assert!(first_session["socket"].as_str().is_some());
    assert_eq!(
        first_session["socket_state"]["path_authority"],
        "cli.sessions.result.items.socket"
    );
}

// ============================================================
// Additional integration tests
// Covered in part by `t056_131_standard_flow_and_runtime_contract_grouped_scenario`.
// ============================================================

/// T117: kill browser process → next command auto-restarts.
#[test]
#[ignore]
#[serial]
fn t117_browser_crash_recovery() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_standard_site_fixture();

    // Open page
    let out = rub_cmd(home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    // Kill the exact managed browser authority owned by this daemon.
    let daemon_pid: u32 = std::fs::read_to_string(default_session_pid_path(home))
        .expect("default session pid file")
        .trim()
        .parse()
        .expect("daemon pid should parse");
    let browser_pids = browser_processes_for_daemon_pid(daemon_pid);
    assert!(
        !browser_pids.is_empty(),
        "managed browser crash-recovery guardrail must target the real session-scoped browser authority"
    );
    for pid in browser_pids {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    // Wait for process to die
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Next command should work (daemon auto-starts new browser)
    let out = rub_cmd(home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    // May succeed or fail with restart, but should not panic
    // A fresh open after crash should work
    assert!(
        json["success"] == true || json["error"]["code"] == "BROWSER_CRASHED",
        "should either recover or report crash: {json}"
    );
}

/// T109/T111: Multi-session: start named session, verify isolation.
#[test]
#[ignore]
#[serial]
fn t109_multi_session_isolation() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_test_server(vec![
        (
            "/default",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Default Session</title></head><body><h1>Default Session</h1></body></html>"#,
        ),
        (
            "/second",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Second Session</title></head><body><h1>Second Session</h1></body></html>"#,
        ),
    ]);

    // Start default session
    let out = rub_cmd(home)
        .args(["open", &server.url_for("/default")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    // Start second session named "test2"
    let out = rub_cmd(home)
        .args(["--session", "test2", "open", &server.url_for("/second")])
        .output()
        .unwrap();
    assert_eq!(
        parse_json(&out)["success"],
        true,
        "second session should start"
    );

    // Verify sessions list shows both
    let out = rub_cmd(home).arg("sessions").output().unwrap();
    let json = parse_json(&out);
    let sessions = json["data"]["result"]["items"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"default"), "should have default: {names:?}");
    assert!(names.contains(&"test2"), "should have test2: {names:?}");

    // Clean up both sessions
    rub_cmd(home).arg("close").output().unwrap();
    rub_cmd(home)
        .args(["--session", "test2", "close"])
        .output()
        .unwrap();
}

/// T076/T079: stale snapshot invalidation should reuse one browser-backed session.
#[test]
#[ignore]
#[serial]
fn t076_079_stale_snapshot_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let (_rt, server) = start_test_server(vec![
        (
            "/click",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <button id="advance">Advance</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('advance').addEventListener('click', () => {
      document.getElementById('status').textContent = 'done';
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/shared",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <button id="advance">Advance</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('advance').addEventListener('click', () => {
      document.getElementById('status').textContent = 'done';
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/click")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");
    let state1 = parse_json(&session.cmd().arg("state").output().unwrap());
    let snap1 = state1["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    let changed = parse_json(
        &session
            .cmd()
            .args(["exec", "document.title = 'changed'"])
            .output()
            .unwrap(),
    );
    assert_eq!(changed["success"], true, "{changed}");

    let state2 = parse_json(&session.cmd().arg("state").output().unwrap());
    let snap2 = state2["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    assert_ne!(snap1, snap2, "snapshot_id should differ after epoch change");

    let button_index = find_element_index(&state1, |element| {
        element["text"].as_str() == Some("Advance")
    });
    let stale = parse_json(
        &session
            .cmd()
            .args(["click", &button_index.to_string(), "--snapshot", &snap1])
            .output()
            .unwrap(),
    );
    assert_eq!(stale["success"], false, "{stale}");
    assert_eq!(stale["error"]["code"], "STALE_SNAPSHOT", "{stale}");

    let reopened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/shared")])
            .output()
            .unwrap(),
    );
    assert_eq!(reopened["success"], true, "{reopened}");
    let state = parse_json(&session.cmd().arg("state").output().unwrap());
    let snap = state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();
    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Advance")
    });

    let first = parse_json(
        &session
            .cmd()
            .args(["click", &button_index.to_string(), "--snapshot", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(first["success"], true, "{first}");

    let second = parse_json(
        &session
            .cmd()
            .args(["click", &button_index.to_string(), "--snapshot", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(second["success"], false, "{second}");
    assert_eq!(second["error"]["code"], "STALE_SNAPSHOT", "{second}");
}

/// T112: two sessions using the same profile should fail with PROFILE_IN_USE.
#[test]
#[ignore]
#[serial]
fn t112_profile_in_use_error() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let profile = format!("/tmp/rub-shared-profile-{}", std::process::id());
    let _ = std::fs::remove_dir_all(&profile);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(home)
        .args(["--user-data-dir", &profile, "open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(home)
        .args([
            "--session",
            "test2",
            "--user-data-dir",
            &profile,
            "open",
            &server.url(),
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "PROFILE_IN_USE");
    let _ = std::fs::remove_dir_all(&profile);
}

/// T118/T119: queue-timeout and stale-registry busy-session authority should
/// reuse one browser-backed session.
#[test]
#[ignore]
#[serial]
fn t118_119_queue_and_busy_registry_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let (_rt, server) = start_standard_site_fixture();

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let hold = session
        .cmd()
        .args([
            "exec",
            "(() => { const end = Date.now() + 1500; while (Date.now() < end) {} return 'held'; })()",
        ])
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(150));

    let queue_timeout = parse_json(
        &session
            .cmd()
            .args(["--timeout", "100", "state"])
            .output()
            .unwrap(),
    );
    assert_eq!(queue_timeout["success"], false, "{queue_timeout}");
    assert_eq!(
        queue_timeout["error"]["code"], "IPC_TIMEOUT",
        "{queue_timeout}"
    );
    assert_eq!(
        queue_timeout["error"]["context"]["command"], "state",
        "{queue_timeout}"
    );
    assert_eq!(
        queue_timeout["error"]["context"]["phase"], "queue",
        "{queue_timeout}"
    );
    let timeout_ms = queue_timeout["error"]["context"]["transaction_timeout_ms"]
        .as_u64()
        .expect("transaction timeout should be numeric");
    let queue_ms = queue_timeout["error"]["context"]["queue_ms"]
        .as_u64()
        .expect("queue time should be numeric");
    assert!(
        (1..=100).contains(&timeout_ms),
        "timeout should remain within the requested budget: {queue_timeout}"
    );
    assert!(
        (1..=100).contains(&queue_ms),
        "queue time should remain within the requested budget: {queue_timeout}"
    );
    let suggestion = queue_timeout["error"]["suggestion"]
        .as_str()
        .unwrap_or_default();
    assert!(
        suggestion.contains("one command at a time"),
        "{queue_timeout}"
    );
    assert!(suggestion.contains("separate RUB_HOME"), "{queue_timeout}");

    let mut hold = hold;
    let _ = hold.wait();

    let registry_path = format!("{}/registry.json", session.home());
    let mut registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).unwrap()).unwrap();
    registry["sessions"][0]["ipc_protocol_version"] = serde_json::json!("0.0");
    std::fs::write(
        &registry_path,
        serde_json::to_string_pretty(&registry).unwrap(),
    )
    .unwrap();

    let hold = session
        .cmd()
        .args([
            "exec",
            "(() => { const end = Date.now() + 1500; while (Date.now() < end) {} return 'busy'; })()",
        ])
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(150));

    let state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(
        state["success"], true,
        "live socket protocol authority should win over a stale registry projection: {state}"
    );

    let mut hold = hold;
    let _ = hold.wait();
}

/// T078/T087: exec replay baseline and async DOM mutation should reuse one
/// browser-backed session.
#[test]
#[ignore]
#[serial]
fn t078_087_exec_baseline_and_async_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let (_rt, server) = start_standard_site_fixture();

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let baseline = parse_json(
        &session
            .cmd()
            .args(["exec", "Math.random()"])
            .output()
            .unwrap(),
    );
    assert_eq!(baseline["success"], true, "{baseline}");

    let mutated = parse_json(
        &session
            .cmd()
            .args(["exec", "document.body.innerHTML = '<h1>Modified</h1>'"])
            .output()
            .unwrap(),
    );
    assert_eq!(mutated["success"], true, "{mutated}");

    let state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
}

// ── v1.1: US1 Keyboard Operations ───────────────────────────────────

#[test]
#[ignore]
#[serial]
fn t200_204_keyboard_and_type_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/keys-enter",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input id="trigger" autofocus />
  <div id="status">idle</div>
  <script>
    document.getElementById('trigger').addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        document.getElementById('status').textContent = 'submitted';
      }
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/type-basic",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" autofocus />
</body>
</html>"#,
        ),
        (
            "/type-selector",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" />
</body>
</html>"#,
        ),
        (
            "/type-target-text",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <textarea aria-label="Editor"></textarea>
</body>
</html>"#,
        ),
        (
            "/type-clear",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" value="seed value" />
</body>
</html>"#,
        ),
        (
            "/type-formatter",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body style="margin:0">
  <div style="height: 1800px"></div>
  <input id="editor" value="seed value" />
  <script>
    const editor = document.getElementById('editor');
    editor.addEventListener('input', () => {
      editor.value = editor.value.toUpperCase();
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/type-readonly",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input id="target" value="locked" readonly />
  <script>document.getElementById('target').focus();</script>
</body>
</html>"#,
        ),
        (
            "/type-noneditable",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <button id="target" autofocus>Focusable</button>
</body>
</html>"#,
        ),
        (
            "/keys-invalid",
            "text/html",
            r#"<!DOCTYPE html><html><body><button id="ready">ready</button></body></html>"#,
        ),
        (
            "/keys-control-a",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" value="select me" autofocus />
  <div id="status">idle</div>
  <script>
    document.getElementById('editor').addEventListener('keydown', (event) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a') {
        document.getElementById('status').textContent = 'shortcut';
      }
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/keys-plain",
            "text/html",
            r#"<!DOCTYPE html><html><body><button id="ready">ready</button></body></html>"#,
        ),
    ]);

    let keys_enter = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/keys-enter")])
            .output()
            .unwrap(),
    );
    assert_eq!(keys_enter["success"], true, "Step 1: open keys-enter");
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('trigger').focus(); null"])
            .output()
            .unwrap(),
    );
    let keys_enter = parse_json(&session.cmd().args(["keys", "Enter"]).output().unwrap());
    assert_eq!(keys_enter["success"], true, "{keys_enter}");
    assert_eq!(
        keys_enter["data"]["interaction"]["semantic_class"],
        "invoke_workflow"
    );
    assert_eq!(
        keys_enter["data"]["interaction"]["interaction_confirmed"],
        true
    );
    assert_eq!(
        keys_enter["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );
    let keys_enter_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(keys_enter_verify["data"]["result"], "submitted");

    let type_basic = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-basic")])
            .output()
            .unwrap(),
    );
    assert_eq!(type_basic["success"], true, "Step 2: open type-basic");
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').focus(); null"])
            .output()
            .unwrap(),
    );
    let type_basic = parse_json(&session.cmd().args(["type", "hello"]).output().unwrap());
    assert_eq!(type_basic["success"], true, "{type_basic}");
    assert_eq!(
        type_basic["data"]["interaction"]["semantic_class"],
        "set_value"
    );
    assert_eq!(
        type_basic["data"]["interaction"]["interaction_confirmed"],
        true
    );
    assert_eq!(
        type_basic["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );
    let type_basic_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_basic_verify["data"]["result"], "hello");

    let type_selector = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-selector")])
            .output()
            .unwrap(),
    );
    assert_eq!(type_selector["success"], true, "Step 3: open type-selector");
    let type_selector = parse_json(
        &session
            .cmd()
            .args(["type", "--selector", "#editor", "hello"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_selector["success"], true, "{type_selector}");
    assert_eq!(type_selector["data"]["subject"]["tag"], "input");
    let type_selector_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_selector_verify["data"]["result"], "hello");

    let type_target_text = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-target-text")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        type_target_text["success"], true,
        "Step 4: open type-target-text"
    );
    let type_target_text = parse_json(
        &session
            .cmd()
            .args(["type", "--target-text", "Editor", "hello"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_target_text["success"], true, "{type_target_text}");
    assert_eq!(type_target_text["data"]["subject"]["tag"], "textarea");
    let type_target_text_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.querySelector('textarea').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_target_text_verify["data"]["result"], "hello");

    let type_clear = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-clear")])
            .output()
            .unwrap(),
    );
    assert_eq!(type_clear["success"], true, "Step 5: open type-clear");
    let type_clear = parse_json(
        &session
            .cmd()
            .args(["type", "--selector", "#editor", "--clear", "hello"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_clear["success"], true, "{type_clear}");
    let type_clear_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_clear_verify["data"]["result"], "hello");

    let type_formatter = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-formatter")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        type_formatter["success"], true,
        "Step 6: open type-formatter"
    );
    let type_formatter = parse_json(
        &session
            .cmd()
            .args(["type", "--selector", "#editor", "--clear", "hello world"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_formatter["success"], true, "{type_formatter}");
    assert_eq!(
        type_formatter["data"]["interaction"]["confirmation_status"],
        "contradicted"
    );
    assert_eq!(
        type_formatter["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );
    let type_formatter_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(type_formatter_verify["data"]["result"], "HELLO WORLD");

    let type_projection = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-basic")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        type_projection["success"], true,
        "Step 7: reopen type-basic"
    );
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').focus(); null"])
            .output()
            .unwrap(),
    );
    let compact = parse_json(&session.cmd().args(["type", "hello"]).output().unwrap());
    assert_eq!(compact["success"], true);
    assert!(
        compact["data"].get("interaction_trace").is_none(),
        "{compact}"
    );
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').value = ''; null"])
            .output()
            .unwrap(),
    );
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').focus(); null"])
            .output()
            .unwrap(),
    );
    let verbose = parse_json(
        &session
            .cmd()
            .args(["--verbose", "type", "world"])
            .output()
            .unwrap(),
    );
    assert_eq!(verbose["success"], true);
    assert_eq!(
        verbose["data"]["interaction_trace"]["semantic_class"],
        "set_value"
    );
    assert!(
        verbose["data"]["interaction_trace"]
            .get("observed_effects")
            .is_none(),
        "{verbose}"
    );
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').value = ''; null"])
            .output()
            .unwrap(),
    );
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').focus(); null"])
            .output()
            .unwrap(),
    );
    let traced = parse_json(
        &session
            .cmd()
            .args(["--trace", "type", "codex"])
            .output()
            .unwrap(),
    );
    assert_eq!(traced["success"], true);
    assert!(
        traced["data"]["interaction_trace"]["observed_effects"]
            .as_object()
            .is_some(),
        "{traced}"
    );

    let type_readonly = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-readonly")])
            .output()
            .unwrap(),
    );
    assert_eq!(type_readonly["success"], true, "Step 8: open type-readonly");
    let type_readonly = parse_json(&session.cmd().args(["type", "new-value"]).output().unwrap());
    assert_eq!(type_readonly["success"], false, "{type_readonly}");
    assert_eq!(type_readonly["error"]["code"], "ELEMENT_NOT_INTERACTABLE");
    assert!(
        type_readonly["error"]["message"]
            .as_str()
            .unwrap()
            .contains("readonly")
    );

    let type_noneditable = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/type-noneditable")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        type_noneditable["success"], true,
        "Step 9: open type-noneditable"
    );
    let type_noneditable = parse_json(&session.cmd().args(["type", "new-value"]).output().unwrap());
    assert_eq!(type_noneditable["success"], false, "{type_noneditable}");
    assert_eq!(
        type_noneditable["error"]["code"],
        "ELEMENT_NOT_INTERACTABLE"
    );
    assert!(
        type_noneditable["error"]["message"]
            .as_str()
            .unwrap()
            .contains("editable text target")
    );

    let keys_invalid = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/keys-invalid")])
            .output()
            .unwrap(),
    );
    assert_eq!(keys_invalid["success"], true, "Step 10: open keys-invalid");
    let keys_invalid = parse_json(&session.cmd().args(["keys", "InvalidKey"]).output().unwrap());
    assert_eq!(keys_invalid["success"], false, "{keys_invalid}");
    assert_eq!(keys_invalid["error"]["code"], "INVALID_KEY_NAME");

    let keys_control_a = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/keys-control-a")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        keys_control_a["success"], true,
        "Step 11: open keys-control-a"
    );
    parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('editor').focus(); null"])
            .output()
            .unwrap(),
    );
    let keys_control_a = parse_json(&session.cmd().args(["keys", "Control+a"]).output().unwrap());
    assert_eq!(keys_control_a["success"], true, "{keys_control_a}");
    assert_eq!(
        keys_control_a["data"]["interaction"]["semantic_class"],
        "invoke_workflow"
    );
    assert_eq!(
        keys_control_a["data"]["interaction"]["interaction_confirmed"],
        true
    );
    assert_eq!(
        keys_control_a["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );
    let keys_control_a_verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(keys_control_a_verify["data"]["result"], "shortcut");

    let keys_plain = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/keys-plain")])
            .output()
            .unwrap(),
    );
    assert_eq!(keys_plain["success"], true, "Step 12: open keys-plain");
    let keys_plain = parse_json(&session.cmd().args(["keys", "hello"]).output().unwrap());
    assert_eq!(keys_plain["success"], false, "{keys_plain}");
    assert_eq!(keys_plain["error"]["code"], "INVALID_KEY_NAME");
    let msg = keys_plain["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("rub type"),
        "Error for plain text should suggest 'rub type', got: {msg}"
    );
}

// ── v1.1: US2 Wait Commands ─────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn t210_216c_wait_and_click_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let (_rt, server) = start_test_server(vec![
        (
            "/wait-text",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1>Example Domain</h1></body></html>"#,
        ),
        (
            "/wait-normalize",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Wait Normalize</title></head>
<body>
  <main>
    <h1>Enter
      ACCOUNT      Information</h1>
  </main>
</body>
</html>"#,
        ),
        (
            "/wait-selector",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1>Wait Fixture</h1></body></html>"#,
        ),
        (
            "/wait-role",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Semantic Wait Fixture</title></head>
<body>
  <button>Primary</button>
  <script>
    setTimeout(() => {
      const secondary = document.createElement('button');
      secondary.textContent = 'Secondary';
      document.body.appendChild(secondary);
    }, 150);
  </script>
</body>
</html>"#,
        ),
        (
            "/wait-missing",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1>Missing Fixture</h1></body></html>"#,
        ),
        (
            "/mutation",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Mutation Fixture</title></head>
<body>
  <button id="advance" onclick="document.getElementById('status').textContent='done'; document.body.dataset.step='1';">
    Advance
  </button>
  <div id="status">idle</div>
</body>
</html>"#,
        ),
        (
            "/delayed",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Delayed Click Effect</title></head>
<body>
  <button id="trigger">Apply Later</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('trigger').addEventListener('click', () => {
      setTimeout(() => {
        document.getElementById('status').textContent = 'done';
      }, 1800);
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/focus",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Focus Change Fixture</title></head>
<body>
  <input id="target" placeholder="Focus Only" />
  <div id="status">idle</div>
</body>
</html>"#,
        ),
    ]);

    let wait_text = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/wait-text")])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_text["success"], true, "Step 1: open wait-text");
    let wait_text = parse_json(
        &session
            .cmd()
            .args(["wait", "--text", "Example Domain", "--timeout", "5000"])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_text["success"], true, "{wait_text}");
    assert_eq!(wait_text["data"]["result"]["matched"], true);
    assert!(wait_text["data"]["result"]["elapsed_ms"].as_u64().unwrap() < 5000);

    let wait_normalize = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/wait-normalize")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        wait_normalize["success"], true,
        "Step 2: open wait-normalize"
    );
    let wait_normalize = parse_json(
        &session
            .cmd()
            .args([
                "wait",
                "--text",
                "enter account information",
                "--timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_normalize["success"], true, "{wait_normalize}");
    assert_eq!(wait_normalize["data"]["result"]["matched"], true);

    let wait_selector = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/wait-selector")])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_selector["success"], true, "Step 3: open wait-selector");
    let wait_selector = parse_json(
        &session
            .cmd()
            .args(["wait", "--selector", "h1", "--timeout", "5000"])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_selector["success"], true, "{wait_selector}");
    assert_eq!(wait_selector["data"]["result"]["matched"], true);

    let wait_role = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/wait-role")])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_role["success"], true, "Step 4: open wait-role");
    let wait_role = parse_json(
        &session
            .cmd()
            .args([
                "wait",
                "--role",
                "button",
                "--nth",
                "1",
                "--timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_role["success"], true, "{wait_role}");
    assert_eq!(wait_role["data"]["result"]["matched"], true);
    assert_eq!(wait_role["data"]["subject"]["wait_kind"], "role");
    assert_eq!(wait_role["data"]["subject"]["probe_value"], "button");

    let wait_missing = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/wait-missing")])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_missing["success"], true, "Step 5: open wait-missing");
    let wait_timeout = parse_json(
        &session
            .cmd()
            .args([
                "wait",
                "--selector",
                ".nonexistent-element",
                "--timeout",
                "1000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_timeout["success"], false, "{wait_timeout}");
    assert_eq!(wait_timeout["error"]["code"], "WAIT_TIMEOUT");
    assert_eq!(wait_timeout["error"]["context"]["kind"], "selector");
    assert_eq!(
        wait_timeout["error"]["context"]["value"],
        ".nonexistent-element"
    );
    let timeout_ms = wait_timeout["error"]["context"]["timeout_ms"]
        .as_u64()
        .expect("wait timeout should remain numeric");
    assert!(
        (1..=1000).contains(&timeout_ms),
        "wait timeout should remain within the requested budget: {wait_timeout}"
    );
    assert!(wait_timeout["error"]["context"]["elapsed_ms"].is_number());
    let wait_detached = parse_json(
        &session
            .cmd()
            .args([
                "wait",
                "--selector",
                ".nonexistent",
                "--state",
                "detached",
                "--timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_detached["success"], true, "{wait_detached}");
    assert_eq!(wait_detached["data"]["result"]["matched"], true);

    let mutation = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/mutation")])
            .output()
            .unwrap(),
    );
    assert_eq!(mutation["success"], true, "Step 6: open mutation");
    let state = run_state(session.home());
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| element["text"] == "Advance");
    let click = parse_json(
        &session
            .cmd()
            .args([
                "--trace",
                "click",
                &button_index.to_string(),
                "--snapshot",
                &snapshot,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(click["success"], true, "{click}");
    let exec = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(click["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(click["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(
        click["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );
    assert_eq!(click["data"]["interaction_trace"]["command"], "click");
    assert_eq!(
        click["data"]["interaction_trace"]["semantic_class"],
        "activate"
    );
    assert_eq!(
        click["data"]["interaction_trace"]["confirmation_status"],
        "confirmed"
    );
    assert_eq!(exec["success"], true);
    assert_eq!(exec["data"]["result"], "done");

    let delayed = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/delayed")])
            .output()
            .unwrap(),
    );
    assert_eq!(delayed["success"], true, "Step 7: open delayed");
    let state = run_state(session.home());
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Apply Later")
    });
    let click = parse_json(
        &session
            .cmd()
            .args([
                "--trace",
                "click",
                &button_index.to_string(),
                "--snapshot",
                &snapshot,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(click["success"], true, "{click}");
    assert_eq!(click["data"]["interaction"]["interaction_confirmed"], false);
    assert_eq!(
        click["data"]["interaction"]["confirmation_status"],
        "unconfirmed"
    );
    assert!(click["data"]["interaction"]["confirmation_kind"].is_null());
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let verify = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(verify["success"], true);
    assert_eq!(verify["data"]["result"], "done");

    let focus = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/focus")])
            .output()
            .unwrap(),
    );
    assert_eq!(focus["success"], true, "Step 8: open focus");
    let state = run_state(session.home());
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["tag"].as_str() == Some("input")
            && element["attributes"]["placeholder"].as_str() == Some("Focus Only")
    });
    let click = parse_json(
        &session
            .cmd()
            .args([
                "--trace",
                "click",
                &button_index.to_string(),
                "--snapshot",
                &snapshot,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(click["success"], true, "{click}");
    assert_eq!(click["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        click["data"]["interaction"]["confirmation_status"],
        "confirmed"
    );
    assert_eq!(
        click["data"]["interaction"]["confirmation_kind"],
        "focus_change"
    );
    assert_eq!(
        click["data"]["interaction_trace"]["confirmation_kind"],
        "focus_change"
    );
    assert_eq!(
        click["data"]["interaction_trace"]["observed_effects"]["before_active"],
        false
    );
    assert_eq!(
        click["data"]["interaction_trace"]["observed_effects"]["after_active"],
        true
    );
    assert_eq!(
        click["data"]["interaction"]["observed_effects"]["before_active"],
        false
    );
    assert_eq!(
        click["data"]["interaction"]["observed_effects"]["after_active"],
        true
    );
    let verify = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "document.activeElement && document.activeElement.id",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(verify["data"]["result"], "target");
}

// ── v1.1: US3 Multi-Tab Management ─────────────────────────────────

/// T030a: `tabs` lists current tabs.
#[test]
#[ignore]
#[serial]
fn t220_222_tabs_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Single Tab Fixture</title></head>
<body><h1>Single Tab Fixture</h1></body>
</html>"#,
    )]);

    rub_cmd(home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(home).args(["tabs"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "tabs should succeed");
    let tabs = json["data"]["result"]["items"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_ne!(tabs[0]["url"], "about:blank");

    let out = rub_cmd(home).args(["switch", "99"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "TAB_NOT_FOUND");

    // Close the current tab — should auto-create about:blank
    let out = rub_cmd(home).args(["close-tab"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "close-tab should succeed");
    assert_eq!(json["data"]["subject"]["kind"], "tab");
    assert_eq!(json["data"]["result"]["remaining_tabs"], 1);
    assert_eq!(json["data"]["result"]["active_tab"]["url"], "about:blank");
}

// ── v1.1: US4 DOM Information Retrieval ─────────────────────────────

/// T230/T231/T232/T232e: standard-site read queries should reuse one
/// browser-backed session.
#[test]
#[ignore]
#[serial]
fn t230_232e_get_read_query_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let (_rt, server) = start_standard_site_fixture();

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let title = parse_json(&session.cmd().args(["get", "title"]).output().unwrap());
    assert_eq!(title["success"], true, "{title}");
    assert_eq!(title["data"]["subject"]["kind"], "page", "{title}");
    assert!(
        title["data"]["result"]["value"]
            .as_str()
            .unwrap_or_default()
            .contains("Example"),
        "{title}"
    );

    let full_html = parse_json(&session.cmd().args(["get", "html"]).output().unwrap());
    assert_eq!(full_html["success"], true, "{full_html}");
    assert_eq!(full_html["data"]["subject"]["kind"], "page", "{full_html}");
    assert!(
        full_html["data"]["result"]["value"]
            .as_str()
            .unwrap_or_default()
            .contains("<html"),
        "{full_html}"
    );

    let selector_html = parse_json(
        &session
            .cmd()
            .args(["get", "html", "--selector", "h1"])
            .output()
            .unwrap(),
    );
    assert_eq!(selector_html["success"], true, "{selector_html}");
    assert_eq!(
        selector_html["data"]["subject"]["kind"], "live_read",
        "{selector_html}"
    );
    assert_eq!(
        selector_html["data"]["subject"]["locator"]["selector"], "h1",
        "{selector_html}"
    );
    assert!(
        selector_html["data"]["result"]["value"]
            .as_str()
            .unwrap_or_default()
            .contains("Example Domain"),
        "{selector_html}"
    );

    let invalid = parse_json(
        &session
            .cmd()
            .args(["get", "html", "--selector", "["])
            .output()
            .unwrap(),
    );
    assert_eq!(invalid["success"], false, "{invalid}");
    assert_eq!(invalid["error"]["code"], "INVALID_INPUT", "{invalid}");
    assert!(
        invalid["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not a valid selector"),
        "{invalid}"
    );
    let suggestion = invalid["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(suggestion.contains("--role"), "{invalid}");
    assert!(suggestion.contains("rub observe"), "{invalid}");
}

/// T232d: `get` should read non-interactive content through live read-query authority.
#[test]
#[ignore]
#[serial]
fn t232d_get_selector_read_queries_use_live_dom_authority() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Read Query Fixture</title></head>
<body>
  <article id="story" data-kind="article">
    <h1 id="headline">Read Query Title</h1>
    <p class="lead">Read-only DOM content.</p>
  </article>
</body>
</html>"#,
    )]);

    let opened = parse_json(
        &rub_cmd(home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let text = parse_json(
        &rub_cmd(home)
            .args(["get", "text", "--selector", "#headline"])
            .output()
            .unwrap(),
    );
    assert_eq!(text["success"], true, "{text}");
    assert_eq!(text["data"]["subject"]["kind"], "live_read", "{text}");
    assert_eq!(
        text["data"]["subject"]["locator"]["selector"], "#headline",
        "{text}"
    );
    assert_eq!(text["data"]["result"]["kind"], "text", "{text}");
    assert_eq!(
        text["data"]["result"]["value"], "Read Query Title",
        "{text}"
    );
    assert!(text["data"].get("snapshot_id").is_none(), "{text}");

    let attributes = parse_json(
        &rub_cmd(home)
            .args(["get", "attributes", "--selector", "#story"])
            .output()
            .unwrap(),
    );
    assert_eq!(attributes["success"], true, "{attributes}");
    assert_eq!(
        attributes["data"]["result"]["value"]["data-kind"], "article",
        "{attributes}"
    );
    assert!(
        attributes["data"].get("snapshot_id").is_none(),
        "{attributes}"
    );
}

/// T232f/T232f2/T232f2b: inspect read/list surfaces should reuse one
/// browser-backed session.
#[test]
#[ignore]
#[serial]
fn t232f_f2b_inspect_list_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/inspect",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect Fixture</title></head>
<body>
  <main id="content">
    <h1 id="headline">Inspection Runtime</h1>
    <ul class="items">
      <li class="item"><span class="label">Alpha</span></li>
      <li class="item"><span class="label">Beta</span></li>
    </ul>
  </main>
</body>
</html>"#,
        ),
        (
            "/builder",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect List Builder Fixture</title></head>
<body>
  <select id="flavor">
    <option value="vanilla">Vanilla</option>
    <option value="chocolate">Chocolate</option>
  </select>
</body>
</html>"#,
        ),
        (
            "/row-scope",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect List Row Scope Fixture</title></head>
<body>
  <main class="feed">
    <article class="card" data-id="1">
      <a class="cover" href="/notes/1">
        <img src="https://cdn.example.test/alpha.jpg" alt="Alpha cover" />
      </a>
      <div class="meta">
        <h2 class="title">Alpha title</h2>
        <a class="author" href="/users/alice">Alice</a>
      </div>
    </article>
    <article class="card" data-id="2">
      <a class="cover" href="/notes/2">
        <img src="https://cdn.example.test/beta.jpg" alt="Beta cover" />
      </a>
      <div class="meta">
        <h2 class="title">Beta title</h2>
        <a class="author" href="/users/bob">Bob</a>
      </div>
    </article>
  </main>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/inspect")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let text = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", "#headline"])
            .output()
            .unwrap(),
    );
    assert_eq!(text["success"], true, "{text}");
    assert_eq!(text["data"]["subject"]["kind"], "live_read", "{text}");
    assert_eq!(
        text["data"]["result"]["value"], "Inspection Runtime",
        "{text}"
    );

    let spec = json!({
        "items": {
            "collection": ".item",
            "fields": {
                "label": { "selector": ".label", "kind": "text" }
            }
        }
    })
    .to_string();
    let list = parse_json(
        &session
            .cmd()
            .args(["inspect", "list", &spec])
            .output()
            .unwrap(),
    );
    assert_eq!(list["success"], true, "{list}");
    assert_eq!(
        list["data"]["result"]["items"],
        json!([
            { "label": "Alpha" },
            { "label": "Beta" }
        ]),
        "{list}"
    );

    let opened_builder = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/builder")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_builder["success"], true, "{opened_builder}");

    let list = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "list",
                "--collection",
                "#flavor option",
                "--field",
                "text",
                "--field",
                "value=attribute:value",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(list["success"], true, "{list}");
    assert_eq!(
        list["data"]["result"]["items"],
        json!([
            { "text": "Vanilla", "value": "vanilla" },
            { "text": "Chocolate", "value": "chocolate" }
        ]),
        "{list}"
    );

    let opened_row_scope = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/row-scope")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_row_scope["success"], true, "{opened_row_scope}");

    let list = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "list",
                "--collection",
                ".cover",
                "--row-scope",
                ".card",
                "--field",
                "href=attribute:href",
                "--field",
                "image_url=attribute:src:img",
                "--field",
                "title=text:.title",
                "--field",
                "author=text:.author",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(list["success"], true, "{list}");
    assert_eq!(
        list["data"]["result"]["items"],
        json!([
            {
                "href": "/notes/1",
                "image_url": "https://cdn.example.test/alpha.jpg",
                "title": "Alpha title",
                "author": "Alice"
            },
            {
                "href": "/notes/2",
                "image_url": "https://cdn.example.test/beta.jpg",
                "title": "Beta title",
                "author": "Bob"
            }
        ]),
        "{list}"
    );
}

/// T232f5/T232f5b/T232f5c: inspect harvest should reuse one browser-backed
/// session across file-spec, shorthand-field, and auto-detect flows.
#[test]
#[ignore]
#[serial]
fn t232f5_f5c_inspect_harvest_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body><main>feed root</main></body></html>",
        ),
        (
            "/detail/a",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Alpha detail</h1><div class="author">Alice</div></body></html>"#,
        ),
        (
            "/detail/b",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Beta detail</h1><div class="author">Bob</div></body></html>"#,
        ),
        (
            "/builder/a",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Alpha detail</h1><img class="hero" data-testid="hero-image" src="/img/a.webp"><div class="author">Alice</div></body></html>"#,
        ),
        (
            "/builder/b",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Beta detail</h1><img class="hero" data-testid="hero-image" src="/img/b.webp"><div class="author">Bob</div></body></html>"#,
        ),
    ]);

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{}/detail-rows.json", session.home());
    std::fs::write(
        &source_path,
        serde_json::json!({
            "data": {
                "fields": {
                    "items": [
                        { "href": "/builder/a", "note_id": "alpha" },
                        { "href": "/builder/b", "note_id": "beta" }
                    ]
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let extract_path = format!("{}/detail-spec.json", session.home());
    std::fs::write(
        &extract_path,
        serde_json::json!({
            "title": { "selector": ".title", "kind": "text" },
            "author": { "selector": ".author", "kind": "text" }
        })
        .to_string(),
    )
    .unwrap();

    let harvested = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "harvest",
                "--file",
                &source_path,
                "--input-field",
                "data.fields.items",
                "--url-field",
                "href",
                "--name-field",
                "note_id",
                "--base-url",
                &server.url(),
                "--extract-file",
                &extract_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(harvested["success"], true, "{harvested}");
    assert_eq!(
        harvested["data"]["result"]["summary"]["complete"], true,
        "{harvested}"
    );
    assert_eq!(
        harvested["data"]["result"]["summary"]["harvested_count"], 2,
        "{harvested}"
    );
    assert_eq!(
        harvested["data"]["result"]["entries"][0]["result"]["fields"],
        serde_json::json!({
            "author": "Alice",
            "title": "Alpha detail"
        }),
        "{harvested}"
    );
    assert_eq!(
        harvested["data"]["result"]["entries"][1]["result"]["fields"],
        serde_json::json!({
            "author": "Bob",
            "title": "Beta detail"
        }),
        "{harvested}"
    );

    let builder_path = format!("{}/detail-rows-builder.json", session.home());
    std::fs::write(
        &builder_path,
        serde_json::json!({
            "data": {
                "fields": {
                    "items": [
                        { "href": "/builder/a", "note_id": "alpha" },
                        { "href": "/builder/b", "note_id": "beta" }
                    ]
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let builder = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "harvest",
                "--file",
                &builder_path,
                "--input-field",
                "data.fields.items",
                "--url-field",
                "href",
                "--name-field",
                "note_id",
                "--base-url",
                &server.url(),
                "--field",
                "title=text:.title",
                "--field",
                "author=text:.author",
                "--field",
                "hero=attribute:src:img.hero",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(builder["success"], true, "{builder}");
    assert_eq!(
        builder["data"]["result"]["summary"]["complete"], true,
        "{builder}"
    );
    assert_eq!(
        builder["data"]["result"]["entries"][0]["result"]["fields"],
        serde_json::json!({
            "author": "Alice",
            "hero": "/img/a.webp",
            "title": "Alpha detail"
        }),
        "{builder}"
    );
    assert_eq!(
        builder["data"]["result"]["entries"][1]["result"]["fields"],
        serde_json::json!({
            "author": "Bob",
            "hero": "/img/b.webp",
            "title": "Beta detail"
        }),
        "{builder}"
    );

    let autodetect_path = format!("{}/detail-rows-autodetect.json", session.home());
    std::fs::write(
        &autodetect_path,
        serde_json::json!({
            "data": {
                "result": {
                    "items": [
                        { "href": "/detail/a", "note_id": "alpha" }
                    ],
                    "item_count": 1
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let autodetect = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "harvest",
                "--file",
                &autodetect_path,
                "--base-url",
                &server.url(),
                "--field",
                "title=text:.title",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(autodetect["success"], true, "{autodetect}");
    assert_eq!(
        autodetect["data"]["result"]["summary"]["harvested_count"], 1,
        "{autodetect}"
    );
    assert_eq!(
        autodetect["data"]["result"]["entries"][0]["result"]["fields"],
        serde_json::json!({
            "title": "Alpha detail"
        }),
        "{autodetect}"
    );
}

/// T232f3/T232f4: scan-until complete/partial flows should reuse one
/// browser-backed session.
#[test]
#[ignore]
#[serial]
fn t232f3_f4_inspect_list_scan_until_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let scan_html = r#"<!DOCTYPE html>
<html>
<head>
  <title>Inspect List Scan Fixture</title>
  <style>
    body { margin: 0; font-family: sans-serif; }
    #feed { padding: 12px; }
    .item {
      height: 220px;
      margin: 0 0 12px;
      padding: 12px;
      border: 1px solid #d1d5db;
      border-radius: 8px;
      background: #fff;
      box-sizing: border-box;
    }
  </style>
</head>
<body>
  <div id="feed"></div>
  <script>
    (() => {
      const feed = document.getElementById('feed');
      const total = 18;
      const batch = 4;
      let nextId = 1;
      let loading = false;

      function appendBatch() {
        const limit = Math.min(total, nextId + batch - 1);
        while (nextId <= limit) {
          const item = document.createElement('article');
          item.className = 'item';
          item.dataset.id = String(nextId);
          item.innerHTML = `<div class="label">Item ${nextId}</div>`;
          feed.appendChild(item);
          nextId += 1;
        }
      }

      appendBatch();
      appendBatch();

      window.addEventListener('scroll', () => {
        if (loading || nextId > total) return;
        const nearBottom =
          window.innerHeight + window.scrollY >= document.body.scrollHeight - 60;
        if (!nearBottom) return;
        loading = true;
        setTimeout(() => {
          appendBatch();
          loading = false;
        }, 120);
      });
    })();
  </script>
</body>
</html>"#;
    let bottom_html = r#"<!DOCTYPE html>
<html>
<head>
  <title>Inspect List Scan Bottom Fixture</title>
  <style>
    body { margin: 0; font-family: sans-serif; }
    #feed { padding: 12px; }
    .item {
      height: 260px;
      margin: 0 0 12px;
      padding: 12px;
      border: 1px solid #d1d5db;
      border-radius: 8px;
      background: #fff;
      box-sizing: border-box;
    }
  </style>
</head>
<body>
  <div id="feed">
    <article class="item" data-id="alpha"><div class="label">Alpha</div></article>
    <article class="item" data-id="beta"><div class="label">Beta</div></article>
    <article class="item" data-id="gamma"><div class="label">Gamma</div></article>
  </div>
</body>
</html>"#;
    let (_rt, server) = start_test_server(vec![
        ("/scan", "text/html", scan_html),
        ("/bottom", "text/html", bottom_html),
    ]);

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/scan")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let list = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "list",
                "--collection",
                ".item",
                "--field",
                "note_id=attribute:data-id",
                "--field",
                "label=text:.label",
                "--scan-until",
                "12",
                "--scan-key",
                "note_id",
                "--settle-ms",
                "250",
                "--max-scrolls",
                "10",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(list["success"], true, "{list}");
    let rows = list["data"]["result"]["items"]
        .as_array()
        .expect("scan rows");
    assert_eq!(rows.len(), 12, "{list}");
    assert_eq!(list["data"]["result"]["scan"]["complete"], true, "{list}");
    assert_eq!(
        list["data"]["result"]["scan"]["stop_reason"], "target_reached",
        "{list}"
    );
    assert_eq!(
        list["data"]["result"]["scan"]["returned_count"], 12,
        "{list}"
    );
    assert_eq!(list["data"]["result"]["scan"]["unique_count"], 12, "{list}");
    assert_eq!(rows[0]["note_id"], "1", "{list}");
    assert_eq!(rows[11]["note_id"], "12", "{list}");

    let opened_bottom = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/bottom")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_bottom["success"], true, "{opened_bottom}");

    let partial = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "list",
                "--collection",
                ".item",
                "--field",
                "note_id=attribute:data-id",
                "--field",
                "label=text:.label",
                "--scan-until",
                "5",
                "--scan-key",
                "note_id",
                "--settle-ms",
                "200",
                "--stall-limit",
                "4",
                "--max-scrolls",
                "6",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(partial["success"], true, "{partial}");
    let rows = partial["data"]["result"]["items"]
        .as_array()
        .expect("partial scan rows");
    assert_eq!(rows.len(), 3, "{partial}");
    assert_eq!(
        partial["data"]["result"]["scan"]["complete"], false,
        "{partial}"
    );
    assert_eq!(
        partial["data"]["result"]["scan"]["stop_reason"], "at_bottom",
        "{partial}"
    );
    assert_eq!(
        partial["data"]["result"]["scan"]["returned_count"], 3,
        "{partial}"
    );
    assert_eq!(
        partial["data"]["result"]["scan"]["unique_count"], 3,
        "{partial}"
    );
    assert_eq!(rows[0]["note_id"], "alpha", "{partial}");
    assert_eq!(rows[2]["note_id"], "gamma", "{partial}");
}

/// T232g/T232h/T232m/T232n: inspect page/text/html read surfaces should
/// reuse one browser-backed session.
#[test]
#[ignore]
#[serial]
fn t232g_n_inspect_page_text_html_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/scope",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect Page Scope Fixture</title></head>
<body>
  <main role="main">
    <button>Primary CTA</button>
  </main>
  <section role="complementary">
    <button>Secondary CTA</button>
  </section>
</body>
</html>"#,
        ),
        (
            "/compact",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect Page Compact Fixture</title></head>
<body>
  <main role="main">
    <button>Primary CTA</button>
    <section>
      <a href="/docs">Docs</a>
      <div><button>Nested CTA</button></div>
    </section>
  </main>
</body>
</html>"#,
        ),
        (
            "/text-many",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect Text Many Fixture</title></head>
<body>
  <main>
    <h2 class="section">Alpha release</h2>
    <h2 class="section">Beta release</h2>
  </main>
</body>
</html>"#,
        ),
        (
            "/html-many",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect HTML Fixture</title></head>
<body>
  <main>
    <article><h2>Alpha</h2><p>First card</p></article>
    <article><h2>Beta</h2><p>Second card</p></article>
  </main>
</body>
</html>"#,
        ),
    ]);

    let opened_scope = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/scope")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_scope["success"], true, "{opened_scope}");

    let inspected = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "page",
                "--scope-role",
                "main",
                "--format",
                "a11y",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(inspected["data"]["subject"]["kind"], "page_observation");
    assert_eq!(inspected["data"]["subject"]["format"], "a11y");
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["scope"]["kind"],
        "role"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["scope"]["role"],
        "main"
    );
    let a11y = inspected["data"]["result"]["snapshot"]["a11y_text"]
        .as_str()
        .unwrap();
    assert!(a11y.contains("Primary CTA"), "{inspected}");
    assert!(!a11y.contains("Secondary CTA"), "{inspected}");

    let opened_compact = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/compact")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_compact["success"], true, "{opened_compact}");

    let inspected = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "page",
                "--scope-role",
                "main",
                "--format",
                "compact",
                "--depth",
                "2",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["observation_projection"]["mode"],
        "compact"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["observation_projection"]["depth_limit"],
        2
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["observation_projection"]["depth_count"],
        3
    );
    let compact_text = inspected["data"]["result"]["snapshot"]["compact_text"]
        .as_str()
        .unwrap();
    assert!(compact_text.contains("Primary CTA"), "{inspected}");
    assert!(compact_text.contains("Docs"), "{inspected}");
    assert!(compact_text.contains("Nested CTA"), "{inspected}");
    assert!(
        compact_text.lines().all(|line| !line.starts_with("  ")),
        "{inspected}"
    );
    assert!(
        compact_text.contains("@1]")
            || compact_text.contains("@2]")
            || compact_text.contains("@3]"),
        "{inspected}"
    );

    let opened_text_many = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/text-many")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_text_many["success"], true, "{opened_text_many}");

    let scalar = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", ".section"])
            .output()
            .unwrap(),
    );
    assert_eq!(scalar["success"], false, "{scalar}");
    assert_eq!(scalar["error"]["code"], "INVALID_INPUT", "{scalar}");
    let scalar_suggestion = scalar["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(scalar_suggestion.contains("--first"), "{scalar}");
    assert!(scalar_suggestion.contains("--nth"), "{scalar}");

    let many = parse_json(
        &session
            .cmd()
            .args(["inspect", "text", "--selector", ".section", "--many"])
            .output()
            .unwrap(),
    );
    assert_eq!(many["success"], true, "{many}");
    assert_eq!(many["data"]["subject"]["kind"], "live_read", "{many}");
    assert_eq!(many["data"]["subject"]["read_kind"], "text", "{many}");
    assert_eq!(many["data"]["result"]["kind"], "text", "{many}");
    assert_eq!(many["data"]["result"]["item_count"], 2, "{many}");
    assert_eq!(
        many["data"]["result"]["items"],
        json!(["Alpha release", "Beta release"]),
        "{many}"
    );

    let opened_html_many = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/html-many")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_html_many["success"], true, "{opened_html_many}");

    let single = parse_json(
        &session
            .cmd()
            .args(["inspect", "html", "--role", "article", "--first"])
            .output()
            .unwrap(),
    );
    assert_eq!(single["success"], true, "{single}");
    assert_eq!(single["data"]["subject"]["kind"], "live_read", "{single}");
    let single_html = single["data"]["result"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(single_html.contains("<article"), "{single}");
    assert!(single_html.contains("Alpha"), "{single}");
    assert!(!single_html.contains("Beta"), "{single}");

    let many = parse_json(
        &session
            .cmd()
            .args(["inspect", "html", "--role", "article", "--many"])
            .output()
            .unwrap(),
    );
    assert_eq!(many["success"], true, "{many}");
    assert_eq!(many["data"]["subject"]["kind"], "live_read", "{many}");
    assert_eq!(many["data"]["result"]["kind"], "html", "{many}");
    assert_eq!(many["data"]["result"]["item_count"], 2, "{many}");
    let items = many["data"]["result"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(items.len(), 2, "{many}");
    assert!(
        items[0].as_str().unwrap_or_default().contains("Alpha"),
        "{many}"
    );
    assert!(
        items[1].as_str().unwrap_or_default().contains("Beta"),
        "{many}"
    );

    let page = parse_json(&session.cmd().args(["inspect", "html"]).output().unwrap());
    assert_eq!(page["success"], false, "{page}");
    assert_eq!(page["error"]["code"], "INVALID_INPUT", "{page}");
    assert!(
        page["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Missing required target"),
        "{page}"
    );
}

/// T232i/T232j: network detail/curl/wait should reuse one browser-backed session.
#[test]
#[ignore]
#[serial]
fn t232i_j_network_detail_and_wait_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect Network Fixture</title></head>
<body>
  <div id="orders">pending</div>
  <div id="missing">pending</div>
  <script>
    fetch('/api/orders', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'x-rub-trace': 'alpha'
      },
      body: JSON.stringify({ orderId: 42 })
    })
      .then(async (response) => {
        document.getElementById('orders').textContent =
          'orders:' + response.status + ':' + (await response.text());
      });

    fetch('/api/missing')
      .then(async (response) => {
        document.getElementById('missing').textContent =
          'missing:' + response.status + ':' + (await response.text());
      });
  </script>
</body>
</html>"#,
        ),
        (
            "/wait",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Inspect Network Wait Fixture</title></head>
<body>
  <div id="status">idle</div>
  <script>
    setTimeout(() => {
      fetch('/api/delayed?order=7', {
        method: 'POST',
        headers: {
          'content-type': 'application/json'
        },
        body: JSON.stringify({ delayed: true })
      })
        .then(async (response) => {
          document.getElementById('status').textContent =
            'done:' + response.status + ':' + (await response.text());
        });
    }, 250);
  </script>
</body>
</html>"#,
        ),
        (
            "/api/orders",
            "application/json",
            r#"{"ok":true,"orderId":42}"#,
        ),
        ("/api/missing", "text/plain", "missing-order"),
        (
            "/api/delayed",
            "application/json",
            r#"{"ok":true,"delayed":true}"#,
        ),
    ]);

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &session
            .cmd()
            .args([
                "wait",
                "--text",
                "missing:200:missing-order",
                "--timeout",
                "10000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let network = parse_json(
        &session
            .cmd()
            .args(["inspect", "network", "--last", "10", "--match", "/api/"])
            .output()
            .unwrap(),
    );
    assert_eq!(network["success"], true, "{network}");
    assert_eq!(
        network["data"]["subject"]["kind"], "network_request_registry",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["source_signal"], "network_request_registry",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "observation_runtime",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub find --content ...",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub extract ...",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub inspect list ... --wait-field ... --wait-contains ...",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["next_command_hints"][4]["command"],
        "rub explain blockers",
        "{network}"
    );
    assert_eq!(
        network["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "mixed_network_registry",
        "{network}"
    );
    let requests = network["data"]["result"]["items"].as_array().unwrap();
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/orders").as_str())
                && request["method"].as_str() == Some("POST")
                && request["status"].as_u64() == Some(200)
                && request["lifecycle"].as_str() == Some("completed")
        }),
        "{network}"
    );
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/missing").as_str())
                && request["status"].as_u64() == Some(200)
        }),
        "{network}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request["request_body"].is_null()),
        "{network}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request["response_body"].is_null()),
        "{network}"
    );
    let orders_id = requests
        .iter()
        .find(|request| request["url"].as_str() == Some(server.url_for("/api/orders").as_str()))
        .and_then(|request| request["request_id"].as_str())
        .unwrap()
        .to_string();

    let filtered = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "network",
                "--status",
                "200",
                "--match",
                "/api/orders",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(filtered["success"], true, "{filtered}");
    assert_eq!(
        filtered["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{filtered}"
    );
    let missing_id = requests
        .iter()
        .find(|request| request["url"].as_str() == Some(server.url_for("/api/missing").as_str()))
        .and_then(|request| request["request_id"].as_str())
        .unwrap()
        .to_string();

    let detail = parse_json(
        &session
            .cmd()
            .args(["inspect", "network", "--id", &orders_id])
            .output()
            .unwrap(),
    );
    assert_eq!(detail["success"], true, "{detail}");
    assert_eq!(
        detail["data"]["subject"]["kind"], "network_request",
        "{detail}"
    );
    assert_eq!(
        detail["data"]["result"]["request"]["method"], "POST",
        "{detail}"
    );
    assert_eq!(
        detail["data"]["result"]["request"]["lifecycle"], "completed",
        "{detail}"
    );
    assert_eq!(
        detail["data"]["result"]["request"]["request_headers"]["content-type"], "application/json",
        "{detail}"
    );
    assert_eq!(
        detail["data"]["result"]["request"]["request_headers"]["x-rub-trace"], "alpha",
        "{detail}"
    );
    assert_eq!(
        detail["data"]["result"]["request"]["request_body"]["preview"], "{\"orderId\":42}",
        "{detail}"
    );
    assert_eq!(
        detail["data"]["result"]["request"]["response_body"]["preview"],
        "{\"ok\":true,\"orderId\":42}",
        "{detail}"
    );

    let read_detail = parse_json(
        &session
            .cmd()
            .args(["inspect", "network", "--id", &missing_id])
            .output()
            .unwrap(),
    );
    assert_eq!(read_detail["success"], true, "{read_detail}");
    assert_eq!(
        read_detail["data"]["workflow_continuity"]["source_signal"], "network_request_record",
        "{read_detail}"
    );
    assert_eq!(
        read_detail["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "content_runtime",
        "{read_detail}"
    );
    assert_eq!(
        read_detail["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub find --content ...",
        "{read_detail}"
    );
    assert_eq!(
        read_detail["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub extract ...",
        "{read_detail}"
    );
    assert_eq!(
        read_detail["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "read_like_network_request",
        "{read_detail}"
    );

    let curl = parse_json(
        &session
            .cmd()
            .args(["inspect", "curl", &orders_id])
            .output()
            .unwrap(),
    );
    assert_eq!(curl["success"], true, "{curl}");
    assert_eq!(curl["data"]["subject"]["kind"], "network_request", "{curl}");
    let command = curl["data"]["result"]["export"]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains("curl -X POST"), "{curl}");
    assert!(
        command.contains(server.url_for("/api/orders").as_str()),
        "{curl}"
    );
    assert!(command.contains("--data-raw"), "{curl}");
    assert!(command.contains("x-rub-trace: alpha"), "{curl}");
    assert_eq!(
        curl["data"]["result"]["export"]["body_complete"], true,
        "{curl}"
    );

    let wait_opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/wait")])
            .output()
            .unwrap(),
    );
    assert_eq!(wait_opened["success"], true, "{wait_opened}");

    let network_wait = parse_json(
        &session
            .cmd()
            .args([
                "inspect",
                "network",
                "--wait",
                "--match",
                "/api/delayed",
                "--method",
                "POST",
                "--lifecycle",
                "terminal",
                "--timeout",
                "10000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(network_wait["success"], true, "{network_wait}");
    assert_eq!(
        network_wait["data"]["subject"]["kind"], "network_request_wait",
        "{network_wait}"
    );
    assert_eq!(
        network_wait["data"]["subject"]["lifecycle"], "terminal",
        "{network_wait}"
    );
    assert_eq!(
        network_wait["data"]["result"]["matched"], true,
        "{network_wait}"
    );
    assert!(
        network_wait["data"]["result"]["elapsed_ms"]
            .as_u64()
            .is_some(),
        "{network_wait}"
    );
    assert_eq!(
        network_wait["data"]["result"]["request"]["method"], "POST",
        "{network_wait}"
    );
    assert_eq!(
        network_wait["data"]["result"]["request"]["status"], 200,
        "{network_wait}"
    );
    assert_eq!(
        network_wait["data"]["result"]["request"]["lifecycle"], "completed",
        "{network_wait}"
    );
    assert_eq!(
        network_wait["data"]["result"]["request"]["response_body"]["preview"],
        "{\"ok\":true,\"delayed\":true}",
        "{network_wait}"
    );
}

/// T232k/T232l: interaction-window grouping and network inspection should
/// reuse one browser-backed session.
#[test]
#[ignore]
#[serial]
fn t232k_l_network_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let server = NetworkInspectionFixtureServer::start();

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &session
            .cmd()
            .args(["click", "--selector", "#request-batch"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["interaction_confirmed"], true,
        "{clicked}"
    );

    let grouped = &clicked["data"]["interaction"]["network_requests"];
    let requests = grouped["requests"]
        .as_array()
        .expect("grouped network requests");
    assert_eq!(requests.len(), 3, "{clicked}");
    assert_eq!(grouped["terminal_count"], 3, "{clicked}");
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/orders").as_str())
                && request["status"].as_u64() == Some(200)
                && request["lifecycle"].as_str() == Some("completed")
        }),
        "{clicked}"
    );
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/missing").as_str())
                && request["status"].as_u64() == Some(404)
                && request["lifecycle"].as_str() == Some("completed")
        }),
        "{clicked}"
    );
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/error").as_str())
                && request["status"].as_u64() == Some(500)
                && request["lifecycle"].as_str() == Some("completed")
        }),
        "{clicked}"
    );
    assert!(
        clicked["data"]["interaction_trace"]["observed_effects"]["network_requests"].is_null(),
        "{clicked}"
    );

    let network = parse_json(
        &session
            .cmd()
            .args(["inspect", "network", "--last", "10", "--match", "/api/"])
            .output()
            .unwrap(),
    );
    assert_eq!(network["success"], true, "{network}");
    let requests = network["data"]["result"]["items"].as_array().unwrap();
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/missing").as_str())
                && request["status"].as_u64() == Some(404)
                && request["lifecycle"].as_str() == Some("completed")
        }),
        "{network}"
    );
    assert!(
        requests.iter().any(|request| {
            request["url"].as_str() == Some(server.url_for("/api/error").as_str())
                && request["status"].as_u64() == Some(500)
                && request["lifecycle"].as_str() == Some("completed")
        }),
        "{network}"
    );
}

// ── v1.1: US5 Extended Click ────────────────────────────────────────

/// T048a: `click --xy` fires click at coordinates.
#[test]
#[ignore]
#[serial]
fn t240_243b_extended_click_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body style="margin:0; width:600px; height:500px;">
    <button
      id="xy-target"
      style="position:absolute; left:80px; top:180px; width:120px; height:60px"
      onclick="document.getElementById('status').textContent='clicked';"
    >
      Click target
    </button>
    <button
      id="hover-target"
      style="position:absolute; left:20px; top:20px; width:120px; height:40px"
      onmouseover="this.dataset.hovered='yes'; document.getElementById('status').textContent='hovered';"
    >
      Hover me
    </button>
    <button
      id="dbl-target"
      style="position:absolute; left:220px; top:20px; width:140px; height:40px"
      ondblclick="document.getElementById('status').textContent='dblclicked';"
    >
      Double click me
    </button>
    <button
      id="right-target"
      style="position:absolute; left:380px; top:20px; width:140px; height:40px"
      oncontextmenu="event.preventDefault(); document.getElementById('status').textContent='context-opened';"
    >
      Right click me
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(home)
        .args(["click", "--xy", "120", "210"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "click --xy should succeed");
    assert_eq!(json["data"]["subject"]["x"], 120.0);
    assert_eq!(json["data"]["subject"]["y"], 210.0);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "clicked");

    rub_cmd(home)
        .args([
            "exec",
            "document.getElementById('status').textContent='idle'; delete document.getElementById('hover-target').dataset.hovered;",
        ])
        .output()
        .unwrap();

    let out = rub_cmd(home)
        .args(["click", "--xy", "300", "300"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], true,
        "click --xy on blank area should succeed"
    );
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], false);
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "unconfirmed"
    );

    let verify = rub_cmd(home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "idle");

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"].as_str().unwrap_or("").contains("Hover me")
    });

    let out = rub_cmd(home)
        .args(["hover", &index.to_string(), "--snapshot", &snapshot])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "hover");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "hover_state"
    );

    let verify = rub_cmd(home)
        .args([
            "exec",
            "document.getElementById('hover-target').dataset.hovered + '|' + document.getElementById('status').textContent",
        ])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "yes|hovered");

    rub_cmd(home)
        .args([
            "exec",
            "document.getElementById('status').textContent='idle'; delete document.getElementById('hover-target').dataset.hovered;",
        ])
        .output()
        .unwrap();

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Double click me")
    });

    let out = rub_cmd(home)
        .args([
            "click",
            "--double",
            &index.to_string(),
            "--snapshot",
            &snapshot,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "dblclicked");

    rub_cmd(home)
        .args([
            "exec",
            "document.getElementById('status').textContent='idle';",
        ])
        .output()
        .unwrap();

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Double click me")
    });

    let out = rub_cmd(home)
        .args([
            "click",
            &index.to_string(),
            "--snapshot",
            &snapshot,
            "--double",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["gesture"], "double");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "dblclicked");

    rub_cmd(home)
        .args([
            "exec",
            "document.getElementById('status').textContent='idle';",
        ])
        .output()
        .unwrap();

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Right click me")
    });

    let out = rub_cmd(home)
        .args([
            "click",
            "--right",
            &index.to_string(),
            "--snapshot",
            &snapshot,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "context-opened");

    rub_cmd(home)
        .args([
            "exec",
            "document.getElementById('status').textContent='idle';",
        ])
        .output()
        .unwrap();

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Right click me")
    });

    let out = rub_cmd(home)
        .args([
            "click",
            &index.to_string(),
            "--snapshot",
            &snapshot,
            "--right",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["gesture"], "right");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "context-opened");

    cleanup(home);
}
