use super::*;

// ── v1.1: US6 Cookie Management ─────────────────────────────────────

/// T056a: `cookies set` + `cookies get` round trip.
#[test]
#[ignore]
#[serial]
fn t250_cookies_set_get() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    rub_cmd(&home)
        .args([
            "cookies",
            "set",
            "test_cookie",
            "abc123",
            "--same-site",
            "Lax",
            "--expires",
            "4102444800",
        ])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["cookies", "get"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "cookies");
    let cookies = json["data"]["result"]["cookies"].as_array().unwrap();
    let found = cookies.iter().any(|c| {
        c["name"] == "test_cookie"
            && c["value"] == "abc123"
            && c["same_site"] == "Lax"
            && c["expires"].is_number()
    });
    assert!(found, "Set cookie should appear in get results");

    cleanup(&home);
}

/// T056b: `cookies clear` removes all cookies.
#[test]
#[ignore]
#[serial]
fn t251_cookies_clear() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    rub_cmd(&home)
        .args(["cookies", "set", "temp", "val"])
        .output()
        .unwrap();

    rub_cmd(&home).args(["cookies", "clear"]).output().unwrap();

    let out = rub_cmd(&home).args(["cookies", "get"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(
        json["data"]["result"]["cookies"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        0,
        "After clear, cookies should be empty"
    );

    cleanup(&home);
}

/// T021e: wait does not block the FIFO queue; another command succeeds while wait is in progress.
#[test]
#[ignore]
#[serial]
fn t214_wait_does_not_block_queue() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/wait",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Wait Fixture</title></head>
<body>
  <div id="status">Loading</div>
  <script>
    setTimeout(() => {
      const el = document.createElement('div');
      el.className = 'delayed';
      el.textContent = 'Ready';
      document.body.appendChild(el);
    }, 1200);
  </script>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/wait")])
        .output()
        .unwrap();

    let wait_home = home.clone();
    let waiter = std::thread::spawn(move || {
        rub_cmd(&wait_home)
            .args(["wait", "--selector", ".delayed", "--timeout", "5000"])
            .output()
            .unwrap()
    });

    std::thread::sleep(Duration::from_millis(200));
    let start = std::time::Instant::now();
    let out = rub_cmd(&home).args(["get", "title"]).output().unwrap();
    let elapsed = start.elapsed();
    let json = parse_json(&out);
    assert_eq!(
        json["success"], true,
        "get title should succeed during wait"
    );
    assert_eq!(json["data"]["result"]["value"], "Wait Fixture");
    assert!(
        elapsed < Duration::from_millis(1000),
        "concurrent command should not sit behind wait, elapsed={elapsed:?}"
    );

    let wait_json = parse_json(&waiter.join().unwrap());
    assert_eq!(wait_json["success"], true);
    assert_eq!(wait_json["data"]["result"]["matched"], true);

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t215_concurrent_first_command_serializes_startup() {
    let home = unique_home();
    cleanup(&home);

    let home_a = home.clone();
    let home_b = home.clone();

    let worker_a = std::thread::spawn(move || rub_cmd(&home_a).arg("doctor").output().unwrap());
    let worker_b = std::thread::spawn(move || rub_cmd(&home_b).arg("doctor").output().unwrap());

    let json_a = parse_json(&worker_a.join().unwrap());
    let json_b = parse_json(&worker_b.join().unwrap());

    assert_eq!(
        json_a["success"], true,
        "first concurrent doctor should succeed"
    );
    assert_eq!(
        json_b["success"], true,
        "second concurrent doctor should succeed"
    );
    assert_eq!(json_a["error"], Value::Null);
    assert_eq!(json_b["error"], Value::Null);

    let registry = rub_daemon::session::read_registry(std::path::Path::new(&home))
        .expect("registry should be readable after concurrent startup");
    assert_eq!(
        registry.sessions.len(),
        1,
        "concurrent startup should converge on a single daemon authority"
    );
    assert_eq!(
        daemon_processes_for_home(&home).len(),
        1,
        "concurrent startup should leave exactly one daemon process for the home"
    );

    cleanup(&home);
}

/// T030d: a popup tab appears in `tabs` and can be switched to.
#[test]
#[ignore]
#[serial]
fn t223_popup_tab_lifecycle() {
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

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let state_json = run_state(&home);
    let snap_id = snapshot_id(&state_json);
    let popup_button = find_element_index(&state_json, |element| {
        element["text"].as_str() == Some("Open Popup")
    });

    let out = rub_cmd(&home)
        .args(["click", &popup_button.to_string(), "--snapshot", &snap_id])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

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
    assert_eq!(tabs_json["success"], true);
    assert!(
        tabs_json["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default()
            >= 2
    );
    let popup_index = tabs_json["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Popup Target"))
        .and_then(|tab| tab["index"].as_u64())
        .unwrap()
        .to_string();

    let out = rub_cmd(&home)
        .args(["switch", &popup_index])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["result"]["active_tab"]["title"],
        "Popup Target"
    );

    cleanup(&home);
}

/// T072a/T072b: `state --a11y` augments existing elements without changing indices or totals.
#[test]
#[ignore]
#[serial]
fn t233_state_a11y_augments_existing_elements() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/a11y",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>A11y Fixture</title></head>
<body>
  <button aria-label="Launch Rocket">Go</button>
  <button>Cancel</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/a11y")])
        .output()
        .unwrap();

    let base_state = run_state(&home);
    let a11y_state = parse_json(&rub_cmd(&home).args(["state", "--a11y"]).output().unwrap());

    assert_eq!(a11y_state["success"], true);
    assert_eq!(
        base_state["data"]["result"]["snapshot"]["total_count"],
        a11y_state["data"]["result"]["snapshot"]["total_count"]
    );

    let base_elements = base_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    let a11y_elements = a11y_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(base_elements.len(), a11y_elements.len());
    for (base, a11y) in base_elements.iter().zip(a11y_elements.iter()) {
        assert_eq!(base["index"], a11y["index"]);
    }

    let launch_button = a11y_elements
        .iter()
        .find(|element| element["text"].as_str() == Some("Go"))
        .unwrap();
    assert_eq!(launch_button["ax_info"]["accessible_name"], "Launch Rocket");
    assert!(
        base_elements
            .iter()
            .find(|element| element["text"].as_str() == Some("Go"))
            .unwrap()["ax_info"]
            .is_null()
    );

    cleanup(&home);
}

