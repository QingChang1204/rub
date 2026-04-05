use super::*;

/// T370: state --listeners promotes a non-native interactive node with addEventListener.
#[test]
#[ignore]
#[serial]
fn t370_js_listeners() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/listeners",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Listeners Fixture</title></head>
<body>
  <div id="listener-target">Listener target</div>
  <script>
    document.getElementById('listener-target').addEventListener('click', () => {});
  </script>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/listeners")])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["state", "--listeners"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    let element = json["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| element["text"] == "Listener target")
        .expect("listener-backed element should be promoted into the snapshot");
    assert_eq!(element["tag"], "other");
    assert_eq!(element["listeners"][0], "click");
    assert!(
        element["element_ref"].as_str().is_some(),
        "listener-backed element should carry an element_ref for follow-up actions"
    );

    cleanup(&home);
}

/// T371: listener detection stays off by default.
#[test]
#[ignore]
#[serial]
fn t371_listeners_not_default() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/listeners-default",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Listeners Default Fixture</title></head>
<body>
  <div id="listener-target">Hidden Listener Target</div>
  <script>
    document.getElementById('listener-target').addEventListener('click', () => {});
  </script>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/listeners-default")])
        .output()
        .unwrap();

    let out = rub_cmd(&home).arg("state").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    let elements = json["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert!(
        !elements
            .iter()
            .any(|element| { element["text"].as_str() == Some("Hidden Listener Target") })
    );
    assert!(
        elements
            .iter()
            .all(|element| element["listeners"].is_null())
    );

    cleanup(&home);
}

/// T386: doctor runtime observatory should capture real console and request signals.
#[test]
#[ignore]
#[serial]
fn t386_doctor_runtime_observatory_captures_console_and_request_signals() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/observatory",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1>Observatory Fixture</h1></body></html>"#,
        ),
        ("/observatory-data", "application/json", r#"{"ok":true}"#),
    ]);

    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/observatory")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args([
            "exec",
            "console.error('rub observatory'); fetch('/observatory-data').then((r) => r.text())",
        ])
        .output()
        .unwrap();
    let exec_json = parse_json(&out);
    assert_eq!(exec_json["success"], true, "{exec_json}");

    let out = rub_cmd(&home).arg("doctor").output().unwrap();
    let json = parse_json(&out);
    let runtime = doctor_runtime(&json);
    assert_eq!(json["success"], true);
    assert_eq!(runtime["runtime_observatory"]["status"], "active");
    assert_eq!(runtime["integration_runtime"]["observatory_ready"], true);
    assert!(
        runtime["runtime_observatory"]["recent_console_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["message"].as_str() == Some("rub observatory"))
    );
    assert!(
        runtime["runtime_observatory"]["recent_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["url"].as_str().is_some_and(|url| {
                    url.ends_with("/observatory") || url.ends_with("/observatory-data")
                })
            })
    );

    cleanup(&home);
}

/// T387: doctor should project live state inspector and readiness signals from the current page.
#[test]
#[ignore]
#[serial]
fn t387_doctor_state_inspector_and_readiness_capture_live_page_state() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/runtime-state",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<body>
  <div id="app">Runtime State Fixture</div>
  <script>
    localStorage.setItem('token', 'abc');
    sessionStorage.setItem('csrf', 'def');
  </script>
</body>
</html>"#,
    )]);

    let out = rub_cmd(&home)
        .args(["open", &server.url_for("/runtime-state")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home).arg("doctor").output().unwrap();
    let json = parse_json(&out);
    let runtime = doctor_runtime(&json);
    assert_eq!(json["success"], true);
    assert_eq!(runtime["state_inspector"]["status"], "active");
    assert_eq!(runtime["state_inspector"]["auth_state"], "unknown");
    assert_eq!(runtime["state_inspector"]["cookie_count"], 0);
    assert_eq!(
        runtime["state_inspector"]["local_storage_keys"],
        json!(["token"])
    );
    assert_eq!(
        runtime["state_inspector"]["session_storage_keys"],
        json!(["csrf"])
    );
    assert_eq!(
        runtime["state_inspector"]["auth_signals"],
        json!([
            "local_storage_present",
            "session_storage_present",
            "auth_like_storage_key_present"
        ])
    );
    assert_eq!(
        runtime["integration_runtime"]["state_inspector_ready"],
        true
    );

    assert_eq!(runtime["readiness_state"]["status"], "active");
    assert_eq!(runtime["readiness_state"]["route_stability"], "stable");
    assert_eq!(runtime["readiness_state"]["loading_present"], false);
    assert_eq!(runtime["readiness_state"]["skeleton_present"], false);
    assert_eq!(runtime["readiness_state"]["overlay_state"], "none");
    assert_eq!(
        runtime["readiness_state"]["document_ready_state"],
        "complete"
    );
    assert_eq!(runtime["readiness_state"]["blocking_signals"], json!([]));
    assert_eq!(runtime["integration_runtime"]["readiness_ready"], true);
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
    assert_eq!(runtime["integration_runtime"]["handoff_ready"], false);

    cleanup(&home);
}

/// T388: doctor should project human verification handoff as available for external sessions.
#[test]
#[ignore]
#[serial]
fn t388_doctor_reports_handoff_available_for_external_session() {
    let (_rt, server) = start_standard_site_fixture();
    let Some((mut chrome, cdp_origin, profile_dir)) = spawn_external_chrome(Some(&server.url()))
    else {
        eprintln!("Skipping external handoff test because no Chrome/Chromium binary was found");
        return;
    };
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args(["--cdp-url", &cdp_origin, "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    let runtime = doctor_runtime(&json);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(runtime["human_verification_handoff"]["status"], "available");
    assert_eq!(
        runtime["human_verification_handoff"]["automation_paused"],
        false
    );
    assert_eq!(
        runtime["human_verification_handoff"]["resume_supported"],
        true
    );
    assert_eq!(runtime["integration_runtime"]["handoff_ready"], true);

    let _ = rub_cmd(&home).arg("close").output();
    terminate_external_chrome(&mut chrome);
    let _ = std::fs::remove_dir_all(profile_dir);
    cleanup(&home);
}

/// T389: starting handoff should pause automation until the user completes it.
#[test]
#[ignore]
#[serial]
fn t389_handoff_start_blocks_mutating_commands_until_complete() {
    let (_rt, server) = start_standard_site_fixture();
    let Some((mut chrome, cdp_origin, profile_dir)) = spawn_external_chrome(Some(&server.url()))
    else {
        eprintln!("Skipping external handoff test because no Chrome/Chromium binary was found");
        return;
    };
    let home = unique_home();
    cleanup(&home);

    let state = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "state"])
            .output()
            .unwrap(),
    );
    assert_eq!(state["success"], true, "{state}");

    let started = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "handoff", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(started["success"], true, "{started}");
    assert_eq!(started["data"]["runtime"]["status"], "active");
    assert_eq!(started["data"]["runtime"]["automation_paused"], true);

    let blocked = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(blocked["success"], false, "{blocked}");
    assert_eq!(blocked["error"]["code"], "AUTOMATION_PAUSED");
    assert_eq!(blocked["error"]["context"]["command"], "exec");
    assert_eq!(blocked["error"]["context"]["handoff"]["status"], "active");
    assert_eq!(
        blocked["error"]["context"]["handoff"]["automation_paused"],
        true
    );

    let completed = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "handoff", "complete"])
            .output()
            .unwrap(),
    );
    assert_eq!(completed["success"], true, "{completed}");
    assert_eq!(completed["data"]["runtime"]["status"], "completed");
    assert_eq!(completed["data"]["runtime"]["automation_paused"], false);

    let resumed = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(resumed["data"]["result"], 4);

    let _ = rub_cmd(&home).arg("close").output();
    terminate_external_chrome(&mut chrome);
    let _ = std::fs::remove_dir_all(profile_dir);
    cleanup(&home);
}

