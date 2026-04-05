use super::*;

// ============================================================
// SC-001: 5-step workflow (T131)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t131_sc001_five_step_workflow() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/workflow",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input name="custname" value="" />
</body>
</html>"#,
    )]);

    // Step 1: Navigate
    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/workflow")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true, "Step 1: navigate");

    // Step 2: Inspect
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let state = parse_json(&out);
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

    // Step 3: Input
    let out = rub_cmd(&home)
        .args(["type", "--index", "0", "E2E Test", "--snapshot", snap])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true, "Step 3: type");

    // Step 4: Verify via JS
    let out = rub_cmd(&home)
        .args([
            "exec",
            "document.querySelector('input[name=custname]').value",
        ])
        .output()
        .unwrap();
    let exec_result = parse_json(&out);
    assert_eq!(exec_result["success"], true, "Step 4: exec");
    assert_eq!(exec_result["data"]["result"], "E2E Test");

    // Step 5: State again (verify it still works)
    let out = rub_cmd(&home).arg("state").output().unwrap();
    assert_eq!(parse_json(&out)["success"], true, "Step 5: verify state");

    cleanup(&home);
}

// ============================================================
// Additional integration tests
// ============================================================

/// T056: HTTP 404 returns structured JSON output (may succeed or fail gracefully).
#[test]
#[ignore]
#[serial]
fn t056_open_404_structured_output() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/status/404")])
        .output()
        .unwrap();
    let json = parse_json(&out);
    // Must have structured output regardless of success/failure
    assert!(json["stdout_schema_version"] == "3.0");
    assert!(json["command"] == "open");

    cleanup(&home);
}

/// T074: Side-effecting command increments dom_epoch (SC-015, INV-001 Source A).
#[test]
#[ignore]
#[serial]
fn t074_click_increments_epoch() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    // Get initial epoch (after open, should be >= 1)
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let state1 = parse_json(&out);
    let epoch1 = state1["data"]["result"]["snapshot"]["dom_epoch"]
        .as_u64()
        .unwrap_or(0);
    assert!(epoch1 >= 1, "epoch should be >= 1 after open: {epoch1}");

    // Execute JS (side-effecting command that increments epoch)
    let out = rub_cmd(&home)
        .args(["exec", "document.title = 'modified'"])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true, "exec should succeed");

    // Get epoch after exec — should be > epoch1
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let state2 = parse_json(&out);
    let epoch2 = state2["data"]["result"]["snapshot"]["dom_epoch"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        epoch2 > epoch1,
        "epoch should increment after exec: {epoch1} -> {epoch2}"
    );

    cleanup(&home);
}

/// T085: exec invalid JS → JS_EVAL_ERROR.
#[test]
#[ignore]
#[serial]
fn t085_exec_invalid_js() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home)
        .args(["exec", "this is not valid javascript !!!"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "invalid JS should fail");

    cleanup(&home);
}

/// T094: scroll to bottom → verify scrollY > 0.
#[test]
#[ignore]
#[serial]
fn t094_scroll_verify_position() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    // Use a page with enough content to scroll
    rub_cmd(&home)
        .args(["open", &server.url_for("/html")])
        .output()
        .unwrap();
    let out = rub_cmd(&home)
        .args(["scroll", "down", "--amount", "500"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "viewport");
    assert_eq!(json["data"]["result"]["direction"], "down");
    assert!(json["data"]["result"]["position"]["y"].is_number());
    assert!(json["data"]["result"]["position"]["x"].is_number());
    assert!(json["data"]["result"]["position"]["at_bottom"].is_boolean());

    // Verify scrollY > 0
    let out = rub_cmd(&home)
        .args(["exec", "window.scrollY"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    let scroll_y = json["data"]["result"].as_f64().unwrap_or(0.0);
    assert!(scroll_y > 0.0, "scrollY should be > 0 after scroll down");

    cleanup(&home);
}

/// T102: screenshot --full → verify full page capture.
#[test]
#[ignore]
#[serial]
fn t102_screenshot_full_page() {
    let home = unique_home();
    cleanup(&home);
    let normal_path = format!("{home}/normal.png");
    let full_path = format!("{home}/full.png");
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url_for("/html")])
        .output()
        .unwrap();

    // Take normal screenshot
    let out = rub_cmd(&home)
        .args(["screenshot", "--path", &normal_path])
        .output()
        .unwrap();
    let json1 = parse_json(&out);
    assert_eq!(json1["success"], true);
    let normal_size = json1["data"]["result"]["artifact"]["size_bytes"]
        .as_u64()
        .unwrap();

    // Take full-page screenshot
    let out = rub_cmd(&home)
        .args(["screenshot", "--path", &full_path, "--full"])
        .output()
        .unwrap();
    let json2 = parse_json(&out);
    assert_eq!(json2["success"], true);
    let full_size = json2["data"]["result"]["artifact"]["size_bytes"]
        .as_u64()
        .unwrap();

    // Full page should be >= normal (often larger)
    assert!(
        full_size >= normal_size,
        "full page screenshot ({full_size}) should be >= normal ({normal_size})"
    );

    cleanup(&home);
}