/// T072c: `state --format a11y` publishes a token-friendly accessibility projection.
#[test]
#[ignore]
#[serial]
fn t233b_state_format_a11y_projects_token_friendly_summary() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/a11y-format",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>A11y Format Fixture</title></head>
<body>
  <button aria-label="Launch Rocket">Go</button>
  <a href="/terms" aria-disabled="true" aria-description="Opens legal page">Terms</a>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/a11y-format")])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["state", "--format", "a11y"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    let snapshot = &json["data"]["result"]["snapshot"];

    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["format"], "a11y");
    assert!(snapshot["snapshot_id"].as_str().is_some());
    assert!(snapshot["elements"].is_null());

    let a11y_text = snapshot["a11y_text"].as_str().unwrap();
    assert!(a11y_text.contains("[0] button \"Launch Rocket\""));
    assert!(a11y_text.contains("[1] link \"Terms\""));
    assert_eq!(snapshot["entry_count"], 2);
    assert_eq!(snapshot["total_count"], 2);

    cleanup(&home);
}

/// T072d: `state --diff` and `--format a11y` are mutually exclusive projections.
#[test]
#[ignore]
#[serial]
fn t233c_state_diff_rejects_a11y_format_projection() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/a11y-format-diff",
        "text/html",
        r#"<!DOCTYPE html><html><head><title>Format Diff Fixture</title></head><body><button>Go</button></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/a11y-format-diff")])
        .output()
        .unwrap();
    let state = run_state(&home);
    let snapshot = snapshot_id(&state);

    let out = rub_cmd(&home)
        .args(["state", "--diff", &snapshot, "--format", "a11y"])
        .output()
        .unwrap();
    let json = parse_json(&out);

    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("state --diff cannot be combined with --format")
    );

    cleanup(&home);
}