/// T389b: takeover status should truthfully classify external attached sessions as user-accessible.
#[test]
#[ignore]
#[serial]
fn t389b_takeover_status_reports_external_session_accessibility() {
    let (_rt, server) = start_standard_site_fixture();
    let Some((mut chrome, cdp_origin, profile_dir)) = spawn_external_chrome(Some(&server.url()))
    else {
        eprintln!("Skipping external takeover test because no Chrome/Chromium binary was found");
        return;
    };
    let home = unique_home();
    cleanup(&home);

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "takeover", "status"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "available");
    assert_eq!(
        runtime["data"]["runtime"]["session_accessibility"],
        "user_accessible"
    );
    assert_eq!(runtime["data"]["runtime"]["visibility_mode"], "external");
    assert_eq!(runtime["data"]["runtime"]["resume_supported"], true);
    assert_eq!(runtime["data"]["runtime"]["automation_paused"], false);

    let _ = rub_cmd(&home).arg("close").output();
    terminate_external_chrome(&mut chrome);
    let _ = std::fs::remove_dir_all(profile_dir);
    cleanup(&home);
}

/// T389b: takeover start/resume should wrap the existing handoff state machine.
#[test]
#[ignore]
#[serial]
fn t389c_takeover_start_and_resume_follow_external_session_state() {
    let (_rt, server) = start_standard_site_fixture();
    let Some((mut chrome, cdp_origin, profile_dir)) = spawn_external_chrome(Some(&server.url()))
    else {
        eprintln!("Skipping external takeover test because no Chrome/Chromium binary was found");
        return;
    };
    let home = unique_home();
    cleanup(&home);

    let started = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "takeover", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(started["success"], true, "{started}");
    assert_eq!(started["data"]["runtime"]["status"], "active");
    assert_eq!(started["data"]["runtime"]["automation_paused"], true);
    assert_eq!(
        started["data"]["runtime"]["last_transition"]["kind"],
        "start"
    );
    assert_eq!(
        started["data"]["runtime"]["last_transition"]["result"],
        "succeeded"
    );

    let blocked = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(blocked["success"], false, "{blocked}");
    assert_eq!(blocked["error"]["code"], "AUTOMATION_PAUSED");

    let resumed = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "takeover", "resume"])
            .output()
            .unwrap(),
    );
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(resumed["data"]["runtime"]["status"], "available");
    assert_eq!(resumed["data"]["runtime"]["automation_paused"], false);
    assert_eq!(
        resumed["data"]["runtime"]["last_transition"]["kind"],
        "resume"
    );
    assert_eq!(
        resumed["data"]["runtime"]["last_transition"]["result"],
        "succeeded"
    );

    let replay = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["data"]["result"], 4);

    let _ = rub_cmd(&home).arg("close").output();
    terminate_external_chrome(&mut chrome);
    let _ = std::fs::remove_dir_all(profile_dir);
    cleanup(&home);
}

/// T389d: managed headed sessions should support takeover start/resume without external attach.
#[test]
#[ignore]
#[serial]
fn t389d_takeover_start_and_resume_follow_managed_headed_session_state() {
    let (_rt, server) = start_standard_site_fixture();
    let home = unique_home();
    cleanup(&home);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "open", server.url().as_str()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let status = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "takeover", "status"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["runtime"]["status"], "available");
    assert_eq!(
        status["data"]["runtime"]["session_accessibility"],
        "user_accessible"
    );
    assert_eq!(status["data"]["runtime"]["visibility_mode"], "headed");
    assert_eq!(status["data"]["runtime"]["elevate_supported"], false);
    assert_eq!(status["data"]["runtime"]["resume_supported"], true);

    let started = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "takeover", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(started["success"], true, "{started}");
    assert_eq!(started["data"]["runtime"]["status"], "active");
    assert_eq!(started["data"]["runtime"]["automation_paused"], true);
    assert_eq!(
        started["data"]["runtime"]["last_transition"]["kind"],
        "start"
    );
    assert_eq!(
        started["data"]["runtime"]["last_transition"]["result"],
        "succeeded"
    );

    let blocked = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(blocked["success"], false, "{blocked}");
    assert_eq!(blocked["error"]["code"], "AUTOMATION_PAUSED");

    let resumed = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "takeover", "resume"])
            .output()
            .unwrap(),
    );
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(resumed["data"]["runtime"]["status"], "available");
    assert_eq!(resumed["data"]["runtime"]["automation_paused"], false);
    assert_eq!(
        resumed["data"]["runtime"]["last_transition"]["kind"],
        "resume"
    );
    assert_eq!(
        resumed["data"]["runtime"]["last_transition"]["result"],
        "succeeded"
    );

    let replay = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["data"]["result"], 4);

    let _ = rub_cmd(&home).arg("close").output();
    cleanup(&home);
}