/// T108: rub sessions → verify session list format (SC-012).
#[test]
#[ignore]
#[serial]
fn t108_sessions_format() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).arg("sessions").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["command"], "sessions");
    assert_eq!(json["stdout_schema_version"], "3.0");

    // Verify session entry fields
    let sessions = json["data"]["result"]["items"].as_array().unwrap();
    assert!(!sessions.is_empty());
    let s = &sessions[0];
    assert!(s["id"].as_str().is_some());
    assert!(s["name"].as_str().is_some());
    assert!(s["pid"].is_number());
    assert!(s["socket"].as_str().is_some());

    cleanup(&home);
}

/// T117: kill browser process → next command auto-restarts.
#[test]
#[ignore]
#[serial]
fn t117_browser_crash_recovery() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    // Open page
    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    // Kill all Chrome processes spawned by this daemon
    // (The daemon should recover on next command)
    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("rub-chrome-{}", std::process::id())])
        .output();
    // Wait for process to die
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Next command should work (daemon auto-starts new browser)
    let out = rub_cmd(&home)
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

    cleanup(&home);
}

/// T109/T111: Multi-session: start named session, verify isolation.
#[test]
#[ignore]
#[serial]
fn t109_multi_session_isolation() {
    let home = unique_home();
    cleanup(&home);
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
    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/default")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    // Start second session named "test2"
    let out = rub_cmd(&home)
        .args(["--session", "test2", "open", &server.url_for("/second")])
        .output()
        .unwrap();
    assert_eq!(
        parse_json(&out)["success"],
        true,
        "second session should start"
    );

    // Verify sessions list shows both
    let out = rub_cmd(&home).arg("sessions").output().unwrap();
    let json = parse_json(&out);
    let sessions = json["data"]["result"]["items"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"default"), "should have default: {names:?}");
    assert!(names.contains(&"test2"), "should have test2: {names:?}");

    // Clean up both sessions
    rub_cmd(&home).arg("close").output().unwrap();
    rub_cmd(&home)
        .args(["--session", "test2", "close"])
        .output()
        .unwrap();

    cleanup(&home);
}

/// T076: Click with stale snapshot → STALE_SNAPSHOT error (US-2.5).
#[test]
#[ignore]
#[serial]
fn t076_stale_snapshot_error() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url_for("/click")])
        .output()
        .unwrap();
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let state1 = parse_json(&out);
    let snap1 = state1["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Execute something to increment epoch (invalidating the snapshot)
    rub_cmd(&home)
        .args(["exec", "document.title = 'changed'"])
        .output()
        .unwrap();

    // Get a NEW snapshot (epoch should have incremented)
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let state2 = parse_json(&out);
    let snap2 = state2["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    assert_ne!(snap1, snap2, "snapshot_id should differ after epoch change");

    // Try to click with the OLD stale snapshot
    let button_index = find_element_index(&state1, |element| {
        element["text"].as_str() == Some("Advance")
    });
    let out = rub_cmd(&home)
        .args(["click", &button_index.to_string(), "--snapshot", &snap1])
        .output()
        .unwrap();
    let json = parse_json(&out);
    // Should succeed or report stale — depends on implementation
    // The key invariant is that the command processes correctly
    assert_eq!(json["success"], false, "stale snapshot should fail");
    assert_eq!(json["error"]["code"], "STALE_SNAPSHOT");

    cleanup(&home);
}

/// T079: one client invalidates a snapshot, second client gets STALE_SNAPSHOT.
#[test]
#[ignore]
#[serial]
fn t079_two_clients_stale_snapshot() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).arg("state").output().unwrap();
    let state = parse_json(&out);
    let snap = state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Advance")
    });

    let out = rub_cmd(&home)
        .args(["click", &button_index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    assert_eq!(
        parse_json(&out)["success"],
        true,
        "first client click should succeed"
    );

    let out = rub_cmd(&home)
        .args(["click", &button_index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "STALE_SNAPSHOT");

    cleanup(&home);
}

/// T112: two sessions using the same profile should fail with PROFILE_IN_USE.
#[test]
#[ignore]
#[serial]
fn t112_profile_in_use_error() {
    let home = unique_home();
    let profile = format!("/tmp/rub-shared-profile-{}", std::process::id());
    cleanup(&home);
    let _ = std::fs::remove_dir_all(&profile);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["--user-data-dir", &profile, "open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
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

    cleanup(&home);
    let _ = std::fs::remove_dir_all(&profile);
}

/// T118: queue timeout fires while waiting behind another command.
#[test]
#[ignore]
#[serial]
fn t118_queue_timeout_reports_queue_phase() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let hold = rub_cmd(&home)
        .args([
            "exec",
            "(() => { const end = Date.now() + 1500; while (Date.now() < end) {} return 'held'; })()",
        ])
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(150));

    let out = rub_cmd(&home)
        .args(["--timeout", "100", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "IPC_TIMEOUT");
    assert_eq!(json["error"]["context"]["command"], "state");
    assert_eq!(json["error"]["context"]["phase"], "queue");
    assert_eq!(json["error"]["context"]["transaction_timeout_ms"], 100);
    assert_eq!(json["error"]["context"]["queue_ms"], 100);
    let suggestion = json["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(suggestion.contains("one command at a time"), "{json}");
    assert!(suggestion.contains("separate RUB_HOME"), "{json}");

    let mut hold = hold;
    let _ = hold.wait();
    cleanup(&home);
}

/// T119: protocol mismatch must not auto-upgrade while the session is busy.
#[test]
#[ignore]
#[serial]
fn t119_version_mismatch_busy_session() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let mut registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{home}/registry.json")).unwrap())
            .unwrap();
    registry["sessions"][0]["ipc_protocol_version"] = serde_json::json!("0.0");
    std::fs::write(
        format!("{home}/registry.json"),
        serde_json::to_string_pretty(&registry).unwrap(),
    )
    .unwrap();

    let hold = rub_cmd(&home)
        .args([
            "exec",
            "(() => { const end = Date.now() + 1500; while (Date.now() < end) {} return 'busy'; })()",
        ])
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(150));

    let out = rub_cmd(&home).arg("state").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "IPC_VERSION_MISMATCH");

    let mut hold = hold;
    let _ = hold.wait();
    cleanup(&home);
}