/// T072e: `observe` returns a shared snapshot summary plus highlighted screenshot evidence.
#[test]
#[ignore]
#[serial]
fn t233d_observe_returns_summary_and_highlighted_screenshot() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/observe",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Observe Fixture</title></head>
<body>
  <button aria-label="Launch Rocket">Go</button>
  <a href="/terms">Terms</a>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/observe")])
        .output()
        .unwrap();

    let screenshot_path = format!("{home}/observe.png");
    let out = rub_cmd(&home)
        .args(["observe", "--path", &screenshot_path])
        .output()
        .unwrap();
    let json = parse_json(&out);

    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["subject"]["kind"], "page_observation");
    assert_eq!(json["data"]["subject"]["format"], "a11y");
    assert!(
        json["data"]["result"]["snapshot"]["snapshot_id"]
            .as_str()
            .is_some()
    );
    assert!(
        json["data"]["result"]["snapshot"]["title"]
            .as_str()
            .is_some()
    );
    assert_eq!(json["data"]["artifact"]["output_path"], screenshot_path);
    assert!(std::fs::metadata(&screenshot_path).is_ok());
    assert_eq!(json["data"]["result"]["highlight"]["cleanup"], true);
    assert!(
        json["data"]["result"]["highlight"]["highlighted_count"].is_number(),
        "{json}"
    );

    let summary = json["data"]["result"]["snapshot"]["summary"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        json["data"]["result"]["snapshot"]["summary"]["format"],
        "a11y"
    );
    assert_eq!(json["data"]["result"]["snapshot"]["a11y_text"], summary);
    assert!(!summary.trim().is_empty(), "{json}");
    assert!(
        json["data"]["result"]["snapshot"]["a11y_lines"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "{json}"
    );
    assert!(
        json["data"]["result"]["snapshot"]["summary"]["line_count"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "{json}"
    );

    let element_map = json["data"]["result"]["snapshot"]["element_map"]
        .as_array()
        .unwrap();
    assert!(
        element_map.iter().all(|entry| {
            entry["index"].is_number()
                && entry["depth"].is_number()
                && entry["role"].is_string()
                && entry["bbox"]["width"].is_number()
        }),
        "{json}"
    );

    cleanup(&home);
}

/// T072f: `history` exposes recent session-scoped command summaries.
#[test]
#[ignore]
#[serial]
fn t233e_history_returns_recent_commands() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/history",
        "text/html",
        r#"<!DOCTYPE html><html><head><title>History Fixture</title></head><body>ok</body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/history")])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "document.title"])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["history", "--last", "2"])
        .output()
        .unwrap();
    let json = parse_json(&out);

    assert_eq!(json["success"], true);
    let entries = json["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["command"], "open");
    assert_eq!(entries[0]["success"], true);
    assert_eq!(entries[1]["command"], "exec");
    assert_eq!(entries[1]["summary"], "success");
    assert_eq!(entries.len(), 2);

    cleanup(&home);
}

/// T233f: `history --export-pipe` should project recent workflow-safe commands from workflow capture.
#[test]
#[ignore]
#[serial]
fn t233f_history_export_pipe_projects_recent_workflow_steps() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/history-export",
        "text/html",
        r#"<!DOCTYPE html><html><body><input id="name" value=""><button id="apply">Apply</button><div id="status">idle</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'idle';});</script></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/history-export")])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["type", "--selector", "#name", "Ada"])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["click", "--selector", "#apply"])
        .output()
        .unwrap();
    rub_cmd(&home).args(["observe"]).output().unwrap();

    let exported = parse_json(
        &rub_cmd(&home)
            .args(["history", "--last", "10", "--export-pipe"])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(exported["data"]["format"], "pipe", "{exported}");
    assert_eq!(
        exported["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        3,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][0]["command"], "open",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][1]["command"], "type",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["steps"][2]["command"], "click",
        "{exported}"
    );
    assert_eq!(exported["data"]["skipped"]["observation"], 1, "{exported}");
    assert_eq!(
        exported["data"]["steps"][1]["args"]["text"], "Ada",
        "{exported}"
    );

    let spec = serde_json::to_string(exported["data"]["steps"].as_array().unwrap()).unwrap();
    let replay_home = unique_home();
    cleanup(&replay_home);
    let (_rt2, server2) = start_test_server(vec![(
        "/history-export",
        "text/html",
        r#"<!DOCTYPE html><html><body><input id="name" value=""><button id="apply">Apply</button><div id="status">idle</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'idle';});</script></body></html>"#,
    )]);
    let replayed = parse_json(
        &rub_cmd(&replay_home)
            .args([
                "pipe",
                &spec.replace(
                    &server.url_for("/history-export"),
                    &server2.url_for("/history-export"),
                ),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(replayed["success"], true, "{replayed}");

    cleanup(&replay_home);
    cleanup(&home);
}

/// T233g: `history --export-pipe --include-observation` should include workflow-safe observation commands.
#[test]
#[ignore]
#[serial]
fn t233g_history_export_pipe_can_include_observation_steps() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/history-observe",
        "text/html",
        r#"<!DOCTYPE html><html><body><div id="status">ok</div></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/history-observe")])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["get", "text", "--selector", "#status"])
        .output()
        .unwrap();

    let exported = parse_json(
        &rub_cmd(&home)
            .args([
                "history",
                "--last",
                "10",
                "--export-pipe",
                "--include-observation",
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
    assert_eq!(exported["data"]["steps"][1]["command"], "get", "{exported}");
    assert_eq!(exported["data"]["skipped"]["observation"], 0, "{exported}");

    cleanup(&home);
}

/// T233h: `history --export-script` should wrap recent workflow steps in a replayable shell script.
#[test]
#[ignore]
#[serial]
fn t233h_history_export_script_replays_recent_workflow_steps() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/history-export-script",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>History Export Script Fixture</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'idle';
    });
  </script>
</body>
</html>"#,
    )]);

    let open = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/history-export-script")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let input = parse_json(
        &rub_cmd(&home)
            .args(["type", "--selector", "#name", "--clear", "Grace Hopper"])
            .output()
            .unwrap(),
    );
    assert_eq!(input["success"], true, "{input}");

    let click = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#apply"])
            .output()
            .unwrap(),
    );
    assert_eq!(click["success"], true, "{click}");

    let exported = parse_json(
        &rub_cmd(&home)
            .args(["history", "--last", "5", "--export-script"])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(exported["data"]["format"], "script", "{exported}");
    let script = exported["data"]["script"].as_str().unwrap();

    let replay_home = unique_home();
    cleanup(&replay_home);
    let script_path = std::env::temp_dir().join(format!(
        "rub-history-export-script-{}.sh",
        uuid::Uuid::now_v7()
    ));
    std::fs::write(&script_path, script).unwrap();

    let replay = Command::new("bash")
        .arg(&script_path)
        .env("RUB", rub_binary())
        .env("RUB_HOME", &replay_home)
        .env("RUB_SESSION", "replay")
        .output()
        .unwrap();
    let replayed = parse_json(&replay);
    assert_eq!(replayed["success"], true, "{replayed}");
    assert_eq!(
        replayed["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        3,
        "{replayed}"
    );

    let actual = parse_json(
        &rub_cmd_env(&replay_home, &[("RUB_SESSION", "replay")])
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(actual["success"], true, "{actual}");
    assert_eq!(actual["data"]["result"], "Grace Hopper", "{actual}");

    let _ = std::fs::remove_file(script_path);
    cleanup(&replay_home);
    cleanup(&home);
}

/// T233i: `history --export-pipe --from/--to --save-as` should persist a replayable named workflow asset,
/// and `pipe --list-workflows` / `pipe --workflow` should reuse it through the canonical pipe surface.
#[test]
#[ignore]
#[serial]
fn t233i_history_export_range_save_as_and_named_workflow_replay() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/workflow-save",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Workflow Save Fixture</title></head>
<body>
  <input id="name" value="" />
  <button id="apply">Apply</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value || 'idle';
    });
  </script>