/// T389e: managed headless sessions should elevate to visible takeover before start/resume.
#[test]
#[ignore]
#[serial]
fn t389e_takeover_elevate_promotes_managed_headless_session_to_visible_control() {
    let (_rt, server) = start_standard_site_fixture();
    let home = unique_home();
    cleanup(&home);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", server.url().as_str()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let status = parse_json(
        &rub_cmd(&home)
            .args(["takeover", "status"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["runtime"]["status"], "unavailable");
    assert_eq!(
        status["data"]["runtime"]["session_accessibility"],
        "automation_only"
    );
    assert_eq!(status["data"]["runtime"]["visibility_mode"], "headless");
    assert_eq!(status["data"]["runtime"]["elevate_supported"], true);
    assert_eq!(
        status["data"]["runtime"]["unavailable_reason"],
        "elevation_required"
    );

    let rejected = parse_json(&rub_cmd(&home).args(["takeover", "start"]).output().unwrap());
    assert_eq!(rejected["success"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "INVALID_INPUT");
    assert_eq!(
        rejected["error"]["context"]["takeover_runtime"]["unavailable_reason"],
        "elevation_required"
    );

    let elevated = parse_json(
        &rub_cmd(&home)
            .args(["takeover", "elevate"])
            .output()
            .unwrap(),
    );
    assert_eq!(elevated["success"], true, "{elevated}");
    assert_eq!(elevated["data"]["runtime"]["status"], "available");
    assert_eq!(
        elevated["data"]["runtime"]["session_accessibility"],
        "user_accessible"
    );
    assert_eq!(elevated["data"]["runtime"]["visibility_mode"], "headed");
    assert_eq!(elevated["data"]["runtime"]["automation_paused"], false);
    assert_eq!(
        elevated["data"]["runtime"]["last_transition"]["kind"],
        "elevate"
    );
    assert_eq!(
        elevated["data"]["runtime"]["last_transition"]["result"],
        "succeeded"
    );

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "takeover", "status"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "available");
    assert_eq!(runtime["data"]["runtime"]["visibility_mode"], "headed");

    let state = parse_json(&rub_cmd(&home).args(["--headed", "state"]).output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(
        state["data"]["result"]["snapshot"]["title"],
        "Example Domain"
    );

    let started = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "takeover", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(started["success"], true, "{started}");
    assert_eq!(started["data"]["runtime"]["status"], "active");
    assert_eq!(
        started["data"]["runtime"]["last_transition"]["kind"],
        "start"
    );

    let resumed = parse_json(
        &rub_cmd(&home)
            .args(["--headed", "takeover", "resume"])
            .output()
            .unwrap(),
    );
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(resumed["data"]["runtime"]["status"], "available");
    assert_eq!(
        resumed["data"]["runtime"]["last_transition"]["kind"],
        "resume"
    );
    assert_eq!(
        resumed["data"]["runtime"]["last_transition"]["result"],
        "succeeded"
    );

    let _ = rub_cmd(&home).arg("close").output();
    cleanup(&home);
}

/// T390: session-scoped intercept rewrite should re-route matching requests and recover on clear.
#[test]
#[ignore]
#[serial]
fn t390_intercept_rewrite_round_trip() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/app",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <div id="status">loading</div>
  <script>
    fetch('/api/data')
      .then((r) => r.text())
      .then((text) => { document.getElementById('status').textContent = text; })
      .catch((error) => { document.getElementById('status').textContent = 'error:' + error; });
  </script>
</body>
</html>"#,
        ),
        ("/api/data", "text/plain", "prod"),
        ("/mock/data", "text/plain", "mock"),
    ]);

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "intercept",
                "rewrite",
                &server.url_for("/api/*"),
                &server.url_for("/mock"),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    assert_eq!(added["data"]["subject"]["kind"], "intercept_rule");
    assert_eq!(added["data"]["subject"]["action"], "rewrite");
    assert_eq!(added["data"]["result"]["rule"]["action"], "rewrite");
    assert_eq!(
        added["data"]["result"]["rule"]["pattern"],
        server.url_for("/api/*")
    );
    assert_eq!(added["data"]["result"]["rule"]["status"], "active");
    assert_eq!(
        added["data"]["result"]["rules"].as_array().unwrap().len(),
        1
    );
    assert_eq!(added["data"]["runtime"]["request_rule_count"], 1, "{added}");

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    let runtime = doctor_runtime(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(runtime["integration_runtime"]["status"], "active");
    assert_eq!(runtime["integration_runtime"]["request_rule_count"], 1);

    let listed = parse_json(&rub_cmd(&home).args(["intercept", "list"]).output().unwrap());
    assert_eq!(listed["success"], true, "{listed}");
    assert_eq!(listed["data"]["subject"]["kind"], "intercept_rule_registry");
    assert_eq!(
        listed["data"]["result"]["rules"].as_array().unwrap().len(),
        1
    );
    assert_eq!(listed["data"]["result"]["rules"][0]["action"], "rewrite");
    assert_eq!(
        listed["data"]["result"]["rules"][0]["pattern"],
        server.url_for("/api/*")
    );

    let open = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/app")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["wait", "--text", "mock", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let mocked = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.querySelector('#status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(mocked["success"], true, "{mocked}");
    assert_eq!(mocked["data"]["result"], "mock");

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    let runtime = doctor_runtime(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert!(
        runtime["runtime_observatory"]["recent_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["url"]
                    .as_str()
                    .is_some_and(|url| url.ends_with("/api/data"))
                    && event["rewritten_url"]
                        .as_str()
                        .is_some_and(|url| url.ends_with("/mock/data"))
                    && event["applied_rule_effects"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|effect| effect["kind"] == "rewrite")
            }),
        "{doctor}"
    );

    let cleared = parse_json(
        &rub_cmd(&home)
            .args(["intercept", "clear"])
            .output()
            .unwrap(),
    );
    assert_eq!(cleared["success"], true, "{cleared}");
    assert_eq!(cleared["data"]["result"]["cleared"], true);
    assert_eq!(cleared["data"]["result"]["rules"], json!([]));

    let listed = parse_json(&rub_cmd(&home).args(["intercept", "list"]).output().unwrap());
    assert_eq!(listed["success"], true, "{listed}");
    assert_eq!(listed["data"]["result"]["rules"], json!([]));

    let open = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/app")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["wait", "--text", "prod", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let prod = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.querySelector('#status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(prod["success"], true, "{prod}");
    assert_eq!(prod["data"]["result"], "prod");

    cleanup(&home);
}

/// T391: blocked requests should surface correlated rule effects in the runtime observatory.
#[test]
#[ignore]
#[serial]
fn t391_intercept_block_correlates_network_failure() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/blocked-app",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <div id="status">loading</div>
  <script>
    fetch('/api/blocked')
      .then((r) => r.text())
      .then((text) => { document.getElementById('status').textContent = text; })
      .catch((error) => { document.getElementById('status').textContent = 'error:' + error.name; });
  </script>
</body>
</html>"#,
        ),
        ("/api/blocked", "text/plain", "should-not-load"),
    ]);

    let added = parse_json(
        &rub_cmd(&home)
            .args(["intercept", "block", &server.url_for("/api/blocked")])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    assert_eq!(added["data"]["result"]["rule"]["action"], "block");

    let open = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/blocked-app")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["wait", "--text", "error:TypeError", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    let runtime = doctor_runtime(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert!(
        runtime["runtime_observatory"]["recent_network_failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["url"]
                    .as_str()
                    .is_some_and(|url| url.ends_with("/api/blocked"))
                    && event["applied_rule_effects"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|effect| effect["kind"] == "block")
            }),
        "{doctor}"
    );

    cleanup(&home);
}