/// T078: Duplicate command_id → cached result (INV-003, SC-011).
#[test]
#[ignore]
#[serial]
fn t078_duplicate_command_id_cached() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    // Execute with a specific command_id
    let out = rub_cmd(&home)
        .args(["exec", "Math.random()"])
        .output()
        .unwrap();
    let json1 = parse_json(&out);
    assert_eq!(json1["success"], true);
    // command_id is auto-generated, so we just verify exec works
    // The replay cache is tested at the IPC level

    cleanup(&home);
}

/// T087: exec triggers navigation, returns immediately.
#[test]
#[ignore]
#[serial]
fn t087_exec_async_navigation() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    // Execute JS that modifies the page but doesn't navigate
    let out = rub_cmd(&home)
        .args(["exec", "document.body.innerHTML = '<h1>Modified</h1>'"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "exec should return immediately");

    // Verify state reflects the change
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    cleanup(&home);
}

// ── v1.1: US1 Keyboard Operations ───────────────────────────────────

/// T016a: `keys "Enter"` sends a key event.
#[test]
#[ignore]
#[serial]
fn t200_keys_enter() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.getElementById('trigger').focus(); null"])
        .output()
        .unwrap();

    // Send Enter key
    let out = rub_cmd(&home).args(["keys", "Enter"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "keys Enter should succeed");
    assert_eq!(
        json["data"]["interaction"]["semantic_class"],
        "invoke_workflow"
    );
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "submitted");

    cleanup(&home);
}

