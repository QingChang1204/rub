use super::*;

/// T370/T371/T386/T387: listener and doctor runtime projections should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t370_387_listener_and_doctor_projection_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
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
        ),
        (
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
        ),
        (
            "/observatory",
            "text/html",
            r#"<!DOCTYPE html><html><body><h1>Observatory Fixture</h1></body></html>"#,
        ),
        ("/observatory-data", "application/json", r#"{"ok":true}"#),
        (
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
        ),
    ]);

    let out = session
        .cmd()
        .args(["open", &server.url_for("/listeners")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = session
        .cmd()
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

    let out = session
        .cmd()
        .args(["open", &server.url_for("/listeners-default")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = session.cmd().arg("state").output().unwrap();
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

    let out = session
        .cmd()
        .args(["open", &server.url_for("/observatory")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = session
        .cmd()
        .args([
            "exec",
            "console.error('rub observatory'); fetch('/observatory-data').then((r) => r.text())",
        ])
        .output()
        .unwrap();
    let exec_json = parse_json(&out);
    assert_eq!(exec_json["success"], true, "{exec_json}");

    let out = session.cmd().arg("doctor").output().unwrap();
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

    let out = session
        .cmd()
        .args(["open", &server.url_for("/runtime-state")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = session.cmd().arg("doctor").output().unwrap();
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
}

/// T388-T389c: external handoff/takeover flows should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t388_389c_external_handoff_and_takeover_grouped_scenario() {
    let (_rt, server) = start_standard_site_fixture();
    let (mut chrome, cdp_origin, profile_dir) = match spawn_external_chrome(Some(&server.url())) {
        Ok(Some(spawned)) => spawned,
        Ok(None) => {
            eprintln!("Skipping external takeover test because no Chrome/Chromium binary was found");
            return;
        }
        Err(error) => panic!("external takeover helper launch/readiness failed: {error}"),
    };
    let session = ManagedBrowserSession::new();
    let _home = session.home();

    let doctor = parse_json(
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "doctor"])
            .output()
            .unwrap(),
    );
    let doctor_runtime = doctor_runtime(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        doctor_runtime["human_verification_handoff"]["status"],
        "available"
    );
    assert_eq!(
        doctor_runtime["human_verification_handoff"]["automation_paused"],
        false
    );
    assert_eq!(
        doctor_runtime["human_verification_handoff"]["resume_supported"],
        true
    );
    assert_eq!(doctor_runtime["integration_runtime"]["handoff_ready"], true);

    let state = parse_json(
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "state"])
            .output()
            .unwrap(),
    );
    assert_eq!(state["success"], true, "{state}");

    let handoff_started = parse_json(
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "handoff", "start"])
            .output()
            .unwrap(),
    );
    assert_eq!(handoff_started["success"], true, "{handoff_started}");
    assert_eq!(handoff_started["data"]["runtime"]["status"], "active");
    assert_eq!(
        handoff_started["data"]["runtime"]["automation_paused"],
        true
    );

    let blocked = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "handoff", "complete"])
            .output()
            .unwrap(),
    );
    assert_eq!(completed["success"], true, "{completed}");
    assert_eq!(completed["data"]["runtime"]["status"], "completed");
    assert_eq!(completed["data"]["runtime"]["automation_paused"], false);

    let resumed_exec = parse_json(
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(resumed_exec["success"], true, "{resumed_exec}");
    assert_eq!(resumed_exec["data"]["result"], 4);

    let handoff_binding = parse_json(
        &session
            .cmd()
            .args([
                "--cdp-url",
                &cdp_origin,
                "binding",
                "capture",
                "external-handoff",
            ])
            .output()
            .unwrap(),
    );
    assert_binding_result_auth(
        &handoff_binding,
        "capture",
        "external-handoff",
        "handoff_completed",
        "human",
        Some("handoff_complete"),
    );
    assert_binding_capture_candidate(
        &handoff_binding,
        "capture_ready",
        Some("handoff_complete"),
        Some("external_reattachment_required"),
        None,
        Some("external_reattach_required"),
    );

    let runtime = parse_json(
        &session
            .cmd()
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

    let started = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(blocked["success"], false, "{blocked}");
    assert_eq!(blocked["error"]["code"], "AUTOMATION_PAUSED");

    let resumed = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["--cdp-url", &cdp_origin, "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["data"]["result"], 4);

    let takeover_binding = parse_json(
        &session
            .cmd()
            .args([
                "--cdp-url",
                &cdp_origin,
                "binding",
                "capture",
                "external-takeover",
            ])
            .output()
            .unwrap(),
    );
    assert_binding_result_auth(
        &takeover_binding,
        "capture",
        "external-takeover",
        "takeover_resumed",
        "human",
        Some("takeover_resume"),
    );
    assert_binding_capture_candidate(
        &takeover_binding,
        "capture_ready",
        Some("takeover_resume"),
        None,
        None,
        None,
    );

    let _ = session.cmd().arg("close").output();
    terminate_external_chrome(&mut chrome, &profile_dir);
}

/// T389d: managed headed sessions should support takeover start/resume without external attach.
#[test]
#[ignore]
#[serial]
fn t389d_takeover_start_and_resume_follow_managed_headed_session_state() {
    let (_rt, server) = start_standard_site_fixture();
    let session = ManagedBrowserSession::new();
    let _home = session.home();

    let opened = parse_json(
        &session
            .cmd()
            .args(["--headed", "open", server.url().as_str()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let status = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["--headed", "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(blocked["success"], false, "{blocked}");
    assert_eq!(blocked["error"]["code"], "AUTOMATION_PAUSED");

    let resumed = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["--headed", "exec", "2+2"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["data"]["result"], 4);

    let _ = session.cmd().arg("close").output();
}

/// T389e: managed headless sessions should elevate to visible takeover before start/resume.
#[test]
#[ignore]
#[serial]
fn t389e_takeover_elevate_promotes_managed_headless_session_to_visible_control() {
    let (_rt, server) = start_standard_site_fixture();
    let session = ManagedBrowserSession::new();
    let _home = session.home();

    let _opened = open_and_assert_success(session.cmd(), server.url().as_str());

    let status = parse_json(&session.cmd().args(["takeover", "status"]).output().unwrap());
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

    let rejected = parse_json(&session.cmd().args(["takeover", "start"]).output().unwrap());
    assert_eq!(rejected["success"], false, "{rejected}");
    assert_eq!(rejected["error"]["code"], "INVALID_INPUT");
    assert_eq!(
        rejected["error"]["context"]["takeover_runtime"]["unavailable_reason"],
        "elevation_required"
    );

    let elevated = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["--headed", "takeover", "status"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "available");
    assert_eq!(runtime["data"]["runtime"]["visibility_mode"], "headed");

    let state = parse_json(&session.cmd().args(["--headed", "state"]).output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(
        state["data"]["result"]["snapshot"]["title"],
        "Example Domain"
    );

    let started = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
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

    let _ = session.cmd().arg("close").output();
}

/// T389f: bind-current should preserve temp-home ephemerality instead of claiming durable account memory.
#[test]
#[ignore]
#[serial]
fn t389f_bind_current_marks_temp_home_runtime_ephemeral() {
    let (_rt, server) = start_standard_site_fixture();
    let session = ManagedBrowserSession::new();
    let _home = session.home();

    let _opened = open_and_assert_success(session.cmd(), server.url().as_str());

    let bound = parse_json(
        &session
            .cmd()
            .args(["binding", "bind-current", "temp-home"])
            .output()
            .unwrap(),
    );
    assert_binding_result_auth(
        &bound,
        "bind_current",
        "temp-home",
        "bound_existing_runtime",
        "unknown",
        None,
    );
    assert_eq!(
        bound["data"]["result"]["binding"]["persistence_policy"],
        "rub_home_local_ephemeral"
    );
    assert_binding_capture_candidate(
        &bound,
        "bind_current_only",
        None,
        Some("rub_home_local_ephemeral"),
        Some("rub_home_local_ephemeral"),
        Some("temp_home_ephemeral"),
    );

    let _ = session.cmd().arg("close").output();
}

/// T389g: explicit CLI-auth capture should stay operator-fenced instead of depending on heuristic capture-ready state.
#[test]
#[ignore]
#[serial]
fn t389g_capture_after_cli_auth_completion_uses_explicit_cli_fence() {
    let (_rt, server) = start_standard_site_fixture();
    let session = ManagedBrowserSession::new();
    let _home = session.home();

    let _opened = open_and_assert_success(session.cmd(), server.url().as_str());

    let scripted = parse_json(
        &session
            .cmd()
            .args([
                "exec",
                "document.cookie = 'session=ok; path=/'; localStorage.setItem('authToken', 'ok'); 'done'",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(scripted["success"], true, "{scripted}");
    assert_eq!(scripted["data"]["result"], "done");

    let captured = parse_json(
        &session
            .cmd()
            .args(["binding", "capture", "cli-auth", "--auth-input", "cli"])
            .output()
            .unwrap(),
    );
    assert_binding_result_auth(
        &captured,
        "capture",
        "cli-auth",
        "cli_auth_completed",
        "cli",
        Some("explicit_cli_auth_capture"),
    );
    assert_binding_capture_candidate(&captured, "bind_current_only", None, None, None, None);

    let _ = session.cmd().arg("close").output();
}

/// T389h: `--use` should reuse a remembered live binding through the top-level route.
#[test]
#[ignore]
#[serial]
fn t389h_use_alias_reuses_live_binding_through_top_level_route() {
    let (_rt, server) = start_standard_site_fixture();
    let session = ManagedBrowserSession::new();
    let _home = session.home();

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", server.url().as_str()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    bind_current_and_remember_alias(
        session.cmd(),
        session.cmd(),
        "old-admin",
        "finance",
        "workspace",
    );

    let doctor = parse_json(
        &session
            .cmd()
            .args(["--use", "finance", "doctor"])
            .output()
            .unwrap(),
    );
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        doctor["data"]["binding_resolution"]["requested_alias"], "finance",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["binding_alias"], "old-admin",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["mode"], "reuse_live_session",
        "{doctor}"
    );
    assert_eq!(
        doctor_runtime(&doctor)["frame_runtime"]["current_frame"]["url"],
        server.url_for("/"),
        "{doctor}"
    );

    let _ = session.cmd().arg("close").output();
}

/// T389i: `--use` should launch a bound runtime through the top-level route when no live match exists.
#[test]
#[ignore]
#[serial]
fn t389i_use_alias_launches_bound_runtime_when_no_live_match() {
    let (_rt, server) = start_standard_site_fixture();
    let session = ManagedBrowserSession::new();
    let home = session.home().to_string();
    let profile_root = format!("{home}/remembered-runtime");

    let _opened = open_and_assert_success(
        {
            let mut cmd = rub_cmd(&home);
            cmd.args(["--user-data-dir", &profile_root]);
            cmd
        },
        server.url().as_str(),
    );

    let bound = parse_json(
        &rub_cmd(&home)
            .args(["binding", "bind-current", "old-admin"])
            .output()
            .unwrap(),
    );
    assert_eq!(bound["success"], true, "{bound}");
    assert_eq!(
        bound["data"]["result"]["binding"]["persistence_policy"], "rub_home_local_durable",
        "{bound}"
    );
    assert_eq!(
        bound["data"]["result"]["binding"]["user_data_dir_reference"], profile_root,
        "{bound}"
    );

    let remembered = parse_json(
        &rub_cmd(&home)
            .args([
                "binding",
                "remember",
                "finance",
                "--binding",
                "old-admin",
                "--kind",
                "workspace",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(remembered["success"], true, "{remembered}");

    let closed = parse_json(&rub_cmd(&home).args(["close", "--all"]).output().unwrap());
    assert_eq!(closed["success"], true, "{closed}");

    let _no_live_match = wait_for_no_live_sessions(&home);

    let doctor = parse_json(
        &rub_cmd(&home)
            .args(["--use", "finance", "doctor"])
            .output()
            .unwrap(),
    );
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        doctor["data"]["binding_resolution"]["requested_alias"], "finance",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["binding_alias"], "old-admin",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["mode"], "launch_bound_runtime",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["effective_user_data_dir"], profile_root,
        "{doctor}"
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    let session_items = sessions["data"]["result"]["items"]
        .as_array()
        .expect("sessions items must be an array");
    assert!(
        session_items
            .iter()
            .any(|entry| entry["user_data_dir"] == profile_root),
        "expected relaunched session to use remembered user_data_dir: {sessions}"
    );

    let _ = rub_cmd(&home).arg("close").output();
}

/// T389j: `--use` should launch a bound profile through the top-level route when no live match exists.
#[test]
#[ignore]
#[serial]
fn t389j_use_alias_launches_bound_profile_when_no_live_match() {
    let session = ManagedBrowserSession::new();
    let home = session.home().to_string();
    let (fake_home, resolved_profile, envs_owned) = prepare_fake_profile_env();
    let (_rt, server) = start_standard_site_fixture();
    let envs = envs_owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();

    let opened = parse_json(
        &rub_cmd_env(&home, &envs)
            .args(["--profile", "Default", "open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    bind_current_and_remember_alias(
        rub_cmd_env(&home, &envs),
        rub_cmd_env(&home, &envs),
        "old-admin",
        "finance",
        "workspace",
    );

    let closed = parse_json(
        &rub_cmd_env(&home, &envs)
            .args(["close", "--all"])
            .output()
            .unwrap(),
    );
    assert_eq!(closed["success"], true, "{closed}");

    let _no_live_match = wait_for_no_live_sessions(&home);

    let doctor = parse_json(
        &rub_cmd_env(&home, &envs)
            .args(["--use", "finance", "doctor"])
            .output()
            .unwrap(),
    );
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        doctor["data"]["binding_resolution"]["requested_alias"], "finance",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["binding_alias"], "old-admin",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["mode"], "launch_bound_profile",
        "{doctor}"
    );
    assert_eq!(
        doctor["data"]["binding_resolution"]["effective_profile_dir_name"], "Default",
        "{doctor}"
    );
    assert!(
        doctor["data"]["binding_resolution"]["effective_user_data_dir"].is_null(),
        "{doctor}"
    );

    let report = doctor_result(&doctor);
    assert_eq!(
        report["launch_policy"]["connection_target"]["source"], "profile",
        "{doctor}"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["name"], "Default",
        "{doctor}"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["resolved_path"],
        resolved_profile.display().to_string(),
        "{doctor}"
    );

    let _ = rub_cmd_env(&home, &envs).arg("close").output();
    let _ = std::fs::remove_dir_all(fake_home);
}

/// T390/T391/T392/T392b: intercept rewrite/block/header-override network flows
/// should reuse one browser-backed session.
#[test]
#[ignore]
#[serial]
fn t390_392b_intercept_network_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, rewrite_server) = start_test_server(vec![
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
        &session
            .cmd()
            .args([
                "intercept",
                "rewrite",
                &rewrite_server.url_for("/api/*"),
                &rewrite_server.url_for("/mock"),
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
        rewrite_server.url_for("/api/*")
    );
    assert_eq!(added["data"]["result"]["rule"]["status"], "active");
    assert_eq!(
        added["data"]["result"]["rules"].as_array().unwrap().len(),
        1
    );
    assert_eq!(added["data"]["runtime"]["request_rule_count"], 1, "{added}");

    let doctor = parse_json(&session.cmd().arg("doctor").output().unwrap());
    let runtime = doctor_runtime(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(runtime["integration_runtime"]["status"], "active");
    assert_eq!(runtime["integration_runtime"]["request_rule_count"], 1);

    let listed = parse_json(&session.cmd().args(["intercept", "list"]).output().unwrap());
    assert_eq!(listed["success"], true, "{listed}");
    assert_eq!(listed["data"]["subject"]["kind"], "intercept_rule_registry");
    assert_eq!(
        listed["data"]["result"]["rules"].as_array().unwrap().len(),
        1
    );
    assert_eq!(listed["data"]["result"]["rules"][0]["action"], "rewrite");
    assert_eq!(
        listed["data"]["result"]["rules"][0]["pattern"],
        rewrite_server.url_for("/api/*")
    );

    let open = parse_json(
        &session
            .cmd()
            .args(["open", &rewrite_server.url_for("/app")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let waited = parse_json(
        &session
            .cmd()
            .args(["wait", "--text", "mock", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let mocked = parse_json(
        &session
            .cmd()
            .args(["exec", "document.querySelector('#status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(mocked["success"], true, "{mocked}");
    assert_eq!(mocked["data"]["result"], "mock");

    let cleared = parse_json(&session.cmd().args(["intercept", "clear"]).output().unwrap());
    assert_eq!(cleared["success"], true, "{cleared}");
    assert_eq!(cleared["data"]["result"]["cleared"], true);
    assert_eq!(cleared["data"]["result"]["rules"], json!([]));

    let listed = parse_json(&session.cmd().args(["intercept", "list"]).output().unwrap());
    assert_eq!(listed["success"], true, "{listed}");
    assert_eq!(listed["data"]["result"]["rules"], json!([]));

    let open = parse_json(
        &session
            .cmd()
            .args(["open", &rewrite_server.url_for("/app")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let waited = parse_json(
        &session
            .cmd()
            .args(["wait", "--text", "prod", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let prod = parse_json(
        &session
            .cmd()
            .args(["exec", "document.querySelector('#status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(prod["success"], true, "{prod}");
    assert_eq!(prod["data"]["result"], "prod");

    let (_rt, blocked_server) = start_test_server(vec![
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
        &session
            .cmd()
            .args([
                "intercept",
                "block",
                &blocked_server.url_for("/api/blocked"),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");
    assert_eq!(added["data"]["result"]["rule"]["action"], "block");

    let open = parse_json(
        &session
            .cmd()
            .args(["open", &blocked_server.url_for("/blocked-app")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let waited = parse_json(
        &session
            .cmd()
            .args(["wait", "--text", "error:TypeError", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let mut doctor = serde_json::Value::Null;
    let mut doctor_matched = false;
    for _ in 0..60 {
        let out = parse_json(&session.cmd().arg("doctor").output().unwrap());
        let runtime = doctor_runtime(&out);
        assert_eq!(out["success"], true, "{out}");
        doctor_matched = runtime["runtime_observatory"]["recent_network_failures"]
            .as_array()
            .is_some_and(|events| {
                events.iter().any(|event| {
                    event["url"]
                        .as_str()
                        .is_some_and(|url| url.ends_with("/api/blocked"))
                        && event["applied_rule_effects"]
                            .as_array()
                            .is_some_and(|effects| {
                                effects.iter().any(|effect| effect["kind"] == "block")
                            })
                })
            });
        doctor = out;
        if doctor_matched {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(doctor_matched, "{doctor}");

    let cleared = parse_json(&session.cmd().args(["intercept", "clear"]).output().unwrap());
    assert_eq!(cleared["success"], true, "{cleared}");
    assert_eq!(cleared["data"]["result"]["rules"], json!([]));

    let (base_url, rx, handle) = start_header_fixture_server();
    let app_url = format!("{base_url}/app");
    let capture_url = format!("{base_url}/capture");

    let added = parse_json(
        &session
            .cmd()
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

    let opened = parse_json(&session.cmd().args(["open", &app_url]).output().unwrap());
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["inspect", "network", "--match", &capture_url, "--last", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    let request = &inspected["data"]["result"]["items"][0];
    assert_eq!(request["url"], capture_url, "{inspected}");
    assert_eq!(
        request["request_headers"]["x-rub-env"], "dev",
        "{inspected}"
    );
    assert_eq!(
        request["request_headers"]["x-rub-trace"], "1",
        "{inspected}"
    );

    let mut doctor = serde_json::Value::Null;
    let mut doctor_matched = false;
    for _ in 0..60 {
        let out = parse_json(&session.cmd().arg("doctor").output().unwrap());
        let runtime = doctor_runtime(&out);
        assert_eq!(out["success"], true, "{out}");
        let rule_active = runtime["integration_runtime"]["request_rules"]
            .as_array()
            .is_some_and(|rules| {
                rules.iter().any(|rule| {
                    rule["kind"] == "header_override"
                        && rule["url_pattern"].as_str() == Some(capture_url.as_str())
                })
            });
        let request_visible = runtime["runtime_observatory"]["recent_requests"]
            .as_array()
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event["url"].as_str() == Some(capture_url.as_str()))
            });
        doctor_matched = rule_active && request_visible;
        doctor = out;
        if doctor_matched {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(doctor_matched, "{doctor}");

    let cleared = parse_json(&session.cmd().args(["intercept", "clear"]).output().unwrap());
    assert_eq!(cleared["success"], true, "{cleared}");
    assert_eq!(cleared["data"]["result"]["rules"], json!([]));

    let (base_url, rx, handle) = start_header_fixture_server();
    let app_url = format!("{base_url}/app");
    let capture_url = format!("{base_url}/capture");

    let added = parse_json(
        &session
            .cmd()
            .args(["intercept", "header", &capture_url, "x-rub-live", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(added["success"], true, "{added}");

    let opened = parse_json(&session.cmd().args(["open", &app_url]).output().unwrap());
    assert_eq!(opened["success"], true, "{opened}");

    let waited = parse_json(
        &session
            .cmd()
            .args(["wait", "--text", "ok", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let raw_request = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(raw_request.contains("x-rub-live: 1") || raw_request.contains("X-Rub-Live: 1"));
    handle.join().unwrap();

    let inspected = parse_json(
        &session
            .cmd()
            .args(["inspect", "network", "--match", &capture_url, "--last", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["items"][0]["request_headers"]["x-rub-live"], "1",
        "{inspected}"
    );
}

/// T394/T395/T396: runtime summary, interaction observatory, and observatory
/// subcommand should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t394_396_runtime_observatory_and_summary_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

    let (_rt, server) = start_test_server(vec![
        (
            "/summary",
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
        ),
        (
            "/observatory-click",
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
        (
            "/observatory-passive",
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

    let opened_summary = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/summary")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_summary["success"], true, "{opened_summary}");

    let runtime = parse_json(&session.cmd().args(["runtime"]).output().unwrap());
    assert_eq!(runtime["success"], true, "{runtime}");
    let runtime_payload = &runtime["data"]["runtime"];
    assert_eq!(
        runtime_payload["integration_runtime"]["status"], "active",
        "{runtime}"
    );
    assert_eq!(
        runtime_payload["frame_runtime"]["status"], "top",
        "{runtime}"
    );
    assert_eq!(
        runtime_payload["frame_runtime"]["current_frame"]["depth"], 0,
        "{runtime}"
    );
    assert_eq!(
        runtime_payload["dialog_runtime"]["status"], "inactive",
        "{runtime}"
    );
    assert!(
        runtime_payload["dialog_runtime"]["pending_dialog"].is_null(),
        "{runtime}"
    );
    assert_eq!(runtime_payload["interference_runtime"]["mode"], "normal");
    assert_eq!(
        runtime_payload["interference_runtime"]["status"],
        "inactive"
    );
    assert_eq!(
        runtime_payload["integration_runtime"]["active_surfaces"],
        json!(["runtime_observatory", "state_inspector", "readiness"]),
        "{runtime}"
    );
    assert_eq!(runtime_payload["storage_runtime"]["status"], "active");
    assert_eq!(
        runtime_payload["storage_runtime"]["local_storage_keys"],
        json!(["authToken"]),
        "{runtime}"
    );
    assert_eq!(
        runtime_payload["integration_runtime"]["degraded_surfaces"],
        json!([]),
        "{runtime}"
    );
    assert_eq!(
        runtime_payload["state_inspector"]["auth_signals"],
        json!(["local_storage_present", "auth_like_storage_key_present"])
    );
    assert_eq!(
        runtime_payload["readiness_state"]["blocking_signals"],
        json!(["loading_present", "route_transitioning"])
    );
    assert_eq!(
        runtime_payload["human_verification_handoff"]["status"],
        "unavailable"
    );

    let interference = parse_json(
        &session
            .cmd()
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

    let frame = parse_json(&session.cmd().args(["runtime", "frame"]).output().unwrap());
    assert_eq!(frame["success"], true, "{frame}");
    assert_eq!(frame["data"]["runtime"]["status"], "top", "{frame}");
    assert_eq!(
        frame["data"]["runtime"]["current_frame"]["depth"], 0,
        "{frame}"
    );
    assert_eq!(
        frame["data"]["runtime"]["current_frame"]["same_origin_accessible"], true,
        "{frame}"
    );

    let opened_click = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/observatory-click")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_click["success"], true, "{opened_click}");

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let button_index = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Trigger")
    });

    let clicked = parse_json(
        &session
            .cmd()
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

    let opened_passive = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/observatory-passive")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_passive["success"], true, "{opened_passive}");

    let waited = parse_json(
        &session
            .cmd()
            .args(["wait", "--text", "done", "--timeout", "10000"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let observatory = parse_json(
        &session
            .cmd()
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
}

/// T415/T416/T416b/T417/T424/T425: frame continuity, inventory, stale-frame fencing,
/// and top-frame reset should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t416_417_424_425_frame_runtime_inventory_and_stale_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/frame-switch",
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
        (
            "/frame-remove",
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
            "/nested",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Nested Frame Fixture</title></head>
<body>
  <iframe
    id="child-frame"
    name="child-frame"
    src="/frame-nested-child"
    title="Child Frame"
  ></iframe>
</body>
</html>"#,
        ),
        (
            "/frame-nested-child",
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
        &session
            .cmd()
            .args(["open", &server.url_for("/frame-switch")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let frames = parse_json(&session.cmd().arg("frames").output().unwrap());
    assert_eq!(frames["success"], true, "{frames}");
    let entries = frames["data"]["result"]["items"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{frames}");

    let top = &entries[0];
    assert_eq!(top["index"], 0, "{frames}");
    assert_eq!(top["is_current"], true, "{frames}");
    assert_eq!(top["is_primary"], true, "{frames}");
    assert_eq!(top["frame"]["depth"], 0, "{frames}");
    assert_eq!(top["frame"]["same_origin_accessible"], true, "{frames}");

    let initial_child = entries
        .iter()
        .find(|entry| entry["frame"]["name"] == "child-frame")
        .expect("expected named child frame entry");
    assert_eq!(initial_child["frame"]["depth"], 1, "{frames}");
    assert_eq!(initial_child["is_current"], false, "{frames}");
    assert_eq!(initial_child["is_primary"], false, "{frames}");
    assert_eq!(
        initial_child["frame"]["same_origin_accessible"], true,
        "{frames}"
    );
    assert_eq!(
        initial_child["frame"]["parent_frame_id"], top["frame"]["frame_id"],
        "{frames}"
    );
    assert!(
        initial_child["frame"]["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/frame-child")),
        "{frames}"
    );

    let top_state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(top_state["success"], true, "{top_state}");
    let snapshot_id = top_state["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();

    let switched = parse_json(&session.cmd().args(["frame", "1"]).output().unwrap());
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(switched["data"]["runtime"]["status"], "child", "{switched}");
    assert_eq!(
        switched["data"]["result"]["current_frame"]["name"], "child-frame",
        "{switched}"
    );

    let frames = parse_json(&session.cmd().arg("frames").output().unwrap());
    let child = frames["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["frame"]["name"] == "child-frame")
        .expect("expected named child frame entry");
    assert_eq!(child["is_current"], true, "{frames}");
    assert_eq!(child["is_primary"], false, "{frames}");

    let child_state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(child_state["success"], true, "{child_state}");
    assert_eq!(
        child_state["data"]["result"]["snapshot"]["frame_context"]["frame_id"],
        switched["data"]["result"]["current_frame"]["frame_id"],
        "{child_state}"
    );
    assert_eq!(
        child_state["data"]["result"]["snapshot"]["frame_context"]["name"], "child-frame",
        "{child_state}"
    );
    let elements = child_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 1, "{child_state}");
    assert_eq!(elements[0]["text"], "Inside Frame", "{child_state}");

    let get_text = parse_json(
        &session
            .cmd()
            .args(["get", "text", "0", "--snapshot", snapshot_id])
            .output()
            .unwrap(),
    );
    assert_eq!(get_text["success"], false, "{get_text}");
    assert_eq!(get_text["error"]["code"], "STALE_SNAPSHOT", "{get_text}");
    assert_eq!(
        get_text["error"]["context"]["snapshot_id"], snapshot_id,
        "{get_text}"
    );
    assert_eq!(
        get_text["error"]["context"]["authority_state"], "selected_frame_context_drifted",
        "{get_text}"
    );
    assert_eq!(
        get_text["error"]["context"]["authority_guidance"]["source_signal"],
        "selected_frame_context_drifted",
        "{get_text}"
    );
    assert_eq!(
        get_text["error"]["context"]["authority_guidance"]["next_command_hints"][0]["command"],
        "rub frames",
        "{get_text}"
    );

    let navigated = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/next")])
            .output()
            .unwrap(),
    );
    assert_eq!(navigated["success"], true, "{navigated}");

    let runtime = parse_json(&session.cmd().args(["runtime", "frame"]).output().unwrap());
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "top", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["current_frame"]["frame_id"],
        runtime["data"]["runtime"]["primary_frame"]["frame_id"],
        "{runtime}"
    );

    let next_state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(next_state["success"], true, "{next_state}");
    let next_elements = next_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(next_elements.len(), 1, "{next_state}");
    assert_eq!(next_elements[0]["text"], "Next Top", "{next_state}");

    let opened_nested = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/nested")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_nested["success"], true, "{opened_nested}");

    let nested_frames = parse_json(&session.cmd().arg("frames").output().unwrap());
    assert_eq!(nested_frames["success"], true, "{nested_frames}");
    let entries = nested_frames["data"]["result"]["items"].as_array().unwrap();
    let grandchild = entries
        .iter()
        .find(|entry| entry["frame"]["name"] == "grandchild-frame")
        .expect("expected grandchild frame");
    assert_eq!(grandchild["frame"]["depth"], 2, "{nested_frames}");
    assert_eq!(
        grandchild["frame"]["same_origin_accessible"], true,
        "{nested_frames}"
    );

    let switched_grandchild = parse_json(
        &session
            .cmd()
            .args(["frame", "--name", "grandchild-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        switched_grandchild["success"], true,
        "{switched_grandchild}"
    );
    assert_eq!(
        switched_grandchild["data"]["result"]["current_frame"]["name"], "grandchild-frame",
        "{switched_grandchild}"
    );

    let grandchild_state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(grandchild_state["success"], true, "{grandchild_state}");
    assert_eq!(
        grandchild_state["data"]["result"]["snapshot"]["frame_context"]["name"], "grandchild-frame",
        "{grandchild_state}"
    );
    assert_eq!(
        grandchild_state["data"]["result"]["snapshot"]["frame_lineage"][0],
        grandchild_state["data"]["result"]["snapshot"]["frame_context"]["frame_id"],
        "{grandchild_state}"
    );
    let grandchild_elements = grandchild_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(grandchild_elements.len(), 1, "{grandchild_state}");
    assert_eq!(
        grandchild_elements[0]["text"], "Inside Grandchild",
        "{grandchild_state}"
    );

    let opened_stale = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/frame-remove")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_stale["success"], true, "{opened_stale}");

    let switched_stale = parse_json(
        &session
            .cmd()
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched_stale["success"], true, "{switched_stale}");

    std::thread::sleep(Duration::from_millis(750));

    let stale_runtime = parse_json(&session.cmd().args(["runtime", "frame"]).output().unwrap());
    assert_eq!(stale_runtime["success"], true, "{stale_runtime}");
    assert_eq!(
        stale_runtime["data"]["runtime"]["status"], "stale",
        "{stale_runtime}"
    );
    assert_eq!(
        stale_runtime["data"]["runtime"]["degraded_reason"], "selected_frame_not_found",
        "{stale_runtime}"
    );
    assert_eq!(
        stale_runtime["data"]["workflow_continuity"]["source_signal"], "frame_runtime_stale",
        "{stale_runtime}"
    );
    assert_eq!(
        stale_runtime["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub frames",
        "{stale_runtime}"
    );
    assert_eq!(
        stale_runtime["data"]["workflow_continuity"]["authority_observation"]["frame_status"],
        "stale",
        "{stale_runtime}"
    );

    let stale_state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(stale_state["success"], false, "{stale_state}");
    assert_eq!(
        stale_state["error"]["code"], "STALE_SNAPSHOT",
        "{stale_state}"
    );
    assert_eq!(
        stale_state["error"]["context"]["frame_runtime"]["status"], "stale",
        "{stale_state}"
    );
}

/// T418/T419/T420/T426/T427: selected-frame input/extract/fill and ref
/// boundary behaviors should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t418_420_426_427_selected_frame_rw_and_ref_boundary_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Grouped Frame Read/Write Fixture</title></head>
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
  <h1>Child Heading</h1>
  <p class="content">Child paragraph</p>
  <input id="shared-input" value="" placeholder="Inside Frame Input" />
  <button id="child-submit" type="button" onclick="this.textContent='Saved Child'">Save Child</button>
  <button id="inside-frame">Inside Frame</button>
</body>
</html>"#,
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

    let switched = parse_json(
        &session
            .cmd()
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");
    assert_eq!(switched["data"]["runtime"]["status"], "child", "{switched}");

    let typed = parse_json(
        &session
            .cmd()
            .args(["type", "--selector", "#shared-input", "hello from frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(typed["success"], true, "{typed}");
    assert_eq!(
        typed["data"]["interaction"]["confirmation_status"], "confirmed",
        "{typed}"
    );
    assert_eq!(
        typed["data"]["interaction"]["confirmation_kind"], "value_applied",
        "{typed}"
    );

    let typed_value = parse_json(
        &session
            .cmd()
            .args(["get", "value", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(typed_value["success"], true, "{typed_value}");
    assert_eq!(
        typed_value["data"]["result"]["value"], "hello from frame",
        "{typed_value}"
    );

    let extract = parse_json(
        &session
            .cmd()
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

    let fill = parse_json(
        &session
            .cmd()
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

    let filled_value = parse_json(
        &session
            .cmd()
            .args(["get", "value", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(filled_value["success"], true, "{filled_value}");
    assert_eq!(
        filled_value["data"]["result"]["value"], "child hello",
        "{filled_value}"
    );

    let child_state = parse_json(&session.cmd().arg("state").output().unwrap());
    assert_eq!(child_state["success"], true, "{child_state}");
    let elements = child_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert!(
        elements
            .iter()
            .any(|element| element["text"] == "Saved Child"),
        "{child_state}"
    );

    let input_ref = find_element_ref(&child_state, |element| {
        element["attributes"]["placeholder"] == "Inside Frame Input"
    });
    let ref_input = parse_json(
        &session
            .cmd()
            .args(["type", "--ref", &input_ref, "--clear", "hello via ref"])
            .output()
            .unwrap(),
    );
    assert_eq!(ref_input["success"], true, "{ref_input}");
    assert_eq!(
        ref_input["data"]["interaction"]["confirmation_kind"], "value_applied",
        "{ref_input}"
    );
    assert_eq!(
        ref_input["data"]["interaction"]["frame_context"]["name"], "child-frame",
        "{ref_input}"
    );

    let ref_value = parse_json(
        &session
            .cmd()
            .args(["get", "value", "--ref", &input_ref])
            .output()
            .unwrap(),
    );
    assert_eq!(ref_value["success"], true, "{ref_value}");
    assert_eq!(
        ref_value["data"]["result"]["value"], "hello via ref",
        "{ref_value}"
    );

    let button_ref = find_element_ref(&child_state, |element| element["text"] == "Inside Frame");

    let reset = parse_json(&session.cmd().args(["frame", "--top"]).output().unwrap());
    assert_eq!(reset["success"], true, "{reset}");

    let get_text = parse_json(
        &session
            .cmd()
            .args(["get", "text", "--ref", &button_ref])
            .output()
            .unwrap(),
    );
    assert_eq!(get_text["success"], false, "{get_text}");
    assert_eq!(get_text["error"]["code"], "ELEMENT_NOT_FOUND", "{get_text}");
}

#[test]
#[ignore]
#[serial]
fn t421_423_selected_same_origin_frame_grouped_context_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Grouped Frame Fixture</title></head>
<body>
  <input id="shared-input" value="top" />
  <select id="shared-select">
    <option value="top_a" selected>Top Alpha</option>
    <option value="top_b">Top Beta</option>
  </select>
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
  <input id="shared-input" value="" />
  <select id="shared-select">
    <option value="child_a" selected>Child Alpha</option>
    <option value="child_b">Child Beta</option>
  </select>
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

    let file_path = format!(
        "/tmp/rub-frame-upload-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::fs::write(&file_path, b"frame upload").unwrap();

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let switched = parse_json(
        &session
            .cmd()
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let clicked = parse_json(
        &session
            .cmd()
            .args(["click", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let typed = parse_json(
        &session
            .cmd()
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
        typed["data"]["interaction"]["frame_context"]["name"], "child-frame",
        "{typed}"
    );

    let input_value = parse_json(
        &session
            .cmd()
            .args(["get", "value", "--selector", "#shared-input"])
            .output()
            .unwrap(),
    );
    assert_eq!(input_value["success"], true, "{input_value}");
    assert_eq!(
        input_value["data"]["result"]["value"], "child hello",
        "{input_value}"
    );

    let top_input_value = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('shared-input').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_input_value["success"], true, "{top_input_value}");
    assert_eq!(
        top_input_value["data"]["result"], "child hello",
        "{top_input_value}"
    );

    let reset_top = parse_json(&session.cmd().args(["frame", "--top"]).output().unwrap());
    assert_eq!(reset_top["success"], true, "{reset_top}");
    let top_input_value = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('shared-input').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_input_value["success"], true, "{top_input_value}");
    assert_eq!(top_input_value["data"]["result"], "top", "{top_input_value}");
    let switched = parse_json(
        &session
            .cmd()
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let selected = parse_json(
        &session
            .cmd()
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

    let select_value = parse_json(
        &session
            .cmd()
            .args(["get", "value", "--selector", "#shared-select"])
            .output()
            .unwrap(),
    );
    assert_eq!(select_value["success"], true, "{select_value}");
    assert_eq!(
        select_value["data"]["result"]["value"], "child_b",
        "{select_value}"
    );

    let top_select_value = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('shared-select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_select_value["success"], true, "{top_select_value}");
    assert_eq!(
        top_select_value["data"]["result"], "child_b",
        "{top_select_value}"
    );

    let reset_top = parse_json(&session.cmd().args(["frame", "--top"]).output().unwrap());
    assert_eq!(reset_top["success"], true, "{reset_top}");
    let top_select_value = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('shared-select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_select_value["success"], true, "{top_select_value}");
    assert_eq!(
        top_select_value["data"]["result"], "top_a",
        "{top_select_value}"
    );
    let switched = parse_json(
        &session
            .cmd()
            .args(["frame", "--name", "child-frame"])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let uploaded = parse_json(
        &session
            .cmd()
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
    assert_eq!(
        uploaded["data"]["result"]["path_state"]["truth_level"], "input_path_reference",
        "{uploaded}"
    );
    assert_eq!(
        uploaded["data"]["result"]["path_state"]["path_authority"], "router.upload.input_path",
        "{uploaded}"
    );
    assert_eq!(
        uploaded["data"]["result"]["path_state"]["path_kind"], "external_input_file",
        "{uploaded}"
    );

    let extracted = parse_json(
        &session
            .cmd()
            .args([
                "extract",
                r##"{"filename":{"selector":"#filename","kind":"text","required":true}}"##,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(extracted["success"], true, "{extracted}");
    let expected_filename = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap()
        .to_string();
    assert_eq!(
        extracted["data"]["result"]["fields"]["filename"], expected_filename,
        "{extracted}"
    );

    let top_filename = parse_json(
        &session
            .cmd()
            .args(["frame", "--top"])
            .output()
            .unwrap(),
    );
    assert_eq!(top_filename["success"], true, "{top_filename}");
    let top_filename = parse_json(
        &session
            .cmd()
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
}

// T396 is covered by `t394_396_runtime_observatory_and_summary_grouped_scenario`.

/// T397/T398/T399: isolated single-page UI flows should reuse one
/// browser-backed session.
#[test]
#[ignore]
#[serial]
fn t397_399_stylized_modal_and_dense_card_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/stylized",
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
        ),
        (
            "/modal",
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
        ),
        (
            "/cards",
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
        ),
    ]);

    let opened_stylized = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/stylized")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_stylized["success"], true, "{opened_stylized}");

    let state = parse_json(&session.cmd().arg("state").output().unwrap());
    let snapshot = snapshot_id(&state);
    let checkbox = find_element_index(&state, |element| {
        element["tag"].as_str() == Some("checkbox")
    });
    let clicked = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "on", "{status}");

    let opened_modal = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/modal")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_modal["success"], true, "{opened_modal}");

    let initial = parse_json(&session.cmd().arg("state").output().unwrap());
    let initial_snapshot = snapshot_id(&initial);
    let launch = find_element_index(&initial, |element| {
        element["text"].as_str() == Some("Launch workflow")
    });
    let launched = parse_json(
        &session
            .cmd()
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

    let modal_state = parse_json(&session.cmd().arg("state").output().unwrap());
    let modal_snapshot = snapshot_id(&modal_state);
    let confirm = find_element_index(&modal_state, |element| {
        element["text"].as_str() == Some("Confirm")
    });
    let confirmed = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "confirmed", "{status}");

    let opened_cards = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/cards")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_cards["success"], true, "{opened_cards}");

    let state = parse_json(&session.cmd().arg("state").output().unwrap());
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
        &session
            .cmd()
            .args(["click", &matching[1].to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    let status = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "beta", "{status}");
}

/// T400-T402: interference classification, recovery, primary-context promotion,
/// and explicit popup switching should share a single browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t400_402_interference_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

    let (_rt_a, server_a) = start_test_server(vec![
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
        (
            "/popup-source",
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

    let opened_primary = parse_json(
        &session
            .cmd()
            .args(["open", &server_a.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_primary["success"], true, "{opened_primary}");

    let baseline = parse_json(
        &session
            .cmd()
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(baseline["success"], true, "{baseline}");
    assert_eq!(
        baseline["data"]["runtime"]["status"], "inactive",
        "{baseline}"
    );

    let interstitial_url = format!("{}#vignette", server_a.url_for("/interstitial"));
    let drifted = parse_json(
        &session
            .cmd()
            .args(["exec", &format!("location.href = '{}'", interstitial_url)])
            .output()
            .unwrap(),
    );
    assert_eq!(drifted["success"], true, "{drifted}");

    let active_interference = parse_json(
        &session
            .cmd()
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        active_interference["success"], true,
        "{active_interference}"
    );
    assert_eq!(
        active_interference["data"]["runtime"]["status"], "active",
        "{active_interference}"
    );
    assert_eq!(
        active_interference["data"]["runtime"]["current_interference"]["kind"],
        "interstitial_navigation",
        "{active_interference}"
    );
    assert_eq!(
        active_interference["data"]["runtime"]["current_interference"]["current_url"],
        interstitial_url,
        "{active_interference}"
    );
    assert_eq!(
        active_interference["data"]["workflow_continuity"]["source_signal"],
        "interstitial_navigation",
        "{active_interference}"
    );
    assert_eq!(
        active_interference["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub interference recover",
        "{active_interference}"
    );
    assert_eq!(
        active_interference["data"]["workflow_continuity"]["authority_observation"]["interference_kind"],
        "interstitial_navigation",
        "{active_interference}"
    );

    let recovered = parse_json(
        &session
            .cmd()
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        recovered["data"]["result"]["recovery"]["action"], "back_navigate",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["result"]["recovery"]["result"], "succeeded",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["result"]["recovery"]["fence_satisfied"], true,
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

    let title = parse_json(&session.cmd().args(["get", "title"]).output().unwrap());
    assert_eq!(title["success"], true, "{title}");
    assert_eq!(title["data"]["result"]["value"], "Primary Page", "{title}");

    let opened_secondary = parse_json(
        &session
            .cmd()
            .args(["open", &server_b.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_secondary["success"], true, "{opened_secondary}");

    let secondary_interference = parse_json(
        &session
            .cmd()
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        secondary_interference["success"], true,
        "{secondary_interference}"
    );
    assert_eq!(
        secondary_interference["data"]["runtime"]["status"], "inactive",
        "{secondary_interference}"
    );
    assert_eq!(
        secondary_interference["data"]["runtime"]["current_interference"],
        serde_json::Value::Null,
        "{secondary_interference}"
    );

    let opened_popup_source = parse_json(
        &session
            .cmd()
            .args(["open", &server_a.url_for("/popup-source")])
            .output()
            .unwrap(),
    );
    assert_eq!(
        opened_popup_source["success"], true,
        "{opened_popup_source}"
    );

    let state = run_state(home);
    let snapshot = snapshot_id(&state);
    let popup_button = find_element_index(&state, |element| {
        element["text"].as_str() == Some("Open Popup")
    });

    let clicked = parse_json(
        &session
            .cmd()
            .args(["click", &popup_button.to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], false, "{clicked}");
    assert_eq!(
        clicked["error"]["code"],
        "INTERACTION_NOT_CONFIRMED",
        "{clicked}"
    );
    assert_eq!(
        clicked["error"]["context"]["committed_response_projection"]["interaction"]
            ["interference"]["after"]["current_interference"]["kind"],
        "popup_hijack",
        "{clicked}"
    );

    let tabs_json = (0..30)
        .find_map(|_| {
            let out = session.cmd().arg("tabs").output().unwrap();
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
    let active_tab = tabs_json["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["active"] == true)
        .expect("tabs projection should mark one active tab");
    assert!(matches!(
        active_tab["active_authority"].as_str(),
        Some("browser_truth" | "local_fallback")
    ));

    let switched = parse_json(
        &session
            .cmd()
            .args(["switch", &popup_index.to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(switched["success"], true, "{switched}");

    let popup_interference = parse_json(
        &session
            .cmd()
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(popup_interference["success"], true, "{popup_interference}");
    assert_eq!(
        popup_interference["data"]["runtime"]["status"], "inactive",
        "{popup_interference}"
    );
    assert_eq!(
        popup_interference["data"]["runtime"]["current_interference"],
        serde_json::Value::Null,
        "{popup_interference}"
    );

    let popup_recovered = parse_json(
        &session
            .cmd()
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(popup_recovered["success"], true, "{popup_recovered}");
    assert_eq!(
        popup_recovered["data"]["result"]["recovery"]["attempted"], false,
        "{popup_recovered}"
    );
    assert_eq!(
        popup_recovered["data"]["result"]["recovery"]["fence_satisfied"], false,
        "{popup_recovered}"
    );
    assert_eq!(
        popup_recovered["data"]["result"]["recovery"]["reason"], "no_active_interference",
        "{popup_recovered}"
    );
    assert_eq!(
        popup_recovered["data"]["runtime"]["status"], "inactive",
        "{popup_recovered}"
    );

    let tabs_after = parse_json(&session.cmd().arg("tabs").output().unwrap());
    assert_eq!(tabs_after["success"], true, "{tabs_after}");
    assert_eq!(
        tabs_after["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{tabs_after}"
    );
    assert_eq!(
        tabs_after["data"]["result"]["active_tab"]["title"], "Popup Target",
        "{tabs_after}"
    );
    assert_eq!(
        tabs_after["data"]["result"]["active_tab"]["active"], true,
        "{tabs_after}"
    );
}