/// T392: header overrides should apply on the real request and appear in observatory correlation.
#[test]
#[ignore]
#[serial]
fn t392_intercept_header_override_round_trip() {
    let home = unique_home();
    cleanup(&home);

    let (base_url, rx, handle) = start_header_fixture_server();
    let app_url = format!("{base_url}/app");
    let capture_url = format!("{base_url}/capture");

    let added = parse_json(
        &rub_cmd(&home)
            .args([
                "intercept",
                "header",
                &capture_url,
                "--header",
                "x-rub-env=dev",
                "--header",
                "x-rub-trace=1",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    assert_eq!(added["data"]["result"]["rule"]["action"], "header_override");
    assert_eq!(added["data"]["result"]["rule"]["pattern"], capture_url);

    let opened = parse_json(&rub_cmd(&home).args(["open", &app_url]).output().unwrap());
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["wait", "--text", "ok", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let raw_request = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(raw_request.contains("x-rub-env: dev") || raw_request.contains("X-Rub-Env: dev"));
    assert!(raw_request.contains("x-rub-trace: 1") || raw_request.contains("X-Rub-Trace: 1"));
    handle.join().unwrap();

    let inspected = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "network", "--match", &capture_url, "--last", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    let request = &inspected["data"]["requests"][0];
    assert_eq!(request["url"], capture_url, "{inspected}");
    assert_eq!(
        request["request_headers"]["x-rub-env"], "dev",
        "{inspected}"
    );
    assert_eq!(
        request["request_headers"]["x-rub-trace"], "1",
        "{inspected}"
    );

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    let runtime = doctor_runtime(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert!(
        runtime["runtime_observatory"]["recent_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["url"].as_str() == Some(capture_url.as_str())
                    && event["applied_rule_effects"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|effect| effect["kind"] == "header_override")
            }),
        "{doctor}"
    );

    cleanup(&home);
}

/// T392b: positional intercept header syntax should normalize to the canonical header override runtime.
#[test]
#[ignore]
#[serial]
fn t392b_intercept_header_positional_name_and_value_round_trip() {
    let home = unique_home();
    cleanup(&home);

    let (base_url, rx, handle) = start_header_fixture_server();
    let app_url = format!("{base_url}/app");
    let capture_url = format!("{base_url}/capture");

    let added = parse_json(
        &rub_cmd(&home)
            .args(["intercept", "header", &capture_url, "x-rub-live", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");

    let opened = parse_json(&rub_cmd(&home).args(["open", &app_url]).output().unwrap());
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["wait", "--text", "ok", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let raw_request = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(raw_request.contains("x-rub-live: 1") || raw_request.contains("X-Rub-Live: 1"));
    handle.join().unwrap();

    let inspected = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "network", "--match", &capture_url, "--last", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["requests"][0]["request_headers"]["x-rub-live"], "1",
        "{inspected}"
    );

    cleanup(&home);
}

/// T393: interactive traces should correlate live runtime-state deltas.
#[test]
#[ignore]
#[serial]
fn t393_interaction_trace_correlates_runtime_state_delta() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Runtime Delta Fixture</title></head>
<body>
  <button id="promote" onclick="
    localStorage.setItem('authToken', 'abc');
    document.body.classList.add('loading');
    document.getElementById('status').textContent = 'working';
  ">
    Promote
  </button>
  <div id="status">idle</div>
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

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Promote")
    });

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", &button_index.to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["interaction_confirmed"],
        true
    );
    assert_eq!(
        clicked["data"]["interaction"]["runtime_state_delta"]["changed"],
        json!([
            "state_inspector.auth_state",
            "state_inspector.local_storage_keys",
            "state_inspector.auth_signals",
            "readiness_state.route_stability",
            "readiness_state.loading_present",
            "readiness_state.blocking_signals"
        ])
    );

    let delta = &clicked["data"]["interaction_trace"]["observed_effects"]["runtime_state_delta"];
    assert_eq!(
        delta["changed"],
        json!([
            "state_inspector.auth_state",
            "state_inspector.local_storage_keys",
            "state_inspector.auth_signals",
            "readiness_state.route_stability",
            "readiness_state.loading_present",
            "readiness_state.blocking_signals"
        ])
    );
    assert_eq!(
        delta["after"]["state_inspector"]["auth_signals"],
        json!(["local_storage_present", "auth_like_storage_key_present"])
    );
    assert_eq!(
        delta["after"]["readiness_state"]["blocking_signals"],
        json!(["loading_present", "route_transitioning"])
    );

    cleanup(&home);
}

/// T394: interactive traces should correlate observatory events emitted during the command window.
#[test]
#[ignore]
#[serial]
fn t394_interaction_trace_correlates_runtime_observatory_events() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Observatory Delta Fixture</title></head>
<body>
  <button id="trigger" onclick="
    console.error('button-boom');
    fetch('/ping').then(() => {
      document.getElementById('status').textContent = 'done';
      document.body.dataset.done = '1';
    });
  ">
    Trigger
  </button>
  <div id="status">idle</div>
</body>
</html>"#,
        ),
        ("/ping", "application/json", r#"{"ok":true}"#),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Trigger")
    });

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", &button_index.to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["interaction_confirmed"],
        true
    );

    let events = clicked["data"]["interaction"]["runtime_observatory_events"]
        .as_array()
        .expect("runtime observatory events");
    assert!(
        events.iter().any(|event| {
            event["kind"] == "console_error"
                && event["event"]["message"].as_str() == Some("button-boom")
        }),
        "{clicked}"
    );
    assert!(
        events.iter().any(|event| {
            event["kind"] == "request_summary"
                && event["event"]["url"].as_str() == Some(server.url_for("/ping").as_str())
                && event["event"]["status"] == 200
        }),
        "{clicked}"
    );
    assert_eq!(
        clicked["data"]["interaction_trace"]["observed_effects"]["runtime_observatory_events"],
        clicked["data"]["interaction"]["runtime_observatory_events"]
    );

    cleanup(&home);
}

/// T395: runtime summary should expose canonical live integration surfaces without the extra doctor envelope.
#[test]
#[ignore]
#[serial]
fn t395_runtime_summary_reports_live_integration_surfaces() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Runtime Summary Fixture</title></head>
<body class="loading">
  <div id="status">ready</div>
  <script>
    localStorage.setItem('authToken', 'abc');
  </script>
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

    let runtime = parse_json(&rub_cmd(&home).args(["runtime"]).output().unwrap());
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(
        runtime["data"]["integration_runtime"]["status"], "active",
        "{runtime}"
    );
    assert_eq!(runtime["data"]["runtime"]["status"], "top", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["current_frame"]["depth"], 0,
        "{runtime}"
    );
    assert_eq!(
        runtime["data"]["dialog_runtime"]["status"], "inactive",
        "{runtime}"
    );
    assert!(
        runtime["data"]["dialog_runtime"]["pending_dialog"].is_null(),
        "{runtime}"
    );
    assert_eq!(runtime["data"]["interference_runtime"]["mode"], "normal");
    assert_eq!(
        runtime["data"]["interference_runtime"]["status"],
        "inactive"
    );
    assert_eq!(
        runtime["data"]["integration_runtime"]["active_surfaces"],
        json!(["runtime_observatory", "state_inspector", "readiness"]),
        "{runtime}"
    );
    assert_eq!(runtime["data"]["storage_runtime"]["status"], "active");
    assert_eq!(
        runtime["data"]["storage_runtime"]["local_storage_keys"],
        json!(["authToken"]),
        "{runtime}"
    );
    assert_eq!(
        runtime["data"]["integration_runtime"]["degraded_surfaces"],
        json!([]),
        "{runtime}"
    );
    assert_eq!(
        runtime["data"]["state_inspector"]["auth_signals"],
        json!(["local_storage_present", "auth_like_storage_key_present"])
    );
    assert_eq!(
        runtime["data"]["readiness_state"]["blocking_signals"],
        json!(["loading_present", "route_transitioning"])
    );
    assert_eq!(
        runtime["data"]["human_verification_handoff"]["status"],
        "unavailable"
    );

    let interference = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(interference["success"], true, "{interference}");
    assert_eq!(interference["data"]["runtime"]["mode"], "normal");
    assert_eq!(interference["data"]["runtime"]["status"], "inactive");
    assert_eq!(
        interference["data"]["runtime"]["active_policies"],
        json!([]),
        "{interference}"
    );

    let frame = parse_json(&rub_cmd(&home).args(["runtime", "frame"]).output().unwrap());
    assert_eq!(frame["success"], true, "{frame}");
    assert_eq!(frame["data"]["runtime"]["status"], "top", "{frame}");
    assert_eq!(
        frame["data"]["result"]["current_frame"]["depth"], 0,
        "{frame}"
    );
    assert_eq!(
        frame["data"]["result"]["current_frame"]["same_origin_accessible"], true,
        "{frame}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t415_frames_lists_same_origin_iframe_inventory() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Inventory Fixture</title></head>
<body>
  <h1>Parent Frame</h1>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <button id="inside-frame">Inside Frame</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let frames = parse_json(&rub_cmd(&home).arg("frames").output().unwrap());
    assert_eq!(frames["success"], true, "{frames}");

    let entries = frames["data"]["result"]["items"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{frames}");

    let top = &entries[0];
    assert_eq!(top["index"], 0, "{frames}");
    assert_eq!(top["is_current"], true, "{frames}");
    assert_eq!(top["is_primary"], true, "{frames}");
    assert_eq!(top["frame"]["depth"], 0, "{frames}");
    assert_eq!(top["frame"]["same_origin_accessible"], true, "{frames}");

    let child = entries
        .iter()
        .find(|entry| entry["frame"]["name"] == "child-frame")
        .expect("expected named child frame entry");
    assert_eq!(child["frame"]["depth"], 1, "{frames}");
    assert_eq!(child["is_current"], false, "{frames}");
    assert_eq!(child["is_primary"], false, "{frames}");
    assert_eq!(child["frame"]["same_origin_accessible"], true, "{frames}");
    assert_eq!(
        child["frame"]["parent_frame_id"], top["frame"]["frame_id"],
        "{frames}"
    );
    assert!(
        child["frame"]["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/frame-child")),
        "{frames}"
    );
}