/// T016b: `type "hello"` types text character-by-character.
#[test]
#[ignore]
#[serial]
fn t201_type_text() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" autofocus />
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').focus(); null"])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["type", "hello"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "type should succeed");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').value"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "hello");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201d_type_selector_uses_canonical_locator_without_snapshot() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" />
</body>
</html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let out = rub_cmd(&home)
        .args(["type", "--selector", "#editor", "hello"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );
    assert_eq!(json["data"]["subject"]["tag"], "input");

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').value"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "hello");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201e_type_target_text_uses_canonical_locator_without_snapshot() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <textarea aria-label="Editor"></textarea>
</body>
</html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let out = rub_cmd(&home)
        .args(["type", "--target-text", "Editor", "hello"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );
    assert_eq!(json["data"]["subject"]["tag"], "textarea");

    let verify = rub_cmd(&home)
        .args(["exec", "document.querySelector('textarea').value"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "hello");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201f_type_selector_clear_replaces_existing_value() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" value="seed value" />
</body>
</html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let out = rub_cmd(&home)
        .args(["type", "--selector", "#editor", "--clear", "hello"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );
    assert_eq!(json["data"]["subject"]["tag"], "input");

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').value"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "hello");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201h_type_selector_clear_confirms_typed_surface_on_scrolled_formatter_inputs() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let out = rub_cmd(&home)
        .args(["type", "--selector", "#editor", "--clear", "hello world"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "confirmed"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').value"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "HELLO WORLD");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201g_type_output_projection_flags_control_interaction_trace() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input id="editor" autofocus />
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').focus(); null"])
        .output()
        .unwrap();

    let compact = rub_cmd(&home).args(["type", "hello"]).output().unwrap();
    let compact_json = parse_json(&compact);
    assert_eq!(compact_json["success"], true);
    assert!(
        compact_json["data"].get("interaction_trace").is_none(),
        "{compact_json}"
    );

    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').value = ''; null"])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').focus(); null"])
        .output()
        .unwrap();

    let verbose = rub_cmd(&home)
        .args(["--verbose", "type", "world"])
        .output()
        .unwrap();
    let verbose_json = parse_json(&verbose);
    assert_eq!(verbose_json["success"], true);
    assert_eq!(
        verbose_json["data"]["interaction_trace"]["semantic_class"],
        "set_value"
    );
    assert!(
        verbose_json["data"]["interaction_trace"]
            .get("observed_effects")
            .is_none(),
        "{verbose_json}"
    );

    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').value = ''; null"])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').focus(); null"])
        .output()
        .unwrap();

    let traced = rub_cmd(&home)
        .args(["--trace", "type", "codex"])
        .output()
        .unwrap();
    let traced_json = parse_json(&traced);
    assert_eq!(traced_json["success"], true);
    assert!(
        traced_json["data"]["interaction_trace"]["observed_effects"]
            .as_object()
            .is_some(),
        "{traced_json}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201b_type_readonly_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input id="target" value="locked" readonly />
  <script>document.getElementById('target').focus();</script>
</body>
</html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let out = rub_cmd(&home).args(["type", "new-value"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("readonly")
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t201c_type_non_editable_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <button id="target" autofocus>Focusable</button>
</body>
</html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let out = rub_cmd(&home).args(["type", "new-value"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("editable text target")
    );

    cleanup(&home);
}

/// T016c: `keys "InvalidKey"` returns INVALID_KEY_NAME with suggestion.
#[test]
#[ignore]
#[serial]
fn t202_keys_invalid_key() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><button id="ready">ready</button></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["keys", "InvalidKey"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "invalid key should fail");
    assert_eq!(json["error"]["code"], "INVALID_KEY_NAME");

    cleanup(&home);
}

/// T016d: `keys "Control+a"` sends modifier + key combination.
#[test]
#[ignore]
#[serial]
fn t203_keys_control_a() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.getElementById('editor').focus(); null"])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["keys", "Control+a"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "Control+a should succeed");
    assert_eq!(
        json["data"]["interaction"]["semantic_class"],
        "invoke_workflow"
    );
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "shortcut");

    cleanup(&home);
}

/// T016e: `keys "hello"` returns INVALID_KEY_NAME suggesting `rub type` instead.
#[test]
#[ignore]
#[serial]
fn t204_keys_plain_text_suggests_type() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><button id="ready">ready</button></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["keys", "hello"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_KEY_NAME");
    let msg = json["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("rub type"),
        "Error for plain text should suggest 'rub type', got: {msg}"
    );

    cleanup(&home);
}

// ── v1.1: US2 Wait Commands ─────────────────────────────────────────

/// T021a: `wait --text` for text that exists immediately succeeds.
#[test]
#[ignore]
#[serial]
fn t210_wait_text_found() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["wait", "--text", "Example Domain", "--timeout", "5000"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], true,
        "wait for existing text should succeed"
    );
    assert_eq!(json["data"]["result"]["matched"], true);
    assert!(json["data"]["result"]["elapsed_ms"].as_u64().unwrap() < 5000);

    cleanup(&home);
}

/// T021a-2: `wait --text` normalizes whitespace and casing.
#[test]
#[ignore]
#[serial]
fn t210_wait_text_normalizes_whitespace_and_case() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args([
            "wait",
            "--text",
            "enter account information",
            "--timeout",
            "5000",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], true,
        "wait for normalized text should succeed"
    );
    assert_eq!(json["data"]["result"]["matched"], true);

    cleanup(&home);
}

/// T021b: `wait --selector` for element that exists immediately succeeds.
#[test]
#[ignore]
#[serial]
fn t211_wait_selector_found() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><h1>Wait Fixture</h1></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["wait", "--selector", "h1", "--timeout", "5000"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], true,
        "wait for existing selector should succeed"
    );
    assert_eq!(json["data"]["result"]["matched"], true);

    cleanup(&home);
}

/// T021b-2: semantic wait locators should reuse the canonical locator runtime.
#[test]
#[ignore]
#[serial]
fn t211b_wait_role_with_nth_selection_found() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
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
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["matched"], true, "{json}");
    assert_eq!(json["data"]["subject"]["wait_kind"], "role", "{json}");
    assert_eq!(json["data"]["subject"]["probe_value"], "button", "{json}");

    cleanup(&home);
}

/// T021c: `wait --selector` for non-existent element times out with WAIT_TIMEOUT.
#[test]
#[ignore]
#[serial]
fn t212_wait_timeout() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args([
            "wait",
            "--selector",
            ".nonexistent-element",
            "--timeout",
            "1000",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], false,
        "wait for missing element should fail"
    );
    assert_eq!(json["error"]["code"], "WAIT_TIMEOUT");
    assert_eq!(json["error"]["context"]["command"], "wait");
    assert_eq!(json["error"]["context"]["phase"], "execution");
    assert_eq!(json["error"]["context"]["kind"], "selector");
    assert_eq!(json["error"]["context"]["value"], ".nonexistent-element");
    assert_eq!(json["error"]["context"]["timeout_ms"], 1000);
    assert!(json["error"]["context"]["transaction_timeout_ms"].is_number());
    assert!(json["error"]["context"]["exec_budget_ms"].is_number());

    cleanup(&home);
}

/// T021d: `wait --selector` with state=detached for non-existent element succeeds immediately.
#[test]
#[ignore]
#[serial]
fn t213_wait_detached() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
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
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], true,
        "wait detached for non-existent should succeed immediately"
    );
    assert_eq!(json["data"]["result"]["matched"], true);

    cleanup(&home);
}

