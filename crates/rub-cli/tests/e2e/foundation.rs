use super::*;

// ============================================================
// Phase 3: Session lifecycle tests (T039-T042)
// ============================================================

#[test]
#[serial]
fn t038b_close_noops_without_bootstrap() {
    let home = unique_home();
    cleanup(&home);

    let output = rub_cmd(&home).arg("close").output().unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["closed"], false, "{json}");
    assert_eq!(
        json["data"]["result"]["daemon_exit_policy"], "no_existing_daemon_authority",
        "{json}"
    );
    assert!(
        !std::path::Path::new(&home).exists(),
        "close must not create RUB_HOME when no daemon authority exists"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t039_daemon_auto_start_lifecycle() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    // open auto-starts daemon
    let output = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true, "open should succeed");

    // daemon PID file should exist
    assert!(default_session_pid_path(&home).exists());

    // close should work
    let output = rub_cmd(&home).arg("close").output().unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true, "close should succeed");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t040_stale_pid_recovery() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    // Start daemon
    let output = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&output)["success"], true);

    // Kill daemon brutally
    let pid_str = std::fs::read_to_string(default_session_pid_path(&home)).unwrap();
    let pid: i32 = pid_str.trim().parse().unwrap();
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Next command should auto-restart
    let output = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true, "should auto-restart after stale PID");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t042_sessions_list() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><head><title>Sessions Fixture</title></head><body><h1>Sessions Fixture</h1></body></html>"#,
    )]);

    // Start daemon
    let output = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&output)["success"], true);

    // List sessions
    let output = rub_cmd(&home).arg("sessions").output().unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true);
    let sessions = json["data"]["result"]["items"].as_array().unwrap();
    assert!(!sessions.is_empty(), "should have at least one session");
    assert_eq!(sessions[0]["name"], "default");

    cleanup(&home);
}

// ============================================================
// Phase 4: Navigate & inspect tests (T052-T057)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t052_open_url_json_response() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let output = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["page"]["url"]
            .as_str()
            .unwrap()
            .contains(&server.url())
    );
    assert_eq!(json["data"]["result"]["page"]["title"], "Example Domain");
    assert!(
        json["data"]["result"]["page"]["final_url"]
            .as_str()
            .unwrap()
            .contains(&server.url())
    );
    assert!(json["data"]["result"]["page"]["http_status"].is_number());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t052b_open_bare_localhost_url_applies_smart_completion() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();
    let bare = server
        .url()
        .strip_prefix("http://")
        .expect("test server should expose localhost http url")
        .to_string();

    let output = rub_cmd(&home).args(["open", &bare]).output().unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true, "{json}");
    assert!(
        json["data"]["result"]["page"]["url"]
            .as_str()
            .is_some_and(|url| url.contains(&server.url())),
        "{json}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t053_state_snapshot() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let output = rub_cmd(&home).arg("state").output().unwrap();
    let json = parse_json(&output);
    let snapshot = &json["data"]["result"]["snapshot"];
    assert_eq!(json["success"], true);
    assert!(
        snapshot["snapshot_id"].as_str().is_some(),
        "snapshot_id missing"
    );
    assert!(snapshot["dom_epoch"].is_number(), "dom_epoch missing");
    let elements = snapshot["elements"].as_array().unwrap();
    assert!(!elements.is_empty(), "should have elements");
    assert!(elements[0]["tag"].as_str().is_some());
    assert!(elements[0]["index"].is_number());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t054_state_limit() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url_for("/forms/post")])
        .output()
        .unwrap();
    let output = rub_cmd(&home)
        .args(["state", "--limit", "3"])
        .output()
        .unwrap();
    let json = parse_json(&output);
    let snapshot = &json["data"]["result"]["snapshot"];
    assert_eq!(json["success"], true);
    let elements = snapshot["elements"].as_array().unwrap();
    assert!(elements.len() <= 3, "should respect limit");
    assert_eq!(snapshot["truncated"], true);
    assert!(snapshot["total_count"].as_u64().unwrap() > 3);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t055_open_nonexistent_domain() {
    let home = unique_home();
    cleanup(&home);

    let output = rub_cmd(&home)
        .args(["open", "https://this-domain-does-not-exist-12345.invalid"])
        .output()
        .unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], false);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t057_e2e_binary_json() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let output = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Must be valid JSON
    let _: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be valid JSON");

    cleanup(&home);
}

// ============================================================
// Phase 5: Structured output tests (T063-T065)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t063_all_output_valid_json() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    // open
    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("open output must be JSON");

    // state
    let out = rub_cmd(&home).arg("state").output().unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("state output must be JSON");

    // exec
    let out = rub_cmd(&home).args(["exec", "1+1"]).output().unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("exec output must be JSON");

    // sessions
    let out = rub_cmd(&home).arg("sessions").output().unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("sessions output must be JSON");

    // screenshot
    let out = rub_cmd(&home).arg("screenshot").output().unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("screenshot output must be JSON");

    // close
    let out = rub_cmd(&home).arg("close").output().unwrap();
    let _: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("close output must be JSON");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t064_stdout_schema_version() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["stdout_schema_version"], "3.0");

    let out = rub_cmd(&home).arg("state").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["stdout_schema_version"], "3.0");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t065_request_id_uuid() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    let request_id = json["request_id"].as_str().unwrap();
    assert!(request_id.len() >= 32, "request_id should be a UUID");
    assert!(
        request_id.contains('-'),
        "request_id should be hyphenated UUID"
    );

    cleanup(&home);
}