</body>
</html>"#,
    )]);

    let open = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url_for("/workflow-save")])
            .output()
            .unwrap(),
    );
    assert_eq!(open["success"], true, "{open}");

    let typed = parse_json(
        &rub_cmd(&home)
            .args(["type", "--selector", "#name", "--clear", "Ada"])
            .output()
            .unwrap(),
    );
    assert_eq!(typed["success"], true, "{typed}");

    let click = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#apply"])
            .output()
            .unwrap(),
    );
    assert_eq!(click["success"], true, "{click}");

    let exec = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec["success"], true, "{exec}");
    assert_eq!(exec["data"]["result"], "Ada", "{exec}");

    let exported = parse_json(
        &rub_cmd(&home)
            .args([
                "history",
                "--export-pipe",
                "--from",
                "1",
                "--to",
                "4",
                "--save-as",
                "login_flow",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(
        exported["data"]["result"]["selection"]["from"], 1,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["selection"]["to"], 4,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["entries"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        4,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["persisted_artifacts"][0]["workflow_name"], "login_flow",
        "{exported}"
    );

    let saved_path = PathBuf::from(
        exported["data"]["result"]["persisted_artifacts"][0]["path"]
            .as_str()
            .unwrap(),
    );
    assert!(saved_path.exists(), "{exported}");
    let saved_value: Value = serde_json::from_str(&std::fs::read_to_string(&saved_path).unwrap())
        .expect("saved workflow json");
    assert!(saved_value.is_array(), "{saved_value}");
    assert_eq!(saved_value.as_array().unwrap().len(), 4, "{saved_value}");
    assert_eq!(saved_value[0]["command"], "open", "{saved_value}");
    assert_eq!(saved_value[3]["command"], "exec", "{saved_value}");

    let listed = parse_json(
        &rub_cmd(&home)
            .args(["pipe", "--list-workflows"])
            .output()
            .unwrap(),
    );
    assert_eq!(listed["success"], true, "{listed}");
    assert_eq!(
        listed["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1,
        "{listed}"
    );
    assert_eq!(
        listed["data"]["result"]["items"][0]["name"], "login_flow",
        "{listed}"
    );

    let replayed = parse_json(
        &rub_cmd(&home)
            .args(["pipe", "--workflow", "login_flow"])
            .output()
            .unwrap(),
    );
    assert_eq!(replayed["success"], true, "{replayed}");
    assert_eq!(
        replayed["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        4,
        "{replayed}"
    );
    assert_eq!(
        replayed["data"]["steps"][3]["result"]["result"], "Ada",
        "{replayed}"
    );

    cleanup(&home);
}

/// T056c: `cookies clear --url` removes only cookies for the specified URL.
#[test]
#[ignore]
#[serial]
fn t252_cookies_clear_url_scoped() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><head><title>Cookie Fixture</title></head><body>ok</body></html>"#,
    )]);
    let ip_url = server.url();
    let localhost_url = ip_url.replace("127.0.0.1", "localhost");

    rub_cmd(&home).args(["open", &ip_url]).output().unwrap();
    rub_cmd(&home)
        .args(["cookies", "set", "ip_cookie", "1"])
        .output()
        .unwrap();

    rub_cmd(&home)
        .args(["open", &localhost_url])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["cookies", "set", "host_cookie", "1"])
        .output()
        .unwrap();

    let cleared = parse_json(
        &rub_cmd(&home)
            .args(["cookies", "clear", "--url", &ip_url])
            .output()
            .unwrap(),
    );
    assert_eq!(cleared["success"], true);
    assert_eq!(cleared["data"]["subject"]["url"], ip_url);

    rub_cmd(&home)
        .args(["open", &localhost_url])
        .output()
        .unwrap();
    let host_cookies = parse_json(&rub_cmd(&home).args(["cookies", "get"]).output().unwrap());
    let host_names = host_cookies["data"]["result"]["cookies"]
        .as_array()
        .unwrap();
    assert!(
        host_names
            .iter()
            .any(|cookie| cookie["name"] == "host_cookie")
    );

    rub_cmd(&home).args(["open", &ip_url]).output().unwrap();
    let ip_cookies = parse_json(&rub_cmd(&home).args(["cookies", "get"]).output().unwrap());
    let ip_names = ip_cookies["data"]["result"]["cookies"].as_array().unwrap();
    assert!(!ip_names.iter().any(|cookie| cookie["name"] == "ip_cookie"));

    cleanup(&home);
}

/// T065a: selecting an option returns the selected value/text and changes the DOM.
#[test]
#[ignore]
#[serial]
fn t260_select_round_trip() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/select",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Select Fixture</title></head>
<body>
  <select name="region">
    <option value="">Choose</option>
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/select")])
        .output()
        .unwrap();

    let state_json = run_state(&home);
    let snap_id = snapshot_id(&state_json);
    let select_index = find_element_index(&state_json, |element| element["tag"] == "select");

    let out = rub_cmd(&home)
        .args([
            "select",
            &select_index.to_string(),
            "California",
            "--snapshot",
            &snap_id,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["interaction"]["semantic_class"],
        "select_choice"
    );
    assert_eq!(json["data"]["result"]["value"], "CA");
    assert_eq!(json["data"]["result"]["text"], "California");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "selection_applied"
    );

    let out = rub_cmd(&home)
        .args(["exec", "document.querySelector('select').value"])
        .output()
        .unwrap();
    let exec_json = parse_json(&out);
    assert_eq!(exec_json["success"], true);
    assert_eq!(exec_json["data"]["result"], "CA");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t260c_select_accepts_explicit_value_flag_with_locator() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/select-explicit-value",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Select Explicit Value Fixture</title></head>
<body>
  <select name="region">
    <option value="">Choose</option>
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/select-explicit-value")])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args([
            "select",
            "--selector",
            "select[name=region]",
            "--value",
            "California",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["value"], "CA", "{json}");
    assert_eq!(json["data"]["result"]["text"], "California", "{json}");

    let exec_json = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.querySelector('select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_json["success"], true, "{exec_json}");
    assert_eq!(exec_json["data"]["result"], "CA", "{exec_json}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t260b_select_reports_contradicted_effect() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/select-contradicted",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Select Contradicted Fixture</title></head>
<body>
  <select name="region">
    <option value="">Choose</option>
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
  <script>
    const select = document.querySelector('select[name=region]');
    select.addEventListener('change', () => {
      select.value = 'NY';
    });
  </script>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/select-contradicted")])
        .output()
        .unwrap();

    let state_json = run_state(&home);
    let snap_id = snapshot_id(&state_json);
    let select_index = find_element_index(&state_json, |element| element["tag"] == "select");

    let out = rub_cmd(&home)
        .args([
            "select",
            &select_index.to_string(),
            "California",
            "--snapshot",
            &snap_id,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["interaction"]["semantic_class"],
        "select_choice"
    );
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], false);
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "contradicted"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "selection_applied"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_details"]["observed"]["value"],
        "NY"
    );

    let out = rub_cmd(&home)
        .args(["exec", "document.querySelector('select').value"])
        .output()
        .unwrap();
    let exec_json = parse_json(&out);
    assert_eq!(exec_json["success"], true);
    assert_eq!(exec_json["data"]["result"], "NY");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t260c_select_reports_degraded_when_context_replaced() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/select-degraded",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Select Degraded Fixture</title></head>
<body>
  <select name="region">
    <option value="">Choose</option>
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
  <script>
    const select = document.querySelector('select[name=region]');
    select.addEventListener('change', () => {
      location.replace('/after-select');
    });
  </script>