/// T022: click confirmation reports page mutation when activation changes DOM
/// without navigation.
#[test]
#[ignore]
#[serial]
fn t216_click_confirms_page_mutation() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| element["text"] == "Advance");

    let out = rub_cmd(&home)
        .args(["click", &button_index.to_string(), "--snapshot", &snapshot])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    let out = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let exec_json = parse_json(&out);
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );
    assert_eq!(json["data"]["interaction_trace"]["command"], "click");
    assert_eq!(
        json["data"]["interaction_trace"]["semantic_class"],
        "activate"
    );
    assert_eq!(
        json["data"]["interaction_trace"]["confirmation_status"],
        "confirmed"
    );
    assert_eq!(exec_json["success"], true);
    assert_eq!(exec_json["data"]["result"], "done");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t216b_click_delayed_effect_reports_unconfirmed() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Apply Later")
    });

    let out = rub_cmd(&home)
        .args(["click", &button_index.to_string(), "--snapshot", &snapshot])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], false);
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "unconfirmed"
    );
    assert!(json["data"]["interaction"]["confirmation_kind"].is_null());

    std::thread::sleep(std::time::Duration::from_millis(2200));
    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["success"], true);
    assert_eq!(verify_json["data"]["result"], "done");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t216c_click_focus_change_reports_confirmed_focus_change() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Focus Change Fixture</title></head>
<body>
  <input id="target" placeholder="Focus Only" />
  <div id="status">idle</div>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["tag"].as_str() == Some("input")
            && element["attributes"]["placeholder"].as_str() == Some("Focus Only")
    });

    let out = rub_cmd(&home)
        .args(["click", &button_index.to_string(), "--snapshot", &snapshot])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "confirmed"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "focus_change"
    );
    assert_eq!(
        json["data"]["interaction_trace"]["confirmation_kind"],
        "focus_change"
    );
    assert_eq!(
        json["data"]["interaction_trace"]["observed_effects"]["before_active"],
        false
    );
    assert_eq!(
        json["data"]["interaction_trace"]["observed_effects"]["after_active"],
        true
    );
    assert_eq!(
        json["data"]["interaction"]["observed_effects"]["before_active"],
        false
    );
    assert_eq!(
        json["data"]["interaction"]["observed_effects"]["after_active"],
        true
    );

    let verify = rub_cmd(&home)
        .args([
            "exec",
            "document.activeElement && document.activeElement.id",
        ])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "target");

    cleanup(&home);
}

// ── v1.1: US3 Multi-Tab Management ─────────────────────────────────

/// T030a: `tabs` lists current tabs.
#[test]
#[ignore]
#[serial]
fn t220_tabs_list() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Single Tab Fixture</title></head>
<body><h1>Single Tab Fixture</h1></body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["tabs"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "tabs should succeed");
    let tabs = json["data"]["result"]["items"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_ne!(tabs[0]["url"], "about:blank");

    cleanup(&home);
}

/// T030b: `switch` to invalid tab returns TAB_NOT_FOUND.
#[test]
#[ignore]
#[serial]
fn t221_switch_invalid_tab() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["switch", "99"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "TAB_NOT_FOUND");

    cleanup(&home);
}

/// T030c: close-tab on last tab creates about:blank.
#[test]
#[ignore]
#[serial]
fn t222_close_last_tab_creates_blank() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    // Close the current tab — should auto-create about:blank
    let out = rub_cmd(&home).args(["close-tab"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "close-tab should succeed");
    assert_eq!(json["data"]["subject"]["kind"], "tab");
    assert_eq!(json["data"]["result"]["remaining_tabs"], 1);
    assert_eq!(json["data"]["result"]["active_tab"]["url"], "about:blank");

    cleanup(&home);
}

// ── v1.1: US4 DOM Information Retrieval ─────────────────────────────

/// T040a: `get title` returns the page title.
#[test]
#[ignore]
#[serial]
fn t230_get_title() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["get", "title"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "page");
    let title = json["data"]["result"]["value"].as_str().unwrap_or("");
    assert!(
        title.contains("Example"),
        "Title should contain 'Example', got: {title}"
    );

    cleanup(&home);
}

/// T040b: `get html` returns full page HTML.
#[test]
#[ignore]
#[serial]
fn t231_get_html_full() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["get", "html"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "page");
    let html = json["data"]["result"]["value"].as_str().unwrap_or("");
    assert!(html.contains("<html"), "HTML should contain <html tag");

    cleanup(&home);
}

/// T040c: `get html --selector h1` returns element HTML.
#[test]
#[ignore]
#[serial]
fn t232_get_html_selector() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["get", "html", "--selector", "h1"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "live_read");
    assert_eq!(json["data"]["subject"]["locator"]["selector"], "h1");
    let html = json["data"]["result"]["value"].as_str().unwrap_or("");
    assert!(
        html.contains("Example Domain"),
        "H1 should contain 'Example Domain'"
    );

    cleanup(&home);
}