// ============================================================
// Phase 6: Interaction tests (T074-T080)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t075_click_without_snapshot_uses_implicit_live_snapshot() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url_for("/click")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Advance")
    });
    // click without --snapshot should auto-capture a live snapshot in non-strict mode
    let out = rub_cmd(&home)
        .args(["click", &index.to_string()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"], "confirmed",
        "{json}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077_click_out_of_range() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let state_out = rub_cmd(&home).arg("state").output().unwrap();
    let state = parse_json(&state_out);
    let snap = state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();

    // Click way out of range
    let out = rub_cmd(&home)
        .args(["click", "999", "--snapshot", snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_FOUND");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077b_click_occluded_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/click-occluded",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body style="margin:0">
  <button id="target" style="position:absolute;left:40px;top:40px;width:160px;height:56px;">Occluded CTA</button>
  <div style="position:fixed;inset:0;background:rgba(0,0,0,0.35);z-index:999"></div>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/click-occluded")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Occluded CTA")
    });

    let out = rub_cmd(&home)
        .args(["click", &index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077c_click_hidden_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/click-hidden",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <button style="display:none">Hidden CTA</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/click-hidden")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Hidden CTA")
    });

    let out = rub_cmd(&home)
        .args(["click", &index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077ca_click_disabled_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);
    let url = "data:text/html,%3C%21DOCTYPE%20html%3E%3Chtml%3E%3Cbody%3E%3Cbutton%20disabled%3EDisabled%20CTA%3C/button%3E%3C/body%3E%3C/html%3E";

    rub_cmd(&home).args(["open", url]).output().unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["tag"].as_str() == Some("button")
            && element["text"].as_str() == Some("Disabled CTA")
    });

    let out = rub_cmd(&home)
        .args(["click", &index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077cb_click_aria_disabled_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);
    let url = "data:text/html,%3C%21DOCTYPE%20html%3E%3Chtml%3E%3Cbody%3E%3Ca%20href%3D%22%23next%22%20role%3D%22button%22%20aria-disabled%3D%22true%22%3EAria%20Disabled%20CTA%3C/a%3E%3C/body%3E%3C/html%3E";

    rub_cmd(&home).args(["open", url]).output().unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["tag"].as_str() == Some("link")
            && element["text"].as_str() == Some("Aria Disabled CTA")
    });

    let out = rub_cmd(&home)
        .args(["click", &index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077d_hover_occluded_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/hover-occluded",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body style="margin:0">
  <button id="target" style="position:absolute;left:40px;top:40px;width:160px;height:56px;">Occluded Hover</button>
  <div style="position:fixed;inset:0;background:rgba(0,0,0,0.35);z-index:999"></div>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/hover-occluded")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Occluded Hover")
    });

    let out = rub_cmd(&home)
        .args(["hover", &index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t077e_hover_hidden_target_reports_not_interactable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/hover-hidden",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <button id="target" style="display:none">Invisible Hover</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/hover-hidden")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Invisible Hover")
    });

    let out = rub_cmd(&home)
        .args(["hover", &index.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t080_input_text_into_form() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url_for("/forms/post")])
        .output()
        .unwrap();
    let state_out = rub_cmd(&home).arg("state").output().unwrap();
    let state = parse_json(&state_out);
    let snap = state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();

    // Type into first field (custname)
    let out = rub_cmd(&home)
        .args(["type", "--index", "0", "Test User", "--snapshot", snap])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert!(json["data"]["dom_epoch"].is_number());

    // Verify via JS
    let out = rub_cmd(&home)
        .args([
            "exec",
            "document.querySelector('input[name=custname]').value",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["data"]["result"], "Test User");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t081_type_reports_contradicted_effect() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/input-contradicted",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <input name="username" />
  <script>
    const input = document.querySelector('input[name=username]');
    input.addEventListener('input', () => {
      input.value = input.value.toUpperCase();
    });
  </script>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/input-contradicted")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snap = snapshot_id(&state);
    let index = find_element_index(&state, |element| {
        element["attributes"]["name"].as_str() == Some("username")
    });

    let out = rub_cmd(&home)
        .args([
            "type",
            "--index",
            &index.to_string(),
            "Test User",
            "--snapshot",
            &snap,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], false);
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "contradicted"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "value_applied"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_details"]["observed"]["value"],
        "TEST USER"
    );

    let out = rub_cmd(&home)
        .args(["exec", "document.querySelector('input').value"])
        .output()
        .unwrap();
    let verify = parse_json(&out);
    assert_eq!(verify["data"]["result"], "TEST USER");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t081b_type_disabled_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/input-disabled",
        "text/html",
        r#"<!doctype html>
<html><body>
  <input name="username" value="" disabled />
</body></html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/input-disabled")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["type", "--index", "0", "blocked", "--snapshot", snapshot])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t081c_type_readonly_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/input-readonly",
        "text/html",
        r#"<!doctype html>
<html><body>
  <input name="username" value="locked" readonly />
</body></html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/input-readonly")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let index = find_element_index(&state_json, |element| {
        element["tag"] == "input" && element["attributes"]["name"] == "username"
    });

    let out = rub_cmd(&home)
        .args([
            "type",
            "--index",
            &index.to_string(),
            "new-value",
            "--snapshot",
            snapshot,
        ])
        .output()
        .unwrap();
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
fn t081d_type_fieldset_disabled_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/input-fieldset-disabled",
        "text/html",
        r#"<!doctype html>
<html><body>
  <fieldset disabled>
    <input name="username" value="" />
  </fieldset>
</body></html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/input-fieldset-disabled")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let index = find_element_index(&state_json, |element| {
        element["tag"] == "input" && element["attributes"]["name"] == "username"
    });

    let out = rub_cmd(&home)
        .args([
            "type",
            "--index",
            &index.to_string(),
            "blocked",
            "--snapshot",
            snapshot,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );

    cleanup(&home);
}