</body>
</html>"#,
        ),
        (
            "/after-select",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>After Select</title></head><body>after</body></html>"#,
        ),
    ]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/select-degraded")])
        .output()
        .unwrap();

    let state_json = run_state(&home);
    let snap_id = snapshot_id(&state_json);
    let select_index = find_element_index(&state_json, |element| element["tag"] == "select");

    let out = rub_cmd(&home)
        .args([
            "select",
            &select_index.to_string(),
            "California",
            "--snapshot",
            &snap_id,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["interaction"]["semantic_class"],
        "select_choice"
    );
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], false);
    assert_eq!(
        json["data"]["interaction"]["confirmation_status"],
        "degraded"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "selection_applied"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_details"]["context_changed"],
        true
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_details"]["after_page"]["context_replaced"],
        false
    );
    assert_eq!(
        json["data"]["interaction"]["context_turnover"]["context_changed"],
        true
    );
    assert_eq!(
        json["data"]["interaction"]["context_turnover"]["context_replaced"],
        false
    );
    assert_eq!(
        json["data"]["interaction"]["observed_effects"]["context_turnover"]["context_replaced"],
        false
    );
    assert_eq!(json["data"]["interaction_trace"]["command"], "select");
    assert_eq!(
        json["data"]["interaction_trace"]["confirmation_status"],
        "degraded"
    );
    assert_eq!(
        json["data"]["interaction_trace"]["observed_effects"]["context_changed"],
        true
    );
    assert_eq!(
        json["data"]["interaction_trace"]["observed_effects"]["context_turnover"]["context_changed"],
        true
    );

    let out = rub_cmd(&home).args(["get", "title"]).output().unwrap();
    let title_json = parse_json(&out);
    assert_eq!(title_json["success"], true);
    assert_eq!(title_json["data"]["result"]["value"], "After Select");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t260d_select_disabled_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/select-disabled",
        "text/html",
        r#"<!doctype html>
<html><body>
  <select name="region" disabled>
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
</body></html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/select-disabled")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let select_index = find_element_index(&state_json, |element| element["tag"] == "select");

    let out = rub_cmd(&home)
        .args([
            "select",
            &select_index.to_string(),
            "CA",
            "--snapshot",
            snapshot,
        ])
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
fn t260e_select_aria_disabled_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/select-aria-disabled",
        "text/html",
        r#"<!doctype html>
<html><body>
  <select name="region" aria-disabled="true">
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
</body></html>"#,
    )]);

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/select-aria-disabled")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let select_index = find_element_index(&state_json, |element| element["tag"] == "select");

    let out = rub_cmd(&home)
        .args([
            "select",
            &select_index.to_string(),
            "CA",
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
            .contains("aria-disabled")
    );

    cleanup(&home);
}

/// T061a: uploading a file attaches it to a file input element.
#[test]
#[ignore]
#[serial]
fn t261_upload_round_trip() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/upload",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Upload Fixture</title></head>
<body>
  <input type="file" name="resume" />
  <div id="filename"></div>
  <script>
    document.querySelector('input[type=file]').addEventListener('change', (event) => {
      const file = event.target.files[0];
      document.getElementById('filename').textContent = file ? file.name : '';
    });
  </script>
</body>
</html>"#,
    )]);

    let file_path = format!("/tmp/rub-upload-{}.txt", std::process::id());
    std::fs::write(&file_path, b"resume").unwrap();

    rub_cmd(&home)
        .args(["open", &server.url_for("/upload")])
        .output()
        .unwrap();

    let state_json = run_state(&home);
    let snap_id = snapshot_id(&state_json);
    let upload_index = find_element_index(&state_json, |element| {
        element["attributes"]["type"].as_str() == Some("file")
    });

    let out = rub_cmd(&home)
        .args([
            "upload",
            &upload_index.to_string(),
            &file_path,
            "--snapshot",
            &snap_id,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["interaction"]["semantic_class"], "set_value");
    assert_eq!(json["data"]["interaction"]["interaction_confirmed"], true);
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"],
        "files_attached"
    );
    let uploaded_path = json["data"]["result"]["path"].as_str().unwrap();
    assert!(uploaded_path.ends_with(&format!("rub-upload-{}.txt", std::process::id())));

    let out = rub_cmd(&home)
        .args(["exec", "document.getElementById('filename').textContent"])
        .output()
        .unwrap();
    let exec_json = parse_json(&out);
    assert_eq!(exec_json["success"], true);
    assert_eq!(
        exec_json["data"]["result"],
        format!("rub-upload-{}.txt", std::process::id())
    );

    let _ = std::fs::remove_file(&file_path);
    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t261b_upload_disabled_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/upload-disabled",
        "text/html",
        r#"<!doctype html>
<html><body>
  <input type="file" name="resume" disabled />