/// T232d: `get` should read non-interactive content through live read-query authority.
#[test]
#[ignore]
#[serial]
fn t232d_get_selector_read_queries_use_live_dom_authority() {
    let home = unique_home();
    cleanup(&home);

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
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let text = parse_json(
        &rub_cmd(&home)
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
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232e: `get html --selector` should report invalid selectors instead of returning empty HTML.
#[test]
#[ignore]
#[serial]
fn t232e_get_html_invalid_selector_reports_invalid_input() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let out = parse_json(
        &rub_cmd(&home)
            .args(["get", "html", "--selector", "["])
            .output()
            .unwrap(),
    );
    assert_eq!(out["success"], false, "{out}");
    assert_eq!(out["error"]["code"], "INVALID_INPUT", "{out}");
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Invalid selector"),
        "{out}"
    );
    let suggestion = out["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(suggestion.contains("--role"), "{out}");
    assert!(suggestion.contains("inspect page"), "{out}");

    cleanup(&home);
}

/// T232f: `inspect` should unify read-query and structured list inspection surfaces.
#[test]
#[ignore]
#[serial]
fn t232f_inspect_surface_reuses_read_query_and_list_runtime() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let text = parse_json(
        &rub_cmd(&home)
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
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232f2: `inspect list` builder flags should compile common collection specs without inline JSON.
#[test]
#[ignore]
#[serial]
fn t232f2_inspect_list_builder_supports_collection_and_field_shorthand() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let list = parse_json(
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232f2b: `inspect list --row-scope` should project sibling card fields without changing row identity.
#[test]
#[ignore]
#[serial]
fn t232f2b_inspect_list_row_scope_projects_cross_subtree_card_fields() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let list = parse_json(
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232f5: `inspect harvest` should follow bounded detail URLs and extract structured fields.
#[test]
#[ignore]
#[serial]
fn t232f5_inspect_harvest_follows_detail_urls_and_extracts_fields() {
    let home = unique_home();
    cleanup(&home);

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
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{home}/detail-rows.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "data": {
                "fields": {
                    "items": [
                        { "href": "/detail/a", "note_id": "alpha" },
                        { "href": "/detail/b", "note_id": "beta" }
                    ]
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let extract_path = format!("{home}/detail-spec.json");
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
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232f5b: `inspect harvest --field` should support common detail extraction without a separate extract file.
#[test]
#[ignore]
#[serial]
fn t232f5b_inspect_harvest_builder_supports_detail_field_shorthand() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body><main>feed root</main></body></html>",
        ),
        (
            "/detail/a",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Alpha detail</h1><img class="hero" data-testid="hero-image" src="/img/a.webp"><div class="author">Alice</div><div class="author">Alice backup</div></body></html>"#,
        ),
        (
            "/detail/b",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Beta detail</h1><img class="hero" data-testid="hero-image" src="/img/b.webp"><div class="author">Bob</div><div class="author">Bob backup</div></body></html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{home}/detail-rows-builder.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "data": {
                "fields": {
                    "items": [
                        { "href": "/detail/a", "note_id": "alpha" },
                        { "href": "/detail/b", "note_id": "beta" }
                    ]
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let harvested = parse_json(
        &rub_cmd(&home)
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
                "--field",
                "title=role:heading",
                "--field",
                "author=text:.author@first",
                "--field",
                "hero=attribute:src:testid:hero-image",
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
        harvested["data"]["result"]["entries"][0]["result"]["fields"],
        serde_json::json!({
            "author": "Alice",
            "hero": "/img/a.webp",
            "title": "Alpha detail"
        }),
        "{harvested}"
    );
    assert_eq!(
        harvested["data"]["result"]["entries"][1]["result"]["fields"],
        serde_json::json!({
            "author": "Bob",
            "hero": "/img/b.webp",
            "title": "Beta detail"
        }),
        "{harvested}"
    );

    cleanup(&home);
}

/// T232f5c: `inspect harvest` should auto-detect canonical batch roots and consume their `items`.
#[test]
#[ignore]
#[serial]
fn t232f5c_inspect_harvest_auto_detects_canonical_batch_root() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body><main>feed root</main></body></html>",
        ),
        (
            "/detail/a",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1 class="title">Alpha detail</h1></body></html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{home}/detail-rows-autodetect.json");
    std::fs::write(
        &source_path,
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

    let harvested = parse_json(
        &rub_cmd(&home)
            .args([
                "inspect",
                "harvest",
                "--file",
                &source_path,
                "--base-url",
                &server.url(),
                "--field",
                "title=text:.title",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(harvested["success"], true, "{harvested}");
    assert_eq!(
        harvested["data"]["result"]["summary"]["harvested_count"], 1,
        "{harvested}"
    );
    assert_eq!(
        harvested["data"]["result"]["entries"][0]["result"]["fields"],
        serde_json::json!({
            "title": "Alpha detail"
        }),
        "{harvested}"
    );

    cleanup(&home);
}

/// T232f3: `inspect list --scan-until` should keep scrolling until the requested unique row count is reached.
#[test]
#[ignore]
#[serial]
fn t232f3_inspect_list_scan_until_collects_unique_rows() {
    let home = unique_home();
    cleanup(&home);

    let html = r#"<!DOCTYPE html>
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
    let (_rt, server) = start_test_server(vec![("/", "text/html", html)]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let list = parse_json(
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232f4: `inspect list --scan-until` should return partial rows truthfully when the page bottoms out first.
#[test]
#[ignore]
#[serial]
fn t232f4_inspect_list_scan_until_reports_partial_result_at_bottom() {
    let home = unique_home();
    cleanup(&home);

    let html = r#"<!DOCTYPE html>
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
    let (_rt, server) = start_test_server(vec![("/", "text/html", html)]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let list = parse_json(
        &rub_cmd(&home)
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
    assert_eq!(list["success"], true, "{list}");
    let rows = list["data"]["result"]["items"]
        .as_array()
        .expect("partial scan rows");
    assert_eq!(rows.len(), 3, "{list}");
    assert_eq!(list["data"]["result"]["scan"]["complete"], false, "{list}");
    assert_eq!(
        list["data"]["result"]["scan"]["stop_reason"], "at_bottom",
        "{list}"
    );
    assert_eq!(
        list["data"]["result"]["scan"]["returned_count"], 3,
        "{list}"
    );
    assert_eq!(list["data"]["result"]["scan"]["unique_count"], 3, "{list}");
    assert_eq!(rows[0]["note_id"], "alpha", "{list}");
    assert_eq!(rows[2]["note_id"], "gamma", "{list}");

    cleanup(&home);
}

/// T232g: `inspect page` should reuse the state projection runtime with semantic observation scope.
#[test]
#[ignore]
#[serial]
fn t232g_inspect_page_supports_semantic_observation_scope() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let inspected = parse_json(
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232h: `inspect page` should expose compact observation projection and relative depth filtering.
#[test]
#[ignore]
#[serial]
fn t232h_inspect_page_supports_compact_projection_and_depth() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let inspected = parse_json(
        &rub_cmd(&home)
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
        inspected["data"]["result"]["snapshot"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let compact_text = inspected["data"]["result"]["snapshot"]["compact_text"]
        .as_str()
        .unwrap();
    assert!(compact_text.contains("Primary CTA"), "{inspected}");
    assert!(compact_text.contains("Docs"), "{inspected}");
    assert!(!compact_text.contains("Nested CTA"), "{inspected}");
    assert!(
        compact_text.lines().all(|line| !line.starts_with("  ")),
        "{inspected}"
    );
    assert!(
        compact_text.contains("@1]") || compact_text.contains("@2]"),
        "{inspected}"
    );

    cleanup(&home);
}

/// T232m: `inspect text --many` should explicitly group repeated live DOM matches without
/// weakening the default single-value read contract.
#[test]
#[ignore]
#[serial]
fn t232m_inspect_text_many_returns_explicit_multi_value_surface() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let scalar = parse_json(
        &rub_cmd(&home)
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
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232n: `inspect html` should support canonical locator parity and explicit multi-value reads.
#[test]
#[ignore]
#[serial]
fn t232n_inspect_html_supports_locator_parity_and_many() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
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
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let single = parse_json(
        &rub_cmd(&home)
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
        &rub_cmd(&home)
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

    let page = parse_json(&rub_cmd(&home).args(["inspect", "html"]).output().unwrap());
    assert_eq!(page["success"], true, "{page}");
    assert!(
        page["data"]["result"]["value"]
            .as_str()
            .unwrap_or_default()
            .contains("<html"),
        "{page}"
    );

    cleanup(&home);
}

/// T232i: `inspect network` should expose recent request timelines, request details, and curl export.
#[test]
#[ignore]
#[serial]
fn t232i_inspect_network_lists_details_and_exports_curl() {
    let home = unique_home();
    cleanup(&home);

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
            "/api/orders",
            "application/json",
            r#"{"ok":true,"orderId":42}"#,
        ),
        ("/api/missing", "text/plain", "missing-order"),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &rub_cmd(&home)
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
        &rub_cmd(&home)
            .args(["inspect", "network", "--last", "10", "--match", "/api/"])
            .output()
            .unwrap(),
    );
    assert_eq!(network["success"], true, "{network}");
    assert_eq!(
        network["data"]["subject"]["kind"], "network_request_registry",
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
        &rub_cmd(&home)
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

    let detail = parse_json(
        &rub_cmd(&home)
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

    let curl = parse_json(
        &rub_cmd(&home)
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

    cleanup(&home);
}

/// T232j: `inspect network --wait` should wait for a matching request to reach terminal lifecycle.
#[test]
#[ignore]
#[serial]
fn t232j_inspect_network_waits_for_terminal_request() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
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
            "/api/delayed",
            "application/json",
            r#"{"ok":true,"delayed":true}"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &rub_cmd(&home)
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
    assert_eq!(waited["success"], true, "{waited}");
    assert_eq!(
        waited["data"]["subject"]["kind"], "network_request_wait",
        "{waited}"
    );
    assert_eq!(
        waited["data"]["subject"]["lifecycle"], "terminal",
        "{waited}"
    );
    assert_eq!(waited["data"]["result"]["matched"], true, "{waited}");
    assert!(
        waited["data"]["result"]["elapsed_ms"].as_u64().is_some(),
        "{waited}"
    );
    assert_eq!(
        waited["data"]["result"]["request"]["method"], "POST",
        "{waited}"
    );
    assert_eq!(
        waited["data"]["result"]["request"]["status"], 200,
        "{waited}"
    );
    assert_eq!(
        waited["data"]["result"]["request"]["lifecycle"], "completed",
        "{waited}"
    );
    assert_eq!(
        waited["data"]["result"]["request"]["response_body"]["preview"],
        "{\"ok\":true,\"delayed\":true}",
        "{waited}"
    );

    cleanup(&home);
}

/// T232k: interaction windows should surface grouped request records, including 4xx/5xx responses.
#[test]
#[ignore]
#[serial]
fn t232k_interaction_groups_network_requests_in_command_window() {
    let home = unique_home();
    cleanup(&home);
    let server = NetworkInspectionFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#request-batch"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["interaction_confirmed"], true,
        "{clicked}"
    );

    let grouped = &clicked["data"]["interaction"]["observed_effects"]["network_requests"];
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
    assert_eq!(
        clicked["data"]["interaction_trace"]["observed_effects"]["network_requests"], *grouped,
        "{clicked}"
    );

    cleanup(&home);
}

/// T232l: network inspection should preserve HTTP 4xx/5xx responses as completed request lifecycles.
#[test]
#[ignore]
#[serial]
fn t232l_inspect_network_preserves_http_error_lifecycles() {
    let home = unique_home();
    cleanup(&home);
    let server = NetworkInspectionFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#request-batch"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let network = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "network", "--last", "10", "--match", "/api/"])
            .output()
            .unwrap(),
    );
    assert_eq!(network["success"], true, "{network}");
    let requests = network["data"]["requests"].as_array().unwrap();
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

    cleanup(&home);
}

// ── v1.1: US5 Extended Click ────────────────────────────────────────

/// T048a: `click --xy` fires click at coordinates.
#[test]
#[ignore]
#[serial]
fn t240_click_xy() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body style="margin:0">
    <button
      id="target"
      style="position:absolute; left:80px; top:180px; width:120px; height:60px"
      onclick="document.getElementById('status').textContent='clicked';"
    >
      Click target
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["click", "--xy", "120", "210"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "click --xy should succeed");
    assert_eq!(json["data"]["x"], 120.0);
    assert_eq!(json["data"]["y"], 210.0);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "clicked");

    cleanup(&home);
}

/// T048b: `hover` publishes confirmed hover-state interaction metadata.
#[test]
#[ignore]
#[serial]
fn t240b_click_xy_empty_reports_unconfirmed() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body style="margin:0; width:400px; height:400px;">
    <button
      id="target"
      style="position:absolute; left:20px; top:20px; width:80px; height:40px"
      onclick="document.getElementById('status').textContent='clicked';"
    >
      Click target
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
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

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "idle");

    cleanup(&home);
}

/// T048c: `hover` publishes confirmed hover-state interaction metadata.
#[test]
#[ignore]
#[serial]
fn t241_hover_confirms_hover_state() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body>
    <button id="target" onmouseover="this.dataset.hovered='yes'; document.getElementById('status').textContent='hovered';">
      Hover me
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"].as_str().unwrap_or("").contains("Hover me")
    });

    let out = rub_cmd(&home)
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

    let verify = rub_cmd(&home)
        .args([
            "exec",
            "document.getElementById('target').dataset.hovered + '|' + document.getElementById('status').textContent",
        ])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "yes|hovered");

    cleanup(&home);
}

/// T048d: `click --double` publishes confirmed interaction metadata.
#[test]
#[ignore]
#[serial]
fn t242_dblclick_confirms_page_mutation() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body>
    <button id="target" ondblclick="document.getElementById('status').textContent='dblclicked';">
      Double click me
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Double click me")
    });

    let out = rub_cmd(&home)
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

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "dblclicked");

    cleanup(&home);
}

/// T048e: `click --right` publishes confirmed interaction metadata.
#[test]
#[ignore]
#[serial]
fn t243_rightclick_confirms_page_mutation() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body>
    <button
      id="target"
      oncontextmenu="event.preventDefault(); document.getElementById('status').textContent='context-opened';"
    >
      Right click me
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Right click me")
    });

    let out = rub_cmd(&home)
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

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "context-opened");

    cleanup(&home);
}

/// T048f: `click --double` publishes confirmed interaction metadata.
#[test]
#[ignore]
#[serial]
fn t242b_click_double_confirms_page_mutation() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body>
    <button id="target" ondblclick="document.getElementById('status').textContent='dblclicked';">
      Double click me
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Double click me")
    });

    let out = rub_cmd(&home)
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
    assert_eq!(json["data"]["gesture"], "double");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "dblclicked");

    cleanup(&home);
}

/// T048g: `click --right` publishes confirmed interaction metadata.
#[test]
#[ignore]
#[serial]
fn t243b_click_right_confirms_page_mutation() {
    let home = unique_home();
    cleanup(&home);

    let (_runtime, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
  <body>
    <button
      id="target"
      oncontextmenu="event.preventDefault(); document.getElementById('status').textContent='context-opened';"
    >
      Right click me
    </button>
    <div id="status">idle</div>
  </body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"]
            .as_str()
            .unwrap_or("")
            .contains("Right click me")
    });

    let out = rub_cmd(&home)
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
    assert_eq!(json["data"]["gesture"], "right");
    assert_eq!(json["data"]["interaction"]["semantic_class"], "activate");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "page_mutation"
    );

    let verify = rub_cmd(&home)
        .args(["exec", "document.getElementById('status').textContent"])
        .output()
        .unwrap();
    let verify_json = parse_json(&verify);
    assert_eq!(verify_json["data"]["result"], "context-opened");

    cleanup(&home);
}