// ============================================================
// Phase 7: JS execution tests (T084-T086)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t084_exec_valid_js() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home).args(["exec", "2 + 2"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"], 4);
    assert!(json["data"]["dom_epoch"].is_number());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t086_exec_undefined_returns_null() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home).args(["exec", "undefined"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t088_exec_non_json_result_is_summarized() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home)
        .args([
            "exec",
            &format!("window.open('{}', '_blank')", server.url()),
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"]["__rub_projection"], "summary");
    assert_eq!(json["data"]["result"]["kind"], "Window");

    let tabs = parse_json(&rub_cmd(&home).args(["tabs"]).output().unwrap());
    assert_eq!(tabs["success"], true);
    assert!(
        tabs["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default()
            >= 2,
        "window.open side effect should still create a new tab"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t088b_exec_raw_prints_result_without_json_envelope() {
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args(["exec", "--raw", "'The Page Title'"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "The Page Title"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t089_exec_timeout_reports_execution_phase() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home)
        .args([
            "--timeout",
            "100",
            "exec",
            "new Promise((resolve) => setTimeout(() => resolve('done'), 1000))",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "JS_TIMEOUT");
    assert_eq!(json["error"]["context"]["command"], "exec");
    assert_eq!(json["error"]["context"]["phase"], "execution");
    let timeout_ms = json["error"]["context"]["transaction_timeout_ms"]
        .as_u64()
        .expect("transaction timeout should be numeric");
    assert!(
        (1..=100).contains(&timeout_ms),
        "timeout should remain within the requested budget: {json}"
    );
    assert!(json["error"]["context"]["exec_budget_ms"].is_number());

    cleanup(&home);
}

// ============================================================
// Phase 8: Scroll & back tests (T093-T095)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t093_scroll_down() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url_for("/forms/post")])
        .output()
        .unwrap();
    let out = rub_cmd(&home).args(["scroll", "down"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "viewport");
    assert_eq!(json["data"]["result"]["direction"], "down");
    assert!(json["data"]["result"]["position"]["y"].is_number());
    assert!(json["data"]["dom_epoch"].is_number());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095_navigate_back() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["open", &server.url_for("/html")])
        .output()
        .unwrap();
    let out = rub_cmd(&home).arg("back").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["page"]["url"]
            .as_str()
            .unwrap()
            .contains(&server.url())
    );
    assert_eq!(json["data"]["subject"]["action"], "back");
    assert!(json["data"]["result"]["at_start"].is_boolean());
    assert!(json["data"]["dom_epoch"].is_number());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095b_navigate_forward() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["open", &server.url_for("/html")])
        .output()
        .unwrap();
    rub_cmd(&home).arg("back").output().unwrap();

    let out = rub_cmd(&home).arg("forward").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert!(
        json["data"]["result"]["page"]["url"]
            .as_str()
            .is_some_and(|url| url.contains(&server.url_for("/html"))),
        "{json}"
    );
    assert_eq!(json["data"]["subject"]["action"], "forward");
    assert!(json["data"]["result"]["at_end"].is_boolean(), "{json}");
    assert!(json["data"]["dom_epoch"].is_number(), "{json}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095f_back_and_forward_report_same_tab_history_summary() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/alpha",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Alpha History Page</title></head><body><h1>Alpha</h1></body></html>"#,
        ),
        (
            "/beta",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Beta History Page</title></head><body><h1>Beta</h1></body></html>"#,
        ),
    ]);

    let alpha = server.url_for("/alpha");
    let beta = server.url_for("/beta");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &alpha]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &beta]).output().unwrap())["success"],
        true
    );

    let back = parse_json(&rub_cmd(&home).arg("back").output().unwrap());
    assert_eq!(back["success"], true, "{back}");
    assert_eq!(
        back["data"]["result"]["page"]["title"], "Alpha History Page",
        "{back}"
    );
    assert_eq!(back["data"]["result"]["page"]["url"], alpha, "{back}");
    assert_eq!(back["data"]["result"]["page"]["final_url"], alpha, "{back}");

    let forward = parse_json(&rub_cmd(&home).arg("forward").output().unwrap());
    assert_eq!(forward["success"], true, "{forward}");
    assert_eq!(
        forward["data"]["result"]["page"]["title"], "Beta History Page",
        "{forward}"
    );
    assert_eq!(forward["data"]["result"]["page"]["url"], beta, "{forward}");
    assert_eq!(
        forward["data"]["result"]["page"]["final_url"], beta,
        "{forward}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095g_back_summary_stays_on_active_tab_with_background_tab_present() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/alpha",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Alpha History Page</title></head><body><h1>Alpha</h1></body></html>"#,
        ),
        (
            "/beta",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Beta History Page</title></head><body><h1>Beta</h1></body></html>"#,
        ),
        (
            "/popup",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Popup Context</title></head><body><h1>Popup</h1></body></html>"#,
        ),
    ]);

    let alpha = server.url_for("/alpha");
    let beta = server.url_for("/beta");
    let popup = server.url_for("/popup");

    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &alpha]).output().unwrap())["success"],
        true
    );
    assert_eq!(
        parse_json(&rub_cmd(&home).args(["open", &beta]).output().unwrap())["success"],
        true
    );

    let popup_open = parse_json(
        &rub_cmd(&home)
            .args(["exec", &format!("window.open('{popup}', '_blank')")])
            .output()
            .unwrap(),
    );
    assert_eq!(popup_open["success"], true, "{popup_open}");

    let tabs = (0..30)
        .find_map(|_| {
            let out = parse_json(&rub_cmd(&home).arg("tabs").output().unwrap());
            if out["data"]["result"]["items"]
                .as_array()
                .map(|items| items.len() as u64)
                .unwrap_or(0)
                >= 2
            {
                Some(out)
            } else {
                std::thread::sleep(Duration::from_millis(100));
                None
            }
        })
        .expect("popup tab should appear");
    let beta_index = tabs["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["title"] == "Beta History Page")
        .and_then(|tab| tab["index"].as_u64())
        .expect("beta tab should exist")
        .to_string();

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &beta_index])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(
        switched["data"]["result"]["active_tab"]["title"],
        "Beta History Page"
    );

    let back = parse_json(&rub_cmd(&home).arg("back").output().unwrap());
    assert_eq!(back["success"], true, "{back}");
    assert_eq!(
        back["data"]["result"]["page"]["title"], "Alpha History Page",
        "{back}"
    );
    assert_eq!(back["data"]["result"]["page"]["url"], alpha, "{back}");
    assert_eq!(back["data"]["result"]["page"]["final_url"], alpha, "{back}");

    let forward = parse_json(&rub_cmd(&home).arg("forward").output().unwrap());
    assert_eq!(forward["success"], true, "{forward}");
    assert_eq!(
        forward["data"]["result"]["page"]["title"], "Beta History Page",
        "{forward}"
    );
    assert_eq!(forward["data"]["result"]["page"]["url"], beta, "{forward}");
    assert_eq!(
        forward["data"]["result"]["page"]["final_url"], beta,
        "{forward}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095c_reload_restores_page_dom() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Reload Fixture</title></head>
<body>
  <h1 id="title">Original</h1>
  <button id="change" onclick="document.getElementById('title').textContent='Changed'">Change</button>
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

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#change"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let changed = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('title').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(changed["success"], true, "{changed}");
    assert_eq!(changed["data"]["result"], "Changed", "{changed}");

    let reloaded = parse_json(
        &rub_cmd(&home)
            .args(["reload", "--wait-after-text", "Original"])
            .output()
            .unwrap(),
    );
    assert_eq!(reloaded["success"], true, "{reloaded}");
    assert!(reloaded["data"]["dom_epoch"].is_number(), "{reloaded}");

    let restored = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('title').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(restored["success"], true, "{restored}");
    assert_eq!(restored["data"]["result"], "Original", "{restored}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095d_dialog_runtime_surfaces_pending_alert_and_dismisses() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/dialog-alert",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Dialog Fixture</title></head>
<body>
  <button id="trigger" onclick="setTimeout(() => alert('Hello!'), 0)">Show Alert</button>
</body>
</html>"#,
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/dialog-alert")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#trigger"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let pending = wait_for_pending_dialog(&home);
    assert_eq!(pending["data"]["runtime"]["status"], "active", "{pending}");
    assert_eq!(
        pending["data"]["runtime"]["pending_dialog"]["kind"], "alert",
        "{pending}"
    );
    assert_eq!(
        pending["data"]["runtime"]["pending_dialog"]["message"], "Hello!",
        "{pending}"
    );

    let runtime = parse_json(&rub_cmd(&home).args(["runtime", "dialog"]).output().unwrap());
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["pending_dialog"]["kind"], "alert",
        "{runtime}"
    );

    let dismissed = parse_json(&rub_cmd(&home).args(["dialog", "dismiss"]).output().unwrap());
    assert_eq!(dismissed["success"], true, "{dismissed}");
    assert!(
        dismissed["data"]["result"]["pending_dialog"].is_null(),
        "{dismissed}"
    );
    assert_eq!(
        dismissed["data"]["result"]["last_result"]["accepted"], false,
        "{dismissed}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t095e_dialog_accept_prompt_records_user_input() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/dialog-prompt",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Prompt Fixture</title></head>
<body>
  <button id="trigger" onclick="setTimeout(() => { document.body.dataset.prompt = prompt('Enter value:', 'seed') ?? 'null'; }, 0)">Show Prompt</button>
</body>
</html>"#,
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/dialog-prompt")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#trigger"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let pending = wait_for_pending_dialog(&home);
    assert_eq!(
        pending["data"]["runtime"]["pending_dialog"]["kind"], "prompt",
        "{pending}"
    );
    assert_eq!(
        pending["data"]["runtime"]["pending_dialog"]["default_prompt"], "seed",
        "{pending}"
    );

    let accepted = parse_json(
        &rub_cmd(&home)
            .args(["dialog", "accept", "--prompt-text", "typed"])
            .output()
            .unwrap(),
    );
    assert_eq!(accepted["success"], true, "{accepted}");
    assert!(
        accepted["data"]["result"]["pending_dialog"].is_null(),
        "{accepted}"
    );
    assert_eq!(
        accepted["data"]["result"]["last_result"]["accepted"], true,
        "{accepted}"
    );
    assert_eq!(
        accepted["data"]["result"]["last_result"]["user_input"], "typed",
        "{accepted}"
    );

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["wait", "--selector", "body[data-prompt='typed']"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let value = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.body.dataset.prompt"])
            .output()
            .unwrap(),
    );
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(value["data"]["result"], "typed", "{value}");

    cleanup(&home);
}

// ============================================================
// Phase 9: Screenshot tests (T100-T102)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t100_screenshot_to_file() {
    let home = unique_home();
    cleanup(&home);
    let screenshot_path = format!("{home}/test.png");
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home)
        .args(["screenshot", "--path", &screenshot_path])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["artifact"]["size_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(std::path::Path::new(&screenshot_path).exists());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t101_screenshot_base64() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let out = rub_cmd(&home).arg("screenshot").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["artifact"]["base64"]
            .as_str()
            .is_some(),
        "should have base64 data"
    );
    assert!(
        json["data"]["result"]["artifact"]["size_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );

    cleanup(&home);
}

// ============================================================
// Phase 10: Doctor test (T107-T108)
// ============================================================

#[test]
#[ignore]
#[serial]
fn t107_doctor_health() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/doctor",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1>Doctor Fixture</h1></body></html>"#,
        ),
        ("/doctor-data", "application/json", r#"{"ok":true}"#),
    ]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/doctor")])
        .output()
        .unwrap();
    let exec = rub_cmd(&home)
        .args([
            "exec",
            "console.error('doctor-health'); fetch('/doctor-data').then((r) => r.text())",
        ])
        .output()
        .unwrap();
    let exec_json = parse_json(&exec);
    assert_eq!(exec_json["success"], true, "{exec_json}");

    let out = rub_cmd(&home).arg("doctor").output().unwrap();
    let json = parse_json(&out);
    let report = doctor_result(&json);
    let runtime = doctor_runtime(&json);
    assert_eq!(json["success"], true);
    assert_eq!(report["daemon"]["running"], true);
    assert_eq!(report["browser"]["healthy"], true);
    assert!(report["daemon"]["pid"].is_number());
    assert!(report["disk"]["log_size_mb"].is_number());
    assert!(report["launch_policy"]["headless"].is_boolean());
    assert!(report["launch_policy"]["hide_infobars"].is_boolean());
    assert_eq!(runtime["integration_runtime"]["mode"], "normal");
    assert_eq!(runtime["integration_runtime"]["status"], "active");
    assert_eq!(runtime["integration_runtime"]["request_rule_count"], 0);
    assert_eq!(runtime["integration_runtime"]["request_rules"], json!([]));
    assert_eq!(runtime["interference_runtime"]["mode"], "normal");
    assert_eq!(runtime["interference_runtime"]["status"], "inactive");
    assert_eq!(
        runtime["interference_runtime"]["current_interference"],
        serde_json::Value::Null
    );
    assert_eq!(
        runtime["interference_runtime"]["active_policies"],
        json!([])
    );
    assert_eq!(runtime["frame_runtime"]["status"], "top");
    assert_eq!(runtime["frame_runtime"]["current_frame"]["depth"], 0);
    assert_eq!(
        runtime["frame_runtime"]["current_frame"]["same_origin_accessible"],
        true
    );
    assert_eq!(runtime["dialog_runtime"]["status"], "inactive");
    assert!(runtime["dialog_runtime"]["pending_dialog"].is_null());
    assert_eq!(runtime["storage_runtime"]["status"], "active");
    assert!(
        runtime["storage_runtime"]["current_origin"].is_string(),
        "{json}"
    );
    assert_eq!(runtime["runtime_observatory"]["status"], "active");
    assert!(
        runtime["runtime_observatory"]["recent_console_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["message"].as_str() == Some("doctor-health"))
    );
    assert_eq!(
        runtime["runtime_observatory"]["recent_page_errors"],
        json!([])
    );
    assert!(
        runtime["runtime_observatory"]["recent_network_failures"].is_array(),
        "recent_network_failures should remain a structured array surface"
    );
    assert!(
        runtime["runtime_observatory"]["recent_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["url"]
                    .as_str()
                    .is_some_and(|url| url.ends_with("/doctor") || url.ends_with("/doctor-data"))
            })
    );
    assert_eq!(runtime["state_inspector"]["status"], "active");
    assert_eq!(runtime["state_inspector"]["auth_state"], "anonymous");
    assert_eq!(runtime["state_inspector"]["cookie_count"], 0);
    assert_eq!(runtime["state_inspector"]["local_storage_keys"], json!([]));
    assert_eq!(
        runtime["state_inspector"]["session_storage_keys"],
        json!([])
    );
    assert_eq!(runtime["readiness_state"]["status"], "active");
    assert_eq!(runtime["readiness_state"]["route_stability"], "stable");
    assert_eq!(runtime["readiness_state"]["loading_present"], false);
    assert_eq!(runtime["readiness_state"]["skeleton_present"], false);
    assert_eq!(runtime["readiness_state"]["overlay_state"], "none");
    assert_eq!(
        runtime["human_verification_handoff"]["status"],
        "unavailable"
    );
    assert_eq!(
        runtime["human_verification_handoff"]["automation_paused"],
        false
    );
    assert_eq!(
        runtime["human_verification_handoff"]["resume_supported"],
        false
    );
    assert_eq!(
        runtime["human_verification_handoff"]["unavailable_reason"],
        "session_not_user_accessible"
    );
    assert!(
        runtime["orchestration_runtime"]["current_session_id"]
            .as_str()
            .is_some(),
        "{json}"
    );
    assert_eq!(
        runtime["orchestration_runtime"]["current_session_name"],
        "default"
    );
    assert_eq!(
        runtime["orchestration_runtime"]["addressing_supported"],
        true
    );
    assert_eq!(
        runtime["orchestration_runtime"]["execution_supported"],
        true
    );
    assert!(
        runtime["orchestration_runtime"]["session_count"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "{json}"
    );
    assert_eq!(runtime["integration_runtime"]["handoff_ready"], false);
    assert_eq!(
        report["capabilities"]["integration_runtime_projection"],
        true
    );
    assert_eq!(report["capabilities"]["frame_runtime_projection"], true);
    assert_eq!(report["capabilities"]["network_rule_projection"], true);
    assert_eq!(
        report["capabilities"]["runtime_observatory_projection"],
        true
    );
    assert_eq!(report["capabilities"]["state_inspector_projection"], true);
    assert_eq!(report["capabilities"]["readiness_projection"], true);
    assert_eq!(
        report["capabilities"]["human_verification_handoff_projection"],
        true
    );
    assert_eq!(
        report["capabilities"]["interference_runtime_projection"],
        true
    );
    assert_eq!(
        report["capabilities"]["orchestration_runtime_projection"],
        true
    );
    assert_eq!(report["capabilities"]["non_blocking_wait"], true);
    assert_eq!(report["capabilities"]["startup_locking"], true);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t108_legacy_automation_compat_mode_is_rejected() {
    let home = unique_home();
    cleanup(&home);

    let output = rub_cmd(&home)
        .args(["--automation-compat-mode", "doctor"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--automation-compat-mode"));

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t372_humanize_doctor_reports_l2() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/humanize",
        "text/html",
        r#"<!DOCTYPE html><html><body><button>Humanize</button></body></html>"#,
    )]);

    let out = rub_cmd(&home)
        .args([
            "--humanize",
            "--humanize-speed",
            "slow",
            "open",
            &server.url_for("/humanize"),
        ])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["--humanize", "--humanize-speed", "slow", "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    let report = doctor_result(&json);
    assert_eq!(json["success"], true);
    assert_eq!(report["launch_policy"]["stealth_level"], "L2");
    assert_eq!(report["launch_policy"]["humanize_enabled"], true);
    assert_eq!(report["launch_policy"]["humanize_speed"], "slow");
    let risk = &report["detection_risks"][0];
    assert!(risk["risk"].is_string());
    assert!(risk["severity"].is_string());
    assert!(risk["mitigation"].is_string());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t373_default_stealth_masks_headless_user_agent() {
    let home = unique_home();
    cleanup(&home);

    let (url, request_rx, handle) = start_header_capture_server();

    let out = rub_cmd(&home).args(["open", &url]).output().unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let request = request_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("server should capture the first request");
    assert!(request.contains("User-Agent:"));
    assert!(
        !request.contains("HeadlessChrome"),
        "network-layer User-Agent should not leak HeadlessChrome: {request}"
    );

    let out = rub_cmd(&home)
        .args(["exec", "navigator.userAgent"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        !json["data"]["result"]
            .as_str()
            .unwrap_or_default()
            .contains("HeadlessChrome")
    );

    let _ = handle.join();
    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t374_state_does_not_trigger_data_rub_mutation_observer() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/mutations",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head>
  <title>Mutation Fixture</title>
  <script>
    window.__rubMutations = [];
    const observer = new MutationObserver((records) => {
      for (const record of records) {
        if (record.type === 'attributes' && record.attributeName && record.attributeName.startsWith('data-rub-')) {
          window.__rubMutations.push(record.attributeName);
        }
      }
    });
    window.addEventListener('DOMContentLoaded', () => {
      observer.observe(document.documentElement, { attributes: true, subtree: true, attributeOldValue: true });
    });
  </script>
</head>
<body>
  <button>Probe</button>
</body>
</html>"#,
    )]);

    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/mutations")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home).arg("state").output().unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["exec", "window.__rubMutations.length"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"], 0);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t375_state_leaves_no_data_rub_attributes() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/data-rub-cleanup",
        "text/html",
        r#"<!DOCTYPE html><html><body><button>Probe</button></body></html>"#,
    )]);

    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/data-rub-cleanup")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home).arg("state").output().unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args([
            "exec",
            "document.querySelectorAll('[data-rub-node-index],[data-rub-highlight]').length",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"], 0);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t376_default_stealth_hides_webdriver() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["exec", "navigator.webdriver"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(json["data"]["result"].is_null());

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t383_default_stealth_removes_webdriver_property_shape() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let probe = r#"(() => ({
        read_type: typeof navigator.webdriver,
        in_navigator: ('webdriver' in navigator),
        own_property: Object.prototype.hasOwnProperty.call(navigator, 'webdriver'),
        proto_property: Object.prototype.hasOwnProperty.call(Object.getPrototypeOf(navigator), 'webdriver'),
        proto_descriptor_present: Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), 'webdriver') !== undefined
    }))()"#;
    let out = rub_cmd(&home).args(["exec", probe]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"]["read_type"], "undefined");
    assert_eq!(json["data"]["result"]["in_navigator"], false);
    assert_eq!(json["data"]["result"]["own_property"], false);
    assert_eq!(json["data"]["result"]["proto_property"], false);
    assert_eq!(json["data"]["result"]["proto_descriptor_present"], false);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t377_default_stealth_exposes_chrome_runtime() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["exec", "typeof chrome.runtime"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"], "object");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t378_default_stealth_exposes_plugins() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["exec", "navigator.plugins.length"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(json["data"]["result"].as_u64().unwrap_or(0) > 0);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t379_no_stealth_restores_webdriver() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["--no-stealth", "open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["--no-stealth", "exec", "navigator.webdriver"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"], true);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t380_doctor_reports_default_l1_with_clean_args_patch() {
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home).arg("doctor").output().unwrap();
    let json = parse_json(&out);
    let report = doctor_result(&json);
    assert_eq!(json["success"], true);
    assert_eq!(report["launch_policy"]["stealth_level"], "L1");
    assert_eq!(report["launch_policy"]["stealth_default_enabled"], true);
    let patches = report["launch_policy"]["stealth_patches"]
        .as_array()
        .unwrap();
    assert!(patches.iter().any(|value| value == "clean_chrome_args"));

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t384_doctor_reports_stealth_coverage_after_open() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><div>Stealth Coverage Fixture</div></body></html>"#,
    )]);

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home).arg("doctor").output().unwrap();
    let json = parse_json(&out);
    let report = doctor_result(&json);
    assert_eq!(json["success"], true);
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["coverage_mode"],
        "page_frame_worker_bridge"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["user_agent_override"],
        true
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["user_agent_metadata_override"],
        true
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["page_main_world"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["ua_consistency"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["webgl_surface"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["canvas_surface"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["audio_surface"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["permissions_surface"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["viewport_surface"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["touch_surface"],
        "passed"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["window_metrics_surface"],
        "passed"
    );
    assert!(
        matches!(
            report["launch_policy"]["stealth_coverage"]["self_probe"]["iframe_context"].as_str(),
            Some("passed" | "unknown")
        ),
        "{json}"
    );
    assert!(
        matches!(
            report["launch_policy"]["stealth_coverage"]["self_probe"]["worker_context"].as_str(),
            Some("passed" | "unknown")
        ),
        "{json}"
    );
    assert_eq!(
        report["launch_policy"]["stealth_coverage"]["self_probe"]["unsupported_surfaces"],
        json!(["service_worker"])
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t384b_default_stealth_projects_platform_consistent_webgl_profile() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let probe = r#"(() => {
        const canvas = document.createElement('canvas');
        const gl = canvas.getContext('webgl') || canvas.getContext('experimental-webgl');
        if (!gl) return { supported: false };
        const ext = gl.getExtension('WEBGL_debug_renderer_info');
        if (!ext) return { supported: false };
        return {
            supported: true,
            vendor: gl.getParameter(ext.UNMASKED_VENDOR_WEBGL),
            renderer: gl.getParameter(ext.UNMASKED_RENDERER_WEBGL)
        };
    })()"#;
    let out = rub_cmd(&home).args(["exec", probe]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"]["supported"], true);

    let renderer = json["data"]["result"]["renderer"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !renderer.contains("GTX 1650"),
        "renderer should no longer be the old fixed NVIDIA string: {renderer}"
    );
    match std::env::consts::OS {
        "macos" => {
            assert!(renderer.contains("Metal Renderer"), "{renderer}");
            assert!(!renderer.contains("Direct3D11"), "{renderer}");
        }
        "windows" => assert!(renderer.contains("Direct3D11"), "{renderer}"),
        "linux" => assert!(renderer.contains("OpenGL"), "{renderer}"),
        _ => assert!(!renderer.is_empty()),
    }

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t384c_default_stealth_stabilizes_canvas_and_audio_fingerprints() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let probe = r#"(() => {
        const canvas = document.createElement('canvas');
        canvas.width = 2;
        canvas.height = 1;
        const ctx = canvas.getContext('2d');
        ctx.fillStyle = 'rgba(10,20,30,1)';
        ctx.fillRect(0, 0, 2, 1);
        const firstPixels = Array.from(ctx.getImageData(0, 0, 1, 1).data);
        const secondPixels = Array.from(ctx.getImageData(0, 0, 1, 1).data);
        const firstDataUrl = canvas.toDataURL();
        const secondDataUrl = canvas.toDataURL();

        const audioContext = new OfflineAudioContext(1, 32, 44100);
        const buffer = audioContext.createBuffer(1, 32, 44100);
        const firstAudio = Array.from(buffer.getChannelData(0));
        const secondAudio = Array.from(buffer.getChannelData(0));
        const nonZeroIndices = firstAudio
            .map((value, index) => ({ value, index }))
            .filter((entry) => Math.abs(entry.value) > 0.0000001)
            .map((entry) => entry.index);
        return {
            canvas_stable: JSON.stringify(firstPixels) === JSON.stringify(secondPixels) && firstDataUrl === secondDataUrl,
            canvas_changed: JSON.stringify(firstPixels) !== JSON.stringify([10, 20, 30, 255]),
            audio_stable: JSON.stringify(firstAudio) === JSON.stringify(secondAudio),
            audio_changed: nonZeroIndices.length >= 2,
        };
    })()"#;
    let out = rub_cmd(&home).args(["exec", probe]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"]["canvas_stable"], true);
    assert_eq!(json["data"]["result"]["canvas_changed"], true);
    assert_eq!(json["data"]["result"]["audio_stable"], true);
    assert_eq!(json["data"]["result"]["audio_changed"], true);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t384d_default_stealth_projects_desktop_environment_consistency() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let probe = r#"(() => ({
        screen_width: Number(screen.width || 0),
        screen_height: Number(screen.height || 0),
        device_pixel_ratio: Number(window.devicePixelRatio || 0),
        max_touch_points: Number(navigator.maxTouchPoints || 0),
        has_ontouchstart: ('ontouchstart' in window),
        inner_width: Number(window.innerWidth || 0),
        inner_height: Number(window.innerHeight || 0),
        outer_width: Number(window.outerWidth || 0),
        outer_height: Number(window.outerHeight || 0)
    }))()"#;
    let out = rub_cmd(&home).args(["exec", probe]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    assert_ne!(json["data"]["result"]["screen_width"], 800);
    assert_ne!(json["data"]["result"]["screen_height"], 600);
    assert_eq!(json["data"]["result"]["max_touch_points"], 0);
    assert_eq!(json["data"]["result"]["has_ontouchstart"], false);
    assert!(
        json["data"]["result"]["outer_width"]
            .as_f64()
            .unwrap_or_default()
            >= json["data"]["result"]["inner_width"]
                .as_f64()
                .unwrap_or_default()
    );
    assert!(
        json["data"]["result"]["outer_height"]
            .as_f64()
            .unwrap_or_default()
            >= json["data"]["result"]["inner_height"]
                .as_f64()
                .unwrap_or_default()
    );

    match std::env::consts::OS {
        "macos" => {
            assert!(
                json["data"]["result"]["screen_width"]
                    .as_u64()
                    .unwrap_or_default()
                    >= 1440
            );
            assert!(
                json["data"]["result"]["device_pixel_ratio"]
                    .as_f64()
                    .unwrap_or_default()
                    >= 1.99
            );
        }
        _ => {
            assert!(
                json["data"]["result"]["screen_width"]
                    .as_u64()
                    .unwrap_or_default()
                    >= 1366
            );
            assert!(
                json["data"]["result"]["device_pixel_ratio"]
                    .as_f64()
                    .unwrap_or_default()
                    >= 0.99
            );
        }
    }

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t384e_default_stealth_cloaks_permissions_query_and_touch_getters() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let probe = r#"(() => {
        const query = Permissions.prototype.query;
        const querySource = String(Function.prototype.toString.call(query));
        const userAgentOwnDescriptor =
            Object.getOwnPropertyDescriptor(navigator, 'userAgent');
        const userAgentProtoDescriptor =
            Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), 'userAgent');
        const userAgentSource =
            userAgentProtoDescriptor && typeof userAgentProtoDescriptor.get === 'function'
                ? String(Function.prototype.toString.call(userAgentProtoDescriptor.get))
                : '';
        const touchOwnDescriptor =
            Object.getOwnPropertyDescriptor(navigator, 'maxTouchPoints');
        const touchDescriptor =
            Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator), 'maxTouchPoints');
        const touchSource =
            touchDescriptor && typeof touchDescriptor.get === 'function'
                ? String(Function.prototype.toString.call(touchDescriptor.get))
                : '';
        return {
            query_native: /\[native code\]/.test(querySource),
            query_leaks_patch: /notifications|Promise\.resolve|originalQuery|wrappedQuery/.test(querySource),
            query_name: String(query.name || ''),
            query_length: Number(query.length || 0),
            user_agent_own_descriptor_absent: userAgentOwnDescriptor === undefined,
            user_agent_proto_getter_native: userAgentSource ? /\[native code\]/.test(userAgentSource) : true,
            touch_own_descriptor_absent: touchOwnDescriptor === undefined,
            touch_getter_native: touchSource ? /\[native code\]/.test(touchSource) : true,
        };
    })()"#;
    let out = rub_cmd(&home).args(["exec", probe]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"]["query_native"], true, "{json}");
    assert_eq!(json["data"]["result"]["query_leaks_patch"], false, "{json}");
    assert_eq!(json["data"]["result"]["query_name"], "query", "{json}");
    assert_eq!(json["data"]["result"]["query_length"], 1, "{json}");
    assert_eq!(
        json["data"]["result"]["user_agent_own_descriptor_absent"], true,
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["user_agent_proto_getter_native"], true,
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["touch_own_descriptor_absent"], true,
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["touch_getter_native"], true,
        "{json}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t385_default_stealth_bridges_worker_identity() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><div>Worker Bridge Fixture</div></body></html>"#,
    )]);

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let probe = r#"(async () => {
        const workerSource = `
            self.onmessage = () => {
                self.postMessage({
                    userAgent: navigator.userAgent,
                    webdriverIn: ('webdriver' in navigator),
                    webdriverType: typeof navigator.webdriver,
                    chromeRuntime: typeof (self.chrome && self.chrome.runtime)
                });
            };
        `;
        const workerBlob = new Blob([workerSource], { type: 'text/javascript' });
        const workerUrl = URL.createObjectURL(workerBlob);
        try {
            return await new Promise((resolve, reject) => {
                const worker = new Worker(workerUrl);
                const timer = setTimeout(() => {
                    worker.terminate();
                    reject(new Error('worker timeout'));
                }, 5000);
                worker.onmessage = (event) => {
                    clearTimeout(timer);
                    worker.terminate();
                    resolve(event.data);
                };
                worker.onerror = (event) => {
                    clearTimeout(timer);
                    worker.terminate();
                    reject(new Error(event.message || 'worker error'));
                };
                worker.postMessage('go');
            });
        } finally {
            URL.revokeObjectURL(workerUrl);
        }
    })()"#;
    let out = rub_cmd(&home).args(["exec", probe]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        !json["data"]["result"]["userAgent"]
            .as_str()
            .unwrap_or_default()
            .contains("HeadlessChrome")
    );
    assert_eq!(json["data"]["result"]["webdriverIn"], false);
    assert_eq!(json["data"]["result"]["webdriverType"], "undefined");
    assert_eq!(json["data"]["result"]["chromeRuntime"], "object");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t381_doctor_reports_l0_when_stealth_disabled() {
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args(["--no-stealth", "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    let report = doctor_result(&json);
    assert_eq!(json["success"], true);
    assert_eq!(report["launch_policy"]["stealth_level"], "L0");
    assert_eq!(report["launch_policy"]["stealth_default_enabled"], false);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t382_humanize_click_reports_delay_in_timing() {
    let home_fast = unique_home();
    cleanup(&home_fast);
    let home_human = unique_home();
    cleanup(&home_human);

    let (_rt, server) = start_test_server(vec![(
        "/humanize-click",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <button id="submit" onclick="document.body.dataset.clicked='yes'">Submit</button>
</body>
</html>"#,
    )]);

    let url = server.url_for("/humanize-click");

    let out = rub_cmd(&home_fast).args(["open", &url]).output().unwrap();
    assert_eq!(parse_json(&out)["success"], true);
    let state = run_state(&home_fast);
    let snap = snapshot_id(&state);
    let idx = find_element_index(&state, |element| element["text"] == "Submit");
    let out = rub_cmd(&home_fast)
        .args(["click", &idx.to_string(), "--snapshot", &snap])
        .output()
        .unwrap();
    let fast_json = parse_json(&out);
    assert_eq!(fast_json["success"], true);
    let fast_exec_ms = fast_json["timing"]["exec_ms"].as_u64().unwrap_or(0);

    let out = rub_cmd(&home_human)
        .args(["--humanize", "--humanize-speed", "slow", "open", &url])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);
    let state = parse_json(
        &rub_cmd(&home_human)
            .args(["--humanize", "--humanize-speed", "slow", "state"])
            .output()
            .unwrap(),
    );
    assert_eq!(state["success"], true);
    let snap = snapshot_id(&state);
    let idx = find_element_index(&state, |element| element["text"] == "Submit");
    let out = rub_cmd(&home_human)
        .args([
            "--humanize",
            "--humanize-speed",
            "slow",
            "click",
            &idx.to_string(),
            "--snapshot",
            &snap,
        ])
        .output()
        .unwrap();
    let human_json = parse_json(&out);
    assert_eq!(human_json["success"], true);
    let human_exec_ms = human_json["timing"]["exec_ms"].as_u64().unwrap_or(0);
    assert!(
        human_exec_ms >= 350,
        "humanized click should visibly add delay, got {human_exec_ms}ms"
    );
    assert!(
        human_exec_ms > fast_exec_ms + 150,
        "humanized click should exceed baseline timing (fast={fast_exec_ms}ms, human={human_exec_ms}ms)"
    );

    cleanup(&home_fast);
    cleanup(&home_human);
}