</body></html>"#,
    )]);

    let file_path = format!("/tmp/rub-upload-disabled-{}.txt", std::process::id());
    std::fs::write(&file_path, "disabled").unwrap();

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/upload-disabled")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let upload_index = find_element_index(&state_json, |element| {
        element["attributes"]["type"] == "file"
    });

    let out = rub_cmd(&home)
        .args([
            "upload",
            &upload_index.to_string(),
            &file_path,
            "--snapshot",
            snapshot,
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE");

    let _ = std::fs::remove_file(&file_path);
    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t261c_upload_fieldset_disabled_reports_not_interactable() {
    let home = unique_home();
    let (_runtime, server) = start_test_server(vec![(
        "/upload-fieldset-disabled",
        "text/html",
        r#"<!doctype html>
<html><body>
  <fieldset disabled>
    <input type="file" name="resume" />
  </fieldset>
</body></html>"#,
    )]);

    let file_path = format!(
        "/tmp/rub-upload-fieldset-disabled-{}.txt",
        std::process::id()
    );
    std::fs::write(&file_path, "disabled").unwrap();

    let open = rub_cmd(&home)
        .args(["open", &server.url_for("/upload-fieldset-disabled")])
        .output()
        .unwrap();
    assert_eq!(parse_json(&open)["success"], true);

    let state = rub_cmd(&home).arg("state").output().unwrap();
    let state_json = parse_json(&state);
    let snapshot = state_json["data"]["result"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let upload_index = find_element_index(&state_json, |element| {
        element["attributes"]["type"] == "file"
    });

    let out = rub_cmd(&home)
        .args([
            "upload",
            &upload_index.to_string(),
            &file_path,
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

    let _ = std::fs::remove_file(&file_path);
    cleanup(&home);
}

/// T270: `window.*` persists across exec calls on the same page.
#[test]
#[ignore]
#[serial]
fn t270_persistent_js_context() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><div>Persistent JS Fixture</div></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["exec", "window.__rub_state = 42"])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home)
        .args(["exec", "window.__rub_state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["result"], 42);

    cleanup(&home);
}

/// T271: navigation clears `window.*` state because Chrome creates a new page context.
#[test]
#[ignore]
#[serial]
fn t271_persistent_js_cleared_on_nav() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/first",
            "text/html",
            r#"<!DOCTYPE html><html><body><div>First Page</div></body></html>"#,
        ),
        (
            "/second",
            "text/html",
            r#"<!DOCTYPE html><html><body><div>Second Page</div></body></html>"#,
        ),
    ]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/first")])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["exec", "window.__rub_state = 42"])
        .output()
        .unwrap();

    rub_cmd(&home)
        .args(["open", &server.url_for("/second")])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["exec", "window.__rub_state ?? null"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(json["data"]["result"].is_null());

    cleanup(&home);
}

/// T280: `state --viewport` only returns visible elements and projects viewport metadata.
#[test]
#[ignore]
#[serial]
fn t280_viewport_filter() {
    let home = unique_home();
    cleanup(&home);

    let fixture = (0..30)
        .map(|idx| {
            format!(
                "<div style=\"height: 180px\"></div><button type=\"button\">Viewport Button {idx}</button>"
            )
        })
        .collect::<String>();
    let html: &'static str = Box::leak(
        format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Viewport Fixture</title></head>
<body style="margin: 0">{fixture}</body>
</html>"#
        )
        .into_boxed_str(),
    );
    let (_rt, server) = start_test_server(vec![("/viewport", "text/html", html)]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/viewport")])
        .output()
        .unwrap();

    let full_state = run_state(&home);
    let viewport_state = parse_json(
        &rub_cmd(&home)
            .args(["state", "--viewport"])
            .output()
            .unwrap(),
    );

    assert_eq!(viewport_state["success"], true);
    assert_eq!(
        viewport_state["data"]["result"]["snapshot"]["viewport_filtered"],
        true
    );
    let full_len = full_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap()
        .len();
    let viewport_len = viewport_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap()
        .len();
    assert!(
        viewport_len < full_len,
        "viewport filter should reduce element count: full={full_len}, viewport={viewport_len}"
    );
    assert_eq!(
        viewport_state["data"]["result"]["snapshot"]["viewport_count"]
            .as_u64()
            .unwrap() as usize,
        viewport_len
    );
    assert_eq!(
        viewport_state["data"]["result"]["snapshot"]["total_count"],
        full_state["data"]["result"]["snapshot"]["total_count"]
    );

    cleanup(&home);
}

/// T281: viewport filtering preserves global indices from the full snapshot.
#[test]
#[ignore]
#[serial]
fn t281_viewport_preserves_index() {
    let home = unique_home();
    cleanup(&home);

    let fixture = (0..24)
        .map(|idx| {
            format!(
                "<div style=\"height: 220px\"></div><button type=\"button\">Preserve Index {idx}</button>"
            )
        })
        .collect::<String>();
    let html: &'static str = Box::leak(
        format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Viewport Index Fixture</title></head>
<body style="margin: 0">{fixture}</body>
</html>"#
        )
        .into_boxed_str(),
    );
    let (_rt, server) = start_test_server(vec![("/viewport-index", "text/html", html)]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/viewport-index")])
        .output()
        .unwrap();
    let full_state = run_state(&home);
    rub_cmd(&home)
        .args(["scroll", "down", "--amount", "1500"])
        .output()
        .unwrap();

    let viewport_state = parse_json(
        &rub_cmd(&home)
            .args(["state", "--viewport"])
            .output()
            .unwrap(),
    );
    assert_eq!(viewport_state["success"], true);
    let full_elements = full_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    let viewport_elements = viewport_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert!(
        viewport_elements
            .iter()
            .any(|element| element["index"].as_u64().unwrap_or(0) > 0),
        "after scrolling, viewport indices should reflect their original global positions"
    );
    for element in viewport_elements {
        let text = element["text"].as_str().unwrap();
        let full = full_elements
            .iter()
            .find(|candidate| candidate["text"].as_str() == Some(text))
            .expect("viewport element should exist in full snapshot");
        assert_eq!(element["index"], full["index"]);
    }

    cleanup(&home);
}

/// T290: `state --diff` reports newly-added interactive elements.
#[test]
#[ignore]
#[serial]
fn t290_state_diff_added() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/diff",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Diff Fixture</title></head>
<body>
  <button id="alpha">Alpha</button>
  <button id="beta">Beta</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/diff")])
        .output()
        .unwrap();
    let base = run_state(&home);
    let snap = snapshot_id(&base);

    rub_cmd(&home)
        .args([
            "exec",
            "document.body.insertAdjacentHTML('beforeend', '<button id=\"gamma\">Gamma</button>')",
        ])
        .output()
        .unwrap();

    let diff = parse_json(
        &rub_cmd(&home)
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true);
    assert_eq!(diff["data"]["result"]["diff"]["has_changes"], true);
    assert!(
        diff["data"]["result"]["diff"]["added"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["text"] == "Gamma")
    );

    cleanup(&home);
}

/// T291: `state --diff` reports removed interactive elements.
#[test]
#[ignore]
#[serial]
fn t291_state_diff_removed() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/diff-remove",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Diff Remove Fixture</title></head>
<body>
  <button id="alpha">Alpha</button>
  <button id="beta">Beta</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/diff-remove")])
        .output()
        .unwrap();
    let base = run_state(&home);
    let snap = snapshot_id(&base);

    rub_cmd(&home)
        .args(["exec", "document.getElementById('beta').remove()"])
        .output()
        .unwrap();

    let diff = parse_json(
        &rub_cmd(&home)
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true);
    assert!(
        diff["data"]["result"]["diff"]["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["text"] == "Beta")
    );

    cleanup(&home);
}