#[test]
#[ignore]
#[serial]
fn t416_frame_switches_same_origin_child_and_state_binds_frame_context() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Switch Fixture</title></head>
<body>
  <button id="top-button">Top Button</button>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <button id="inside-frame">Inside Frame</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(&rub_cmd(&home).args(["frame", "1"]).output().unwrap());
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(switched["data"]["runtime"]["status"], "child", "{switched}");
    assert_eq!(
        switched["data"]["result"]["current_frame"]["name"], "child-frame",
        "{switched}"
    );

    let frames = parse_json(&rub_cmd(&home).arg("frames").output().unwrap());
    let child = frames["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["frame"]["name"] == "child-frame")
        .expect("expected named child frame entry");
    assert_eq!(child["is_current"], true, "{frames}");
    assert_eq!(child["is_primary"], false, "{frames}");

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(
        state["data"]["frame_context"]["frame_id"],
        switched["data"]["result"]["current_frame"]["frame_id"],
        "{state}"
    );
    assert_eq!(
        state["data"]["result"]["snapshot"]["frame_context"]["name"], "child-frame",
        "{state}"
    );
    let elements = state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 1, "{state}");
    assert_eq!(elements[0]["text"], "Inside Frame", "{state}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t416b_open_resets_selected_frame_context_to_top() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Reset Fixture</title></head>
<body>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <button id="inside-frame">Inside Frame</button>
</body>
</html>"#,
        ),
        (
            "/next",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <button id="next-top">Next Top</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(&rub_cmd(&home).args(["frame", "1"]).output().unwrap());
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(switched["data"]["runtime"]["status"], "child", "{switched}");

    let navigated = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/next")])
            .output()
            .unwrap(),
    );
    assert_eq!(navigated["success"], true, "{navigated}");

    let runtime = parse_json(&rub_cmd(&home).args(["runtime", "frame"]).output().unwrap());
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "top", "{runtime}");
    assert_eq!(
        runtime["data"]["result"]["current_frame"]["frame_id"],
        runtime["data"]["runtime"]["primary_frame"]["frame_id"],
        "{runtime}"
    );

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    let elements = state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 1, "{state}");
    assert_eq!(elements[0]["text"], "Next Top", "{state}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t417_frame_switch_rejects_cross_frame_snapshot_reuse() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Snapshot Fixture</title></head>
<body>
  <button id="top-button">Top Button</button>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <button id="inside-frame">Inside Frame</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let top_state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(top_state["success"], true, "{top_state}");
    let snapshot_id = top_state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();

    let switched = parse_json(&rub_cmd(&home).args(["frame", "1"]).output().unwrap());
    assert_eq!(switched["success"], true, "{switched}");

    let get_text = parse_json(
        &rub_cmd(&home)
            .args(["get", "text", "0", "--snapshot", snapshot_id])
            .output()
            .unwrap(),
    );
    assert_eq!(get_text["success"], false, "{get_text}");
    assert_eq!(get_text["error"]["code"], "STALE_SNAPSHOT", "{get_text}");
    assert_eq!(
        get_text["error"]["context"]["snapshot_frame_id"],
        top_state["data"]["frame_context"]["frame_id"],
        "{get_text}"
    );
    assert_eq!(
        get_text["error"]["context"]["current_frame_id"],
        switched["data"]["result"]["current_frame"]["frame_id"],
        "{get_text}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t418_input_in_selected_same_origin_frame_confirms_value_applied() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Input Fixture</title></head>
<body>
  <input id="top-input" />
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <input id="inside-frame-input" value="" placeholder="Inside Frame Input" />
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(switched["data"]["runtime"]["status"], "child", "{switched}");

    let input = parse_json(
        &rub_cmd(&home)
            .args(["type", "--index", "0", "hello from frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(input["success"], true, "{input}");
    assert_eq!(
        input["data"]["interaction"]["confirmation_status"], "confirmed",
        "{input}"
    );
    assert_eq!(
        input["data"]["interaction"]["confirmation_kind"], "value_applied",
        "{input}"
    );

    let value = parse_json(&rub_cmd(&home).args(["get", "value", "0"]).output().unwrap());
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(
        value["data"]["result"]["value"], "hello from frame",
        "{value}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t419_extract_in_selected_same_origin_frame_reads_child_content() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Extract Fixture</title></head>
<body>
  <h1>Top Heading</h1>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <h1>Child Heading</h1>
  <p class="content">Child paragraph</p>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let extract = parse_json(
        &rub_cmd(&home)
            .args([
                "extract",
                r#"{"heading":{"selector":"h1","kind":"text","required":true},"paragraph":{"selector":"p.content","kind":"text","required":true}}"#,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(extract["success"], true, "{extract}");
    assert_eq!(
        extract["data"]["result"]["fields"]["heading"], "Child Heading",
        "{extract}"
    );
    assert_eq!(
        extract["data"]["result"]["fields"]["paragraph"], "Child paragraph",
        "{extract}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t420_fill_in_selected_same_origin_frame_uses_child_frame_context() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Fill Fixture</title></head>
<body>
  <input id="shared-input" value="top" />
  <button id="top-submit" type="button">Save Top</button>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <input id="shared-input" value="" />
  <button id="child-submit" type="button" onclick="this.textContent='Saved Child'">Save Child</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let fill = parse_json(
        &rub_cmd(&home)
            .args([
                "fill",
                r##"[{"selector":"#shared-input","value":"child hello"}]"##,
                "--submit-target-text",
                "Save Child",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(fill["success"], true, "{fill}");
    assert_eq!(
        fill["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{fill}"
    );

    let value = parse_json(
        &rub_cmd(&home)
            .args(["get", "value", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(value["data"]["result"]["value"], "child hello", "{value}");

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    let elements = state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 2, "{state}");
    assert_eq!(elements[1]["text"], "Saved Child", "{state}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t421_type_in_selected_same_origin_frame_uses_child_active_context() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Type Fixture</title></head>
<body>
  <input id="shared-input" value="top" />
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <input id="shared-input" value="" />
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let typed = parse_json(
        &rub_cmd(&home)
            .args(["type", "child hello"])
            .output()
            .unwrap(),
    );
    assert_eq!(typed["success"], true, "{typed}");
    assert_eq!(
        typed["data"]["interaction"]["confirmation_kind"], "value_applied",
        "{typed}"
    );
    assert_eq!(
        typed["data"]["interaction"]["frame_context"]["name"], "child-frame",
        "{typed}"
    );
    assert_eq!(
        typed["data"]["interaction_trace"]["observed_effects"]["frame_context"]["name"],
        "child-frame",
        "{typed}"
    );

    let value = parse_json(
        &rub_cmd(&home)
            .args(["get", "value", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(value["data"]["result"]["value"], "child hello", "{value}");

    let top_value = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('shared-input').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_value["success"], true, "{top_value}");
    assert_eq!(top_value["data"]["result"], "top", "{top_value}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t422_select_in_selected_same_origin_frame_uses_child_context() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Select Fixture</title></head>
<body>
  <select id="shared-select">
    <option value="top_a" selected>Top Alpha</option>
    <option value="top_b">Top Beta</option>
  </select>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <select id="shared-select">
    <option value="child_a" selected>Child Alpha</option>
    <option value="child_b">Child Beta</option>
  </select>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let selected = parse_json(
        &rub_cmd(&home)
            .args(["select", "--selector", "#shared-select", "Child Beta"])
            .output()
            .unwrap(),
    );
    assert_eq!(selected["success"], true, "{selected}");
    assert_eq!(selected["data"]["result"]["value"], "child_b", "{selected}");
    assert_eq!(
        selected["data"]["interaction"]["frame_context"]["name"], "child-frame",
        "{selected}"
    );

    let value = parse_json(
        &rub_cmd(&home)
            .args(["get", "value", "--selector", "#shared-select"])
            .output()
            .unwrap(),
    );
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(value["data"]["result"]["value"], "child_b", "{value}");

    let top_value = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('shared-select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_value["success"], true, "{top_value}");
    assert_eq!(top_value["data"]["result"], "top_a", "{top_value}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t423_upload_in_selected_same_origin_frame_uses_child_context() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Upload Fixture</title></head>
<body>
  <input id="shared-upload" type="file" />
  <div id="top-filename"></div>
  <script>
    document.getElementById('shared-upload').addEventListener('change', (event) => {
      const file = event.target.files[0];
      document.getElementById('top-filename').textContent = file ? file.name : '';
    });
  </script>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <input id="shared-upload" type="file" />
  <div id="filename"></div>
  <script>
    document.getElementById('shared-upload').addEventListener('change', (event) => {
      const file = event.target.files[0];
      document.getElementById('filename').textContent = file ? file.name : '';
    });
  </script>
</body>
</html>"#,
        ),
    ]);

    let file_path = format!("/tmp/rub-frame-upload-{}.txt", std::process::id());
    std::fs::write(&file_path, b"frame upload").unwrap();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let uploaded = parse_json(
        &rub_cmd(&home)
            .args(["upload", "--selector", "#shared-upload", &file_path])
            .output()
            .unwrap(),
    );
    assert_eq!(uploaded["success"], true, "{uploaded}");
    assert_eq!(
        uploaded["data"]["interaction"]["confirmation_kind"], "files_attached",
        "{uploaded}"
    );
    assert_eq!(
        uploaded["data"]["interaction"]["frame_context"]["name"], "child-frame",
        "{uploaded}"
    );

    let extracted = parse_json(
        &rub_cmd(&home)
            .args([
                "extract",
                r##"{"filename":{"selector":"#filename","kind":"text","required":true}}"##,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(extracted["success"], true, "{extracted}");
    assert_eq!(
        extracted["data"]["result"]["fields"]["filename"],
        format!("rub-frame-upload-{}.txt", std::process::id()),
        "{extracted}"
    );

    let top_filename = parse_json(
        &rub_cmd(&home)
            .args([
                "exec",
                "document.getElementById('top-filename').textContent",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(top_filename["success"], true, "{top_filename}");
    assert_eq!(top_filename["data"]["result"], "", "{top_filename}");

    let _ = std::fs::remove_file(&file_path);
    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t424_runtime_frame_marks_removed_selected_frame_stale() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Stale Fixture</title></head>
<body>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
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
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html><html><body><button>Inside Frame</button></body></html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args([
                "exec",
                "new Promise((resolve) => setTimeout(() => resolve('done'), 750))",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let runtime = parse_json(&rub_cmd(&home).args(["runtime", "frame"]).output().unwrap());
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "stale", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["degraded_reason"], "selected_frame_not_found",
        "{runtime}"
    );

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], false, "{state}");
    assert_eq!(state["error"]["code"], "STALE_SNAPSHOT", "{state}");
    assert_eq!(
        state["error"]["context"]["frame_runtime"]["status"], "stale",
        "{state}"
    );

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t425_frames_support_nested_same_origin_inventory_and_switch() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Nested Frame Fixture</title></head>
<body>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <iframe
    id="grandchild-frame"
    name="grandchild-frame"
    src="/frame-grandchild"
    title="Grandchild Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-grandchild",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<body>
  <button id="inside-grandchild">Inside Grandchild</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let frames = parse_json(&rub_cmd(&home).arg("frames").output().unwrap());
    assert_eq!(frames["success"], true, "{frames}");
    let entries = frames["data"]["result"]["items"].as_array().unwrap();
    let grandchild = entries
        .iter()
        .find(|entry| entry["frame"]["name"] == "grandchild-frame")
        .expect("expected grandchild frame");
    assert_eq!(grandchild["frame"]["depth"], 2, "{frames}");
    assert_eq!(
        grandchild["frame"]["same_origin_accessible"], true,
        "{frames}"
    );

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "grandchild-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(
        switched["data"]["result"]["current_frame"]["name"], "grandchild-frame",
        "{switched}"
    );

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(
        state["data"]["result"]["snapshot"]["frame_context"]["name"], "grandchild-frame",
        "{state}"
    );
    assert_eq!(
        state["data"]["result"]["snapshot"]["frame_lineage"][0],
        state["data"]["result"]["snapshot"]["frame_context"]["frame_id"],
        "{state}"
    );
    let elements = state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 1, "{state}");
    assert_eq!(elements[0]["text"], "Inside Grandchild", "{state}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t426_input_ref_in_selected_same_origin_frame_confirms_value_applied() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Ref Input Fixture</title></head>
<body>
  <input id="top-input" value="top" />
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <input id="inside-frame-input" value="" placeholder="Inside Frame Input" />
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    let element_ref = find_element_ref(&state, |element| {
        element["attributes"]["placeholder"] == "Inside Frame Input"
    });

    let input = parse_json(
        &rub_cmd(&home)
            .args(["type", "--ref", &element_ref, "hello via ref"])
            .output()
            .unwrap(),
    );
    assert_eq!(input["success"], true, "{input}");
    assert_eq!(
        input["data"]["interaction"]["confirmation_kind"], "value_applied",
        "{input}"
    );
    assert_eq!(
        input["data"]["interaction"]["frame_context"]["name"], "child-frame",
        "{input}"
    );

    let value = parse_json(
        &rub_cmd(&home)
            .args(["get", "value", "--ref", &element_ref])
            .output()
            .unwrap(),
    );
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(value["data"]["result"]["value"], "hello via ref", "{value}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t427_ref_locator_rejects_cross_frame_live_resolution() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Frame Ref Boundary Fixture</title></head>
<body>
  <button id="top-button">Top Button</button>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-child",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Child Frame</title></head>
<body>
  <button id="inside-frame">Inside Frame</button>
</body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let state = parse_json(&rub_cmd(&home).arg("state").output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    let element_ref = find_element_ref(&state, |element| element["text"] == "Inside Frame");

    let reset = parse_json(&rub_cmd(&home).args(["frame", "--top"]).output().unwrap());
    assert_eq!(reset["success"], true, "{reset}");

    let get_text = parse_json(
        &rub_cmd(&home)
            .args(["get", "text", "--ref", &element_ref])
            .output()
            .unwrap(),
    );
    assert_eq!(get_text["success"], false, "{get_text}");
    assert_eq!(get_text["error"]["code"], "ELEMENT_NOT_FOUND", "{get_text}");

    cleanup(&home);
}

/// T396: runtime observatory subcommand should expose recent console/request activity directly.
#[test]
#[ignore]
#[serial]
fn t396_runtime_observatory_subcommand_returns_recent_events() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Runtime Observatory Fixture</title></head>
<body>
  <div id="status">idle</div>
  <script>
    console.error('runtime-observatory-boom');
    fetch('/ping').then(() => {
      document.getElementById('status').textContent = 'done';
    });
  </script>
</body>
</html>"#,
        ),
        ("/ping", "application/json", r#"{"ok":true}"#),
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
            .args(["wait", "--text", "done", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let observatory = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "observatory"])
            .output()
            .unwrap(),
    );
    assert_eq!(observatory["success"], true, "{observatory}");
    assert_eq!(observatory["data"]["runtime"]["status"], "active");
    assert!(
        observatory["data"]["runtime"]["recent_console_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["message"].as_str() == Some("runtime-observatory-boom")),
        "{observatory}"
    );
    assert!(
        observatory["data"]["runtime"]["recent_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["url"].as_str() == Some(server.url_for("/ping").as_str())),
        "{observatory}"
    );

    cleanup(&home);
}

/// T397: stylized label-backed controls should still confirm toggle state through the semantic checkbox target.
#[test]
#[ignore]
#[serial]
fn t397_stylized_control_label_backed_checkbox_confirms_toggle() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head>
  <title>Stylized Control Fixture</title>
  <style>
    input[type="checkbox"] {
      position: absolute;
      width: 1px;
      height: 1px;
      opacity: 0.01;
    }
    label.card {
      display: inline-flex;
      align-items: center;
      gap: 10px;
      padding: 16px;
      border: 2px solid #444;
      border-radius: 12px;
      cursor: pointer;
      user-select: none;
    }
    label.card .box {
      width: 22px;
      height: 22px;
      border: 2px solid #111;
      background: white;
    }
    input:checked + label.card .box {
      background: #1f9d55;
    }
  </style>
</head>
<body>
  <input id="optin" type="checkbox" />
  <label class="card" for="optin">
    <span class="box"></span>
    <span>Enable notifications</span>
  </label>
  <div id="status">off</div>
  <script>
    const input = document.getElementById('optin');
    input.addEventListener('change', () => {
      document.getElementById('status').textContent = input.checked ? 'on' : 'off';
    });
  </script>
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

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let checkbox = find_element_index(&state, |element| {
        element["tag"].as_str() == Some("checkbox")
    });

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", &checkbox.to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["semantic_class"], "toggle_state",
        "{clicked}"
    );
    assert_eq!(
        clicked["data"]["interaction"]["confirmation_kind"], "toggle_state",
        "{clicked}"
    );

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "on", "{status}");

    cleanup(&home);
}

/// T398: modal workflow fixtures should be directly exercisable through open -> confirm progression.
#[test]
#[ignore]
#[serial]
fn t398_modal_workflow_fixture_progresses_inside_dialog() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head>
  <title>Modal Workflow Fixture</title>
  <style>
    #modal[hidden] { display: none; }
    #modal {
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.45);
      display: flex;
      align-items: center;
      justify-content: center;
    }
    #dialog {
      background: white;
      padding: 20px;
      border-radius: 12px;
      min-width: 260px;
    }
  </style>
</head>
<body>
  <button id="launch">Launch workflow</button>
  <div id="status">idle</div>
  <div id="modal" hidden>
    <div id="dialog" role="dialog" aria-modal="true">
      <p>Confirm launch</p>
      <button id="confirm">Confirm</button>
    </div>
  </div>
  <script>
    const modal = document.getElementById('modal');
    const status = document.getElementById('status');
    document.getElementById('launch').addEventListener('click', () => {
      modal.hidden = false;
      status.textContent = 'modal-open';
    });
    document.getElementById('confirm').addEventListener('click', () => {
      modal.hidden = true;
      status.textContent = 'confirmed';
    });
  </script>
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

    let initial = run_state(&home);
    let initial_snapshot = snapshot_id(&initial);
    let launch = find_element_index(&initial, |element| {
        element["text"].as_str() == Some("Launch workflow")
    });
    let launched = parse_json(
        &rub_cmd(&home)
            .args([
                "click",
                &launch.to_string(),
                "--snapshot",
                &initial_snapshot,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(launched["success"], true, "{launched}");
    assert_eq!(
        launched["data"]["interaction"]["confirmation_kind"], "page_mutation",
        "{launched}"
    );

    let modal_state = run_state(&home);
    let modal_snapshot = snapshot_id(&modal_state);
    let confirm = find_element_index(&modal_state, |element| {
        element["text"].as_str() == Some("Confirm")
    });
    let confirmed = parse_json(
        &rub_cmd(&home)
            .args(["click", &confirm.to_string(), "--snapshot", &modal_snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(confirmed["success"], true, "{confirmed}");
    assert_eq!(
        confirmed["data"]["interaction"]["confirmation_kind"], "page_mutation",
        "{confirmed}"
    );

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "confirmed", "{status}");

    cleanup(&home);
}

/// T399: repeated-card fixtures should preserve per-card targeting instead of collapsing identical CTA buttons.
#[test]
#[ignore]
#[serial]
fn t399_dense_card_fixture_targets_the_correct_repeated_card() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Dense Card Fixture</title></head>
<body>
  <div id="status">none</div>
  <section class="grid">
    <article class="card" data-card="alpha">
      <h2>Alpha</h2>
      <button>View details</button>
    </article>
    <article class="card" data-card="beta">
      <h2>Beta</h2>
      <button>View details</button>
    </article>
    <article class="card" data-card="gamma">
      <h2>Gamma</h2>
      <button>View details</button>
    </article>
  </section>
  <script>
    for (const card of document.querySelectorAll('.card')) {
      card.querySelector('button').addEventListener('click', () => {
        document.getElementById('status').textContent = card.dataset.card;
      });
    }
  </script>
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

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let matching = state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|element| {
            element["tag"].as_str() == Some("button")
                && element["text"].as_str() == Some("View details")
        })
        .map(|element| element["index"].as_u64().unwrap() as u32)
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 3, "{state}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", &matching[1].to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "beta", "{status}");

    cleanup(&home);
}

/// T400: runtime interference should classify interstitial-style navigation drift.
#[test]
#[ignore]
#[serial]
fn t400_runtime_interference_classifies_interstitial_navigation() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Primary Page</title></head>
<body><h1>Primary</h1></body>
</html>"#,
        ),
        (
            "/interstitial",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Interstitial Notice</title></head>
<body><h1>Interstitial</h1></body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let baseline = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(baseline["success"], true, "{baseline}");
    assert_eq!(
        baseline["data"]["runtime"]["status"], "inactive",
        "{baseline}"
    );

    let url = format!("{}#vignette", server.url_for("/interstitial"));
    let drifted = parse_json(
        &rub_cmd(&home)
            .args(["exec", &format!("location.href = '{}'", url)])
            .output()
            .unwrap(),
    );
    assert_eq!(drifted["success"], true, "{drifted}");

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "active", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["current_interference"]["kind"], "interstitial_navigation",
        "{runtime}"
    );
    assert_eq!(
        runtime["data"]["runtime"]["current_interference"]["current_url"], url,
        "{runtime}"
    );

    cleanup(&home);
}

/// T400a: explicit `open` should promote the primary context so same-host assets
/// on the new page are not misclassified as third-party noise.
#[test]
#[ignore]
#[serial]
fn t400a_open_promotes_primary_context_before_noise_classification() {
    let home = unique_home();
    cleanup(&home);

    let (_rt_a, server_a) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Primary A</title></head>
<body><h1>Primary A</h1></body>
</html>"#,
    )]);
    let (_rt_b, server_b) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Primary B</title></head>
<body>
  <h1>Primary B</h1>
  <img src="/missing-1.png" />
  <img src="/missing-2.png" />
  <img src="/missing-3.png" />
</body>
</html>"#,
    )]);

    let opened_a = parse_json(
        &rub_cmd(&home)
            .args(["open", &server_a.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_a["success"], true, "{opened_a}");

    let opened_b = parse_json(
        &rub_cmd(&home)
            .args(["open", &server_b.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_b["success"], true, "{opened_b}");

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["status"], "inactive",
        "{runtime}"
    );
    assert_eq!(
        runtime["data"]["runtime"]["current_interference"],
        serde_json::Value::Null,
        "{runtime}"
    );

    cleanup(&home);
}

/// T401: interference recover should back-navigate out of an interstitial drift and restore the primary context.
#[test]
#[ignore]
#[serial]
fn t401_interference_recover_restores_primary_context_after_interstitial() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Primary Page</title></head>
<body><h1>Primary</h1></body>
</html>"#,
        ),
        (
            "/interstitial",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Interstitial Notice</title></head>
<body><h1>Interstitial</h1></body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let baseline = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(baseline["success"], true, "{baseline}");
    assert_eq!(
        baseline["data"]["runtime"]["status"], "inactive",
        "{baseline}"
    );

    let url = format!("{}#vignette", server.url_for("/interstitial"));
    let drifted = parse_json(
        &rub_cmd(&home)
            .args(["exec", &format!("location.href = '{}'", url)])
            .output()
            .unwrap(),
    );
    assert_eq!(drifted["success"], true, "{drifted}");

    let recovered = parse_json(
        &rub_cmd(&home)
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        recovered["data"]["recovery"]["action"], "back_navigate",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["result"], "succeeded",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["fence_satisfied"], true,
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["runtime"]["status"], "inactive",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["runtime"]["last_recovery_action"], "back_navigate",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["runtime"]["last_recovery_result"], "succeeded",
        "{recovered}"
    );

    let title = parse_json(&rub_cmd(&home).args(["get", "title"]).output().unwrap());
    assert_eq!(title["success"], true, "{title}");
    assert_eq!(title["data"]["result"]["value"], "Primary Page", "{title}");

    cleanup(&home);
}

/// T402: interference recover should close an unexpected popup tab and restore the primary context.
#[test]
#[ignore]
#[serial]
fn t402_interference_recover_closes_popup_hijack() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Popup Source</title></head>
<body>
  <button id="open-popup" onclick="window.open('/popup', '_blank')">Open Popup</button>
</body>
</html>"#,
        ),
        (
            "/popup",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Popup Target</title></head>
<body><h1>Popup Ready</h1></body>
</html>"#,
        ),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let baseline = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(baseline["success"], true, "{baseline}");
    assert_eq!(
        baseline["data"]["runtime"]["status"], "inactive",
        "{baseline}"
    );

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let popup_button = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Open Popup")
    });

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", &popup_button.to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let tabs_json = (0..30)
        .find_map(|_| {
            let out = rub_cmd(&home).arg("tabs").output().unwrap();
            let json = parse_json(&out);
            if json["data"]["result"]["items"]
                .as_array()
                .map(|items| items.len() as u64)
                .unwrap_or(0)
                >= 2
            {
                Some(json)
            } else {
                std::thread::sleep(Duration::from_millis(100));
                None
            }
        })
        .expect("popup tab should appear within 3s");
    let popup_index = tabs_json["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Popup Target"))
        .and_then(|tab| tab["index"].as_u64())
        .expect("popup tab index") as u32;

    let switched = parse_json(
        &rub_cmd(&home)
            .args(["switch", &popup_index.to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let interference = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(interference["success"], true, "{interference}");
    assert_eq!(
        interference["data"]["runtime"]["status"], "active",
        "{interference}"
    );
    assert_eq!(
        interference["data"]["runtime"]["current_interference"]["kind"], "popup_hijack",
        "{interference}"
    );

    let recovered = parse_json(
        &rub_cmd(&home)
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        recovered["data"]["recovery"]["action"], "close_unexpected_tab",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["result"], "succeeded",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["runtime"]["status"], "inactive",
        "{recovered}"
    );

    let tabs_after = parse_json(&rub_cmd(&home).arg("tabs").output().unwrap());
    assert_eq!(tabs_after["success"], true, "{tabs_after}");
    assert_eq!(
        tabs_after["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1,
        "{tabs_after}"
    );
    assert_eq!(
        tabs_after["data"]["result"]["items"][0]["title"], "Popup Source",
        "{tabs_after}"
    );
    assert_eq!(
        tabs_after["data"]["result"]["items"][0]["active"], true,
        "{tabs_after}"
    );

    cleanup(&home);
}