/// T292: `state --diff` reports field-level text changes.
#[test]
#[ignore]
#[serial]
fn t292_state_diff_changed() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/diff-change",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Diff Change Fixture</title></head>
<body>
  <button id="alpha">Alpha</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url_for("/diff-change")])
        .output()
        .unwrap();
    let base = run_state(&home);
    let snap = snapshot_id(&base);

    rub_cmd(&home)
        .args([
            "exec",
            "document.getElementById('alpha').textContent = 'Alpha Updated'",
        ])
        .output()
        .unwrap();

    let diff = parse_json(
        &rub_cmd(&home)
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true);
    let changed = diff["data"]["result"]["diff"]["changed"]
        .as_array()
        .unwrap();
    let text_change = changed
        .iter()
        .flat_map(|element| element["changes"].as_array().unwrap().iter())
        .find(|change| change["field"] == "text")
        .expect("text field change should be present");
    assert_eq!(text_change["from"], "Alpha");
    assert_eq!(text_change["to"], "Alpha Updated");

    cleanup(&home);
}

/// T293: invalid diff baseline returns STALE_SNAPSHOT.
#[test]
#[ignore]
#[serial]
fn t293_state_diff_stale() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><div>Diff Fixture</div></body></html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["state", "--diff", "snapshot-does-not-exist"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "STALE_SNAPSHOT");

    cleanup(&home);
}

/// T310: attach to an external Chrome via HTTP CDP endpoint and inspect state.
#[test]
#[ignore]
#[serial]
fn t310_cdp_url_connect() {
    let (_rt, server) = start_test_server(vec![(
        "/external",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>External CDP Fixture</title></head>
<body><button>External Ready</button></body>
</html>"#,
    )]);
    let Some((mut chrome, cdp_origin, profile_dir)) =
        spawn_external_chrome(Some(&server.url_for("/external")))
    else {
        eprintln!("Skipping external CDP test because no Chrome/Chromium binary was found");
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
    assert_eq!(state["success"], true);
    assert_eq!(
        state["data"]["result"]["snapshot"]["title"],
        "External CDP Fixture"
    );

    let _ = rub_cmd(&home).arg("close").output();
    terminate_external_chrome(&mut chrome);
    let _ = std::fs::remove_dir_all(profile_dir);
    cleanup(&home);
}

/// T310b: failed external attach must not leave a startup daemon residue behind.
#[test]
#[ignore]
#[serial]
fn t310b_failed_external_attach_does_not_leave_daemon_residue() {
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args([
            "--timeout",
            "7000",
            "--cdp-url",
            "http://127.0.0.1:1",
            "state",
        ])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "DAEMON_START_FAILED");

    let residues = daemon_processes_for_home(&home);
    assert!(
        residues.is_empty(),
        "startup failure must not leave daemon residue for home {home}: {residues:#?}"
    );

    cleanup(&home);
}

/// T311: closing an attached external session must not kill the browser process.
#[test]
#[ignore]
#[serial]
fn t311_cdp_url_close_preserves_browser() {
    let (_rt, server) = start_standard_site_fixture();
    let Some((mut chrome, cdp_origin, profile_dir)) = spawn_external_chrome(Some(&server.url()))
    else {
        eprintln!("Skipping external CDP close test because no Chrome/Chromium binary was found");
        return;
    };
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args(["--cdp-url", &cdp_origin, "state"])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home).arg("close").output().unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    let report = doctor_result(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        report["launch_policy"]["connection_target"]["source"],
        "cdp_url"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["url"],
        cdp_origin
    );

    assert!(
        chrome.try_wait().unwrap().is_none(),
        "external browser should still be alive after rub close"
    );
    let addr = cdp_origin.trim_start_matches("http://");
    assert!(
        TcpStream::connect(addr).is_ok(),
        "external CDP port should still accept connections after rub close"
    );

    terminate_external_chrome(&mut chrome);
    let _ = std::fs::remove_dir_all(profile_dir);
    cleanup(&home);
}

/// T320: `--profile` resolves a named Chrome profile and projects it in doctor output.
#[test]
#[ignore]
#[serial]
fn t320_profile_resolve() {
    let home = unique_home();
    cleanup(&home);
    let (fake_home, resolved_profile, envs_owned) = prepare_fake_profile_env();
    let (_rt, server) = start_standard_site_fixture();
    let envs = envs_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();

    let out = rub_cmd_env(&home, &envs)
        .args(["--profile", "Default", "open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    let out = rub_cmd_env(&home, &envs).arg("doctor").output().unwrap();
    let json = parse_json(&out);
    let report = doctor_result(&json);
    assert_eq!(json["success"], true);
    assert_eq!(
        report["launch_policy"]["connection_target"]["source"],
        "profile"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["name"],
        "Default"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["resolved_path"],
        resolved_profile.display().to_string()
    );

    cleanup(&home);
    let _ = std::fs::remove_dir_all(fake_home);
}

/// T330: `close --all` gracefully closes every registered session.
#[test]
#[ignore]
#[serial]
fn t330_close_all() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["--session", "work", "open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home).args(["close", "--all"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["result"]["closed"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true);
    assert_eq!(
        sessions["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    cleanup(&home);
}

/// T330b: `close --all` must terminate the managed Chrome process tree.
#[test]
#[ignore]
#[serial]
fn t330b_close_all_terminates_managed_browser_processes() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><div>Managed Close Fixture</div></body></html>"#,
    )]);

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let daemon_pid: u32 = std::fs::read_to_string(default_session_pid_path(&home))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        !browser_processes_for_daemon_pid(daemon_pid).is_empty(),
        "managed browser processes should exist before close --all"
    );

    let out = rub_cmd(&home).args(["close", "--all"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    wait_until(Duration::from_secs(5), || {
        browser_processes_for_daemon_pid(daemon_pid).is_empty()
    });

    cleanup(&home);
}

/// T331: `close --all` reports stale-session cleanup in `cleaned_stale`.
#[test]
#[ignore]
#[serial]
fn t331_close_all_counts_stale_cleanup() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["--session", "work", "open", &server.url()])
        .output()
        .unwrap();

    let work_pid: i32 = std::fs::read_to_string(format!("{home}/work.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(work_pid, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(300));

    let out = rub_cmd(&home).args(["close", "--all"]).output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["result"]["cleaned_stale"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1
    );
    assert!(
        json["data"]["result"]["cleaned_stale"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "work")
    );

    cleanup(&home);
}

/// T332: `cleanup` removes stale current-home sessions without requiring manual rm/kill.
#[test]
#[ignore]
#[serial]
fn t332_cleanup_removes_stale_session() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let pid: i32 = std::fs::read_to_string(default_session_pid_path(&home))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(500));

    let out = rub_cmd(&home).arg("cleanup").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["cleaned_stale_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "default")
    );

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true);
    assert_eq!(
        sessions["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    cleanup(&home);
}

/// T333: `cleanup` preserves healthy active sessions while only sweeping stale/orphan state.
#[test]
#[ignore]
#[serial]
fn t333_cleanup_keeps_active_session() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);

    let out = rub_cmd(&home).arg("cleanup").output().unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["kept_active_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "default")
    );

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    assert_eq!(doctor["success"], true);

    cleanup(&home);
}

/// T340: `cookies get --url` only returns cookies that match the requested URL.
#[test]
#[ignore]
#[serial]
fn t340_cookies_get_url() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><head><title>Cookie Filter Fixture</title></head><body>ok</body></html>"#,
    )]);
    let ip_url = server.url();
    let localhost_url = ip_url.replace("127.0.0.1", "localhost");

    rub_cmd(&home).args(["open", &ip_url]).output().unwrap();
    rub_cmd(&home)
        .args(["cookies", "set", "ip_cookie", "1"])
        .output()
        .unwrap();

    rub_cmd(&home)
        .args(["open", &localhost_url])
        .output()
        .unwrap();
    rub_cmd(&home)
        .args(["cookies", "set", "host_cookie", "1"])
        .output()
        .unwrap();

    let ip_cookies = parse_json(
        &rub_cmd(&home)
            .args(["cookies", "get", "--url", &ip_url])
            .output()
            .unwrap(),
    );
    let ip_names = ip_cookies["data"]["result"]["cookies"].as_array().unwrap();
    assert!(ip_names.iter().any(|cookie| cookie["name"] == "ip_cookie"));
    assert!(
        !ip_names
            .iter()
            .any(|cookie| cookie["name"] == "host_cookie")
    );

    let host_cookies = parse_json(
        &rub_cmd(&home)
            .args(["cookies", "get", "--url", &localhost_url])
            .output()
            .unwrap(),
    );
    let host_names = host_cookies["data"]["result"]["cookies"]
        .as_array()
        .unwrap();
    assert!(
        host_names
            .iter()
            .any(|cookie| cookie["name"] == "host_cookie")
    );
    assert!(
        !host_names
            .iter()
            .any(|cookie| cookie["name"] == "ip_cookie")
    );

    cleanup(&home);
}

/// T350: `RUB_SESSION` sets the default session, but explicit `--session` overrides it.
#[test]
#[ignore]
#[serial]
fn t350_rub_session_env() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd_env(&home, &[("RUB_SESSION", "alt")])
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["session"], "alt");

    let out = rub_cmd_env(&home, &[("RUB_SESSION", "alt")])
        .args(["--session", "explicit", "open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["session"], "explicit");

    let sessions = parse_json(&rub_cmd(&home).arg("sessions").output().unwrap());
    let names = sessions["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|session| session["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"alt"));
    assert!(names.contains(&"explicit"));

    cleanup(&home);
}

/// T360: connection mode flags are mutually exclusive.
#[test]
#[ignore]
#[serial]
fn t360_mutual_exclusion() {
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args(["--cdp-url", "http://127.0.0.1:9222", "--connect", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "CONFLICTING_CONNECT_OPTIONS");

    cleanup(&home);
}

/// T300: screenshot --highlight should project at least one overlay from the snapshot.
#[test]
#[ignore]
#[serial]
fn t300_screenshot_highlight() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["screenshot", "--highlight"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["highlight"]["highlighted_count"]
            .as_u64()
            .unwrap()
            > 0,
        "highlighted_count should reflect overlays injected from the snapshot"
    );
    assert_eq!(json["data"]["result"]["highlight"]["cleanup"], true);

    cleanup(&home);
}

/// T301: screenshot --highlight should work on Trusted Types pages such as Chrome new tab.
#[test]
#[ignore]
#[serial]
fn t301_screenshot_highlight_trusted_types_page() {
    let home = unique_home();
    cleanup(&home);

    rub_cmd(&home)
        .args(["open", "chrome://newtab"])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["screenshot", "--highlight"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert!(
        json["data"]["result"]["highlight"]["highlighted_count"].is_number(),
        "highlighted_count should still be projected on Trusted Types pages"
    );
    assert_eq!(json["data"]["result"]["highlight"]["cleanup"], true);

    cleanup(&home);
}

/// T361: explicit connection override must not be silently ignored by an existing session.
#[test]
#[ignore]
#[serial]
fn t361_existing_session_rejects_connection_override() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["--cdp-url", "http://127.0.0.1:1", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");

    cleanup(&home);
}

/// T362: a fresh session with `--cdp-url` must fail on the requested connection,
/// not on an attempted launch-policy probe against a non-existent daemon.
#[test]
#[ignore]
#[serial]
fn t362_new_session_invalid_cdp_url_reports_connection_failure() {
    let home = unique_home();
    cleanup(&home);

    let out = rub_cmd(&home)
        .args(["--cdp-url", "http://127.0.0.1:1", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "CDP_CONNECTION_FAILED");

    cleanup(&home);
}

/// T363: explicit session policy overrides must not be silently ignored by an existing session.
#[test]
#[ignore]
#[serial]
fn t363_existing_session_rejects_session_policy_override() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(&home)
        .args(["--humanize", "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");

    let out = rub_cmd(&home)
        .args(["--no-stealth", "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");

    cleanup(&home);
}
