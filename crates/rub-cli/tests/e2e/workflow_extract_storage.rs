use super::*;

fn list_output_files(dir: &Path) -> Vec<String> {
    let mut files = std::fs::read_dir(dir)
        .expect("output dir should exist")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// T403b: recover must not fake handoff escalation when the session is not user-accessible.
#[test]
#[ignore]
#[serial]
fn t403b_interference_recover_fails_when_handoff_is_unavailable() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Verify you are human</title></head>
<body><h1>Human checkpoint</h1></body>
</html>"#,
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let recovered = parse_json(
        &rub_cmd(&home)
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        recovered["data"]["recovery"]["action"], "escalate_to_handoff",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["result"], "failed",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["reason"], "handoff_unavailable",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["human_verification_handoff"]["status"], "unavailable",
        "{recovered}"
    );

    cleanup(&home);
}

/// T404: interference mode should update the canonical session-scoped policy surface.
#[test]
#[ignore]
#[serial]
fn t404_interference_mode_updates_canonical_policies() {
    let home = unique_home();
    cleanup(&home);

    let stable = parse_json(
        &rub_cmd(&home)
            .args(["interference", "mode", "public_web_stable"])
            .output()
            .unwrap(),
    );
    assert_eq!(stable["success"], true, "{stable}");
    assert_eq!(
        stable["data"]["runtime"]["mode"], "public_web_stable",
        "{stable}"
    );
    assert_eq!(
        stable["data"]["runtime"]["active_policies"],
        json!(["safe_recovery", "handoff_escalation"]),
        "{stable}"
    );

    let strict = parse_json(
        &rub_cmd(&home)
            .args(["interference", "mode", "strict"])
            .output()
            .unwrap(),
    );
    assert_eq!(strict["success"], true, "{strict}");
    assert_eq!(strict["data"]["runtime"]["mode"], "strict", "{strict}");
    assert_eq!(
        strict["data"]["runtime"]["active_policies"],
        json!(["safe_recovery", "handoff_escalation", "strict_containment"]),
        "{strict}"
    );

    let normal = parse_json(
        &rub_cmd(&home)
            .args(["interference", "mode", "normal"])
            .output()
            .unwrap(),
    );
    assert_eq!(normal["success"], true, "{normal}");
    assert_eq!(normal["data"]["runtime"]["mode"], "normal", "{normal}");
    assert_eq!(
        normal["data"]["runtime"]["active_policies"],
        json!([]),
        "{normal}"
    );

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["mode"], "normal", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["active_policies"],
        json!([]),
        "{runtime}"
    );

    cleanup(&home);
}

/// T406: interaction traces should surface classified public-web interference caused by the command.
#[test]
#[ignore]
#[serial]
fn t406_interaction_trace_surfaces_interference_delta() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Primary Page</title></head>
<body>
  <button id="drift" onclick="location.href='/interstitial#vignette'">Drift</button>
</body>
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

    let state = run_state(&home);
    let snapshot = snapshot_id(&state);
    let button = find_element_index(&state, |element| element["text"].as_str() == Some("Drift"));

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", &button.to_string(), "--snapshot", &snapshot])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["observed_effects"]["interference"]["after"]["current_interference"]
            ["kind"],
        "interstitial_navigation",
        "{clicked}"
    );
    assert_eq!(
        clicked["data"]["interaction_trace"]["observed_effects"]["interference"],
        clicked["data"]["interaction"]["observed_effects"]["interference"],
        "{clicked}"
    );
    assert!(
        clicked["data"]["interaction"]["observed_effects"]["interference"]["changed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("interference_runtime.current_interference")),
        "{clicked}"
    );

    cleanup(&home);
}

/// T407: runtime interference should classify readiness overlay blockers as overlay interference.
#[test]
#[ignore]
#[serial]
fn t407_runtime_interference_classifies_overlay_interference() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Overlay Fixture</title></head>
<body>
  <vite-error-overlay></vite-error-overlay>
  <h1>Overlay active</h1>
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

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "active", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["current_interference"]["kind"], "overlay_interference",
        "{runtime}"
    );

    cleanup(&home);
}

/// T407b: interference recover should dismiss a generic user-blocking overlay and resume the page.
#[test]
#[ignore]
#[serial]
fn t407b_interference_recover_dismisses_user_blocking_overlay() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head>
  <title>User Blocking Overlay</title>
  <style>
    body.locked { overflow: hidden; }
    #scrim {
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.45);
      z-index: 100;
    }
    #modal {
      position: fixed;
      top: 18vh;
      left: 50%;
      transform: translateX(-50%);
      width: 22rem;
      padding: 1rem;
      background: white;
      border: 1px solid #ccc;
      z-index: 101;
    }
  </style>
</head>
<body class="locked">
  <h1 id="status">overlay-active</h1>
  <div id="scrim"></div>
  <div id="modal" role="dialog" aria-modal="true" aria-label="Login prompt">
    <p>Please sign in to continue</p>
    <button id="dismiss" aria-label="Close dialog" onclick="dismissOverlay()">Close</button>
    <button id="login">Sign in</button>
  </div>
  <script>
    function dismissOverlay() {
      document.getElementById('scrim').remove();
      document.getElementById('modal').remove();
      document.body.classList.remove('locked');
      document.getElementById('status').textContent = 'overlay-cleared';
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

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "interference"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "active", "{runtime}");
    assert_eq!(
        runtime["data"]["runtime"]["current_interference"]["kind"], "overlay_interference",
        "{runtime}"
    );

    let readiness = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "readiness"])
            .output()
            .unwrap(),
    );
    assert_eq!(readiness["success"], true, "{readiness}");
    assert_eq!(
        readiness["data"]["overlay_state"], "user_blocking",
        "{readiness}"
    );

    let recovered = parse_json(
        &rub_cmd(&home)
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        recovered["data"]["recovery"]["action"], "dismiss_overlay",
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

    let status = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(
        status["data"]["result"]["value"], "overlay-cleared",
        "{status}"
    );

    cleanup(&home);
}

/// T407c: interference recover should fail truthfully when a user-blocking overlay has no safe dismiss path.
#[test]
#[ignore]
#[serial]
fn t407c_interference_recover_reports_blocked_user_overlay_without_dismiss_path() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head>
  <title>Undismissable Overlay</title>
  <style>
    body.locked { overflow: hidden; }
    #scrim {
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.55);
      z-index: 100;
    }
    #modal {
      position: fixed;
      top: 18vh;
      left: 50%;
      transform: translateX(-50%);
      width: 22rem;
      padding: 1rem;
      background: white;
      border: 1px solid #ccc;
      z-index: 101;
    }
  </style>
</head>
<body class="locked">
  <h1 id="status">overlay-still-active</h1>
  <div id="scrim"></div>
  <div id="modal" role="dialog" aria-modal="true" aria-label="Hard gate">
    <p>Sign in to continue</p>
    <button id="login">Sign in</button>
  </div>
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

    let recovered = parse_json(
        &rub_cmd(&home)
            .args(["interference", "recover"])
            .output()
            .unwrap(),
    );
    assert_eq!(recovered["success"], true, "{recovered}");
    assert_eq!(
        recovered["data"]["recovery"]["action"], "dismiss_overlay",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["result"], "failed",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["recovery"]["reason"], "dismiss_overlay_candidate_not_found",
        "{recovered}"
    );
    assert_eq!(
        recovered["data"]["runtime"]["status"], "active",
        "{recovered}"
    );

    cleanup(&home);
}

/// T408: `open --wait-after-text` should treat post-load page effects as an explicit wait fence.
#[test]
#[ignore]
#[serial]
fn t408_open_wait_after_text_reports_wait_metadata() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Open Wait-After Fixture</title></head>
<body>
  <div id="status">loading</div>
  <script>
    setTimeout(() => {
      document.getElementById('status').textContent = 'ready';
    }, 150);
  </script>
</body>
</html>"#,
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args([
                "open",
                &server.url(),
                "--wait-after-text",
                "ready",
                "--wait-after-timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");
    assert_eq!(opened["data"]["wait_after"]["matched"], true, "{opened}");
    assert_eq!(opened["data"]["wait_after"]["kind"], "text", "{opened}");
    assert_eq!(opened["data"]["wait_after"]["value"], "ready", "{opened}");

    cleanup(&home);
}

/// T408b: semantic post-action waits should honor canonical locator waits.
#[test]
#[ignore]
#[serial]
fn t408b_click_wait_after_testid_reports_wait_metadata() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Semantic Wait-After Fixture</title></head>
<body>
  <button id="trigger">Trigger</button>
  <script>
    document.getElementById('trigger').addEventListener('click', () => {
      setTimeout(() => {
        const ready = document.createElement('div');
        ready.setAttribute('data-testid', 'ready-pill');
        ready.textContent = 'ready';
        document.body.appendChild(ready);
      }, 150);
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

    let clicked = parse_json(
        &rub_cmd(&home)
            .args([
                "click",
                "--selector",
                "#trigger",
                "--wait-after-testid",
                "ready-pill",
                "--wait-after-timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(clicked["data"]["wait_after"]["matched"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["wait_after"]["kind"], "test_id",
        "{clicked}"
    );
    assert_eq!(
        clicked["data"]["wait_after"]["value"], "ready-pill",
        "{clicked}"
    );

    cleanup(&home);
}

/// T409: selector addressing should resolve canonical interactive commands without requiring a manual snapshot id.
#[test]
#[ignore]
#[serial]
fn t409_click_selector_uses_canonical_locator_without_snapshot() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Selector Locator Fixture</title></head>
<body>
  <button id="save">Save profile</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('save').addEventListener('click', () => {
      document.getElementById('status').textContent = 'saved';
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

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#save"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["interaction_confirmed"], true,
        "{clicked}"
    );

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "saved", "{status}");

    cleanup(&home);
}

/// T410: text-based locators should resolve canonical query commands without an explicit element index.
#[test]
#[ignore]
#[serial]
fn t410_get_text_target_text_uses_text_locator() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Text Locator Fixture</title></head>
<body>
  <button id="alpha">Alpha</button>
  <button id="beta">Beta</button>
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
            .args(["get", "text", "--target-text", "Beta"])
            .output()
            .unwrap(),
    );
    assert_eq!(text["success"], true, "{text}");
    assert_eq!(text["data"]["result"]["value"], "Beta", "{text}");

    cleanup(&home);
}

/// T411: `fill` should drive canonical input/select/toggle steps and optionally submit the workflow.
#[test]
#[ignore]
#[serial]
fn t411_fill_executes_multi_step_form_workflow() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Fill Workflow Fixture</title></head>
<body>
  <form id="profile">
    <input id="name" value="" />
    <select id="role">
      <option value="">Choose</option>
      <option value="admin">Admin</option>
      <option value="member">Member</option>
    </select>
    <input id="tos" type="checkbox" />
    <button id="submit" type="button">Submit</button>
  </form>
  <div id="status">idle</div>
  <script>
    document.getElementById('submit').addEventListener('click', () => {
      const name = document.getElementById('name').value;
      const role = document.getElementById('role').value;
      const tos = document.getElementById('tos').checked ? 'yes' : 'no';
      document.getElementById('status').textContent = `${name}|${role}|${tos}|submitted`;
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

    let spec = json!([
        { "selector": "#name", "value": "Ada Lovelace" },
        { "selector": "#role", "value": "admin" },
        { "selector": "#tos", "activate": true }
    ])
    .to_string();

    let filled = parse_json(
        &rub_cmd(&home)
            .args([
                "fill",
                &spec,
                "--submit-selector",
                "#submit",
                "--wait-after-text",
                "submitted",
                "--wait-after-timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(filled["success"], true, "{filled}");
    assert_eq!(
        filled["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        4,
        "{filled}"
    );
    assert_eq!(
        filled["data"]["steps"][0]["action"]["command"], "type",
        "{filled}"
    );
    assert_eq!(
        filled["data"]["steps"][1]["action"]["command"], "select",
        "{filled}"
    );
    assert_eq!(filled["data"]["wait_after"]["matched"], true, "{filled}");

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(
        status["data"]["result"], "Ada Lovelace|admin|yes|submitted",
        "{status}"
    );

    cleanup(&home);
}

/// T411f: `fill --file` should load the fill workflow from disk and reuse canonical fill execution.
#[test]
#[ignore]
#[serial]
fn t411f_fill_file_loads_spec_from_disk() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Fill File Fixture</title></head>
<body>
  <form id="profile">
    <input id="name" value="" />
    <button id="submit" type="button">Submit</button>
  </form>
  <div id="status">idle</div>
  <script>
    document.getElementById('submit').addEventListener('click', () => {
      document.getElementById('status').textContent = document.getElementById('name').value + '|submitted';
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

    let spec_path =
        std::env::temp_dir().join(format!("rub-fill-file-{}.json", uuid::Uuid::now_v7()));
    std::fs::write(
        &spec_path,
        r##"[{"selector":"#name","value":"Grace Hopper"}]"##,
    )
    .expect("fill spec file");

    let filled = parse_json(
        &rub_cmd(&home)
            .args([
                "fill",
                "--file",
                spec_path.to_string_lossy().as_ref(),
                "--submit-selector",
                "#submit",
                "--wait-after-text",
                "submitted",
                "--wait-after-timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(filled["success"], true, "{filled}");
    assert_eq!(
        filled["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{filled}"
    );

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(
        status["data"]["result"], "Grace Hopper|submitted",
        "{status}"
    );

    let _ = std::fs::remove_file(spec_path);
    cleanup(&home);
}

/// T411b: `fill` submit locators and step-level waits should share canonical semantic locator support.
#[test]
#[ignore]
#[serial]
fn t411b_fill_submit_label_and_step_wait_after_use_semantic_locators() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Fill Semantic Locator Fixture</title></head>
<body>
  <input id="name" value="" />
  <button id="reveal" type="button">Reveal Notes</button>
  <textarea id="notes" aria-label="Notes Area" style="display:none"></textarea>
  <button id="submit" type="button" aria-label="Confirm profile">Submit</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('reveal').addEventListener('click', () => {
      setTimeout(() => {
        document.getElementById('notes').style.display = 'block';
      }, 150);
    });
    document.getElementById('submit').addEventListener('click', () => {
      const name = document.getElementById('name').value;
      const notes = document.getElementById('notes').value;
      document.getElementById('status').textContent = `${name}|${notes}|submitted`;
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

    let spec = json!([
        { "selector": "#name", "value": "Ada" },
        { "selector": "#reveal", "activate": true, "wait_after": { "label": "Notes Area", "timeout_ms": 5000 } },
        { "label": "Notes Area", "value": "Ready" }
    ])
    .to_string();

    let filled = parse_json(
        &rub_cmd(&home)
            .args([
                "fill",
                &spec,
                "--submit-label",
                "Confirm profile",
                "--wait-after-text",
                "submitted",
                "--wait-after-timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(filled["success"], true, "{filled}");
    assert_eq!(
        filled["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        4,
        "{filled}"
    );
    assert_eq!(filled["data"]["wait_after"]["matched"], true, "{filled}");

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "Ada|Ready|submitted", "{status}");

    cleanup(&home);
}

/// T411c: `fill --submit-ref` should route submit through the canonical ref locator.
#[test]
#[ignore]
#[serial]
fn t411c_fill_submit_ref_uses_canonical_locator() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Fill Submit Ref Fixture</title></head>
<body>
  <input id="name" value="" />
  <button id="submit" type="button">Submit</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('submit').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('name').value + '|submitted';
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

    let state = parse_json(&rub_cmd(&home).args(["state"]).output().unwrap());
    assert_eq!(state["success"], true, "{state}");
    let submit_ref = find_element_ref(&state, |element| {
        element["text"] == "Submit" && element["tag"] == "button"
    });

    let spec = json!([{ "selector": "#name", "value": "Grace" }]).to_string();
    let filled = parse_json(
        &rub_cmd(&home)
            .args([
                "fill",
                &spec,
                "--submit-ref",
                &submit_ref,
                "--wait-after-text",
                "submitted",
                "--wait-after-timeout",
                "5000",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(filled["success"], true, "{filled}");

    let status = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["data"]["result"], "Grace|submitted", "{status}");

    cleanup(&home);
}

#[test]
#[ignore]
#[serial]
fn t411d_fill_preserves_multiline_textarea_newlines() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Fill Multiline Fixture</title></head>
<body>
  <textarea id="notes" aria-label="Notes"></textarea>
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

    let spec = json!([
        { "label": "Notes", "value": "Line one\nLine two" }
    ])
    .to_string();

    let filled = parse_json(&rub_cmd(&home).args(["fill", &spec]).output().unwrap());
    assert_eq!(filled["success"], true, "{filled}");
    assert_eq!(
        filled["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1,
        "{filled}"
    );
    assert_eq!(
        filled["data"]["steps"][0]["result"]["interaction"]["confirmation_status"], "confirmed",
        "{filled}"
    );
    assert_eq!(
        filled["data"]["steps"][0]["result"]["interaction"]["confirmation_kind"], "value_applied",
        "{filled}"
    );

    let verify = parse_json(
        &rub_cmd(&home)
            .args(["exec", "document.getElementById('notes').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(verify["success"], true, "{verify}");
    assert_eq!(verify["data"]["result"], "Line one\nLine two", "{verify}");

    cleanup(&home);
}

/// T411e: `fill` should report text-entry steps through the baseline `type` surface and
/// surface locator-aware recovery suggestions on selector failures.
#[test]
#[ignore]
#[serial]
fn t411e_fill_reports_type_surface_and_selector_recovery_suggestion() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Fill Error Fixture</title></head>
<body>
  <input id="name" value="" />
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

    let bad_spec = json!([{ "selector": ".missing-field", "value": "Ada" }]).to_string();
    let failed = parse_json(&rub_cmd(&home).args(["fill", &bad_spec]).output().unwrap());
    assert_eq!(failed["success"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "ELEMENT_NOT_FOUND", "{failed}");
    let suggestion = failed["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(suggestion.contains("--role"), "{failed}");
    assert!(suggestion.contains("inspect page"), "{failed}");

    cleanup(&home);
}

/// T412: `extract` should return structured values through canonical locator resolution.
#[test]
#[ignore]
#[serial]
fn t412_extract_returns_structured_fields() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Extract Fixture</title></head>
<body>
  <input id="email" value="user@example.com" />
  <button id="primary" title="Run primary action" data-intent="primary">Primary CTA</button>
  <button id="secondary">Secondary CTA</button>
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

    let spec = json!({
        "email_value": { "selector": "#email", "kind": "value" },
        "primary_text": { "selector": "#primary", "kind": "text" },
        "primary_title": { "selector": "#primary", "kind": "attribute", "attribute": "title" },
        "all_button_text": { "selector": "button", "kind": "text", "many": true }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    assert_eq!(
        extracted["data"]["result"]["fields"]["email_value"], "user@example.com",
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["primary_text"], "Primary CTA",
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["primary_title"], "Run primary action",
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["all_button_text"],
        json!(["Primary CTA", "Secondary CTA"]),
        "{extracted}"
    );

    cleanup(&home);
}

/// T412d: `extract --file` should load the structured extract spec from disk.
#[test]
#[ignore]
#[serial]
fn t412d_extract_file_loads_spec_from_disk() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Extract File Fixture</title></head>
<body>
  <h1 id="headline">Disk Spec</h1>
  <button id="primary">Primary CTA</button>
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

    let spec_path =
        std::env::temp_dir().join(format!("rub-extract-file-{}.json", uuid::Uuid::now_v7()));
    std::fs::write(
        &spec_path,
        r##"{"headline":{"selector":"#headline","type":"text"},"buttons":{"selector":"button","kind":"text","many":true}}"##,
    )
    .expect("extract spec file");

    let extracted = parse_json(
        &rub_cmd(&home)
            .args(["extract", "--file", spec_path.to_string_lossy().as_ref()])
            .output()
            .unwrap(),
    );
    assert_eq!(extracted["success"], true, "{extracted}");
    assert_eq!(
        extracted["data"]["result"]["fields"]["headline"], "Disk Spec",
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["buttons"],
        json!(["Primary CTA"]),
        "{extracted}"
    );

    let _ = std::fs::remove_file(spec_path);
    cleanup(&home);
}

/// T412b: `extract` should fall back to non-interactive content selectors for public-web style reads.
#[test]
#[ignore]
#[serial]
fn t412b_extract_reads_non_interactive_content_selectors() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Extract Content Fixture</title></head>
<body>
  <article id="story" data-kind="article">
    <h1 id="headline">Hello Public Web</h1>
    <p class="lead">Readable content for extract fallback.</p>
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

    let spec = json!({
        "headline_text": { "selector": "#headline", "kind": "text" },
        "lead_html": { "selector": ".lead", "kind": "html" },
        "story_kind": { "selector": "#story", "kind": "attribute", "attribute": "data-kind" }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    assert_eq!(
        extracted["data"]["result"]["fields"]["headline_text"], "Hello Public Web",
        "{extracted}"
    );
    assert!(
        extracted["data"]["result"]["fields"]["lead_html"]
            .as_str()
            .unwrap()
            .contains("Readable content for extract fallback."),
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["story_kind"], "article",
        "{extracted}"
    );

    cleanup(&home);
}

/// T412g: content fallback should honor first/last/nth selection for repeated matches.
#[test]
#[ignore]
#[serial]
fn t412g_extract_content_fallback_honors_selection() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Extract Selection Fixture</title></head>
<body>
  <section id="feed">
    <p class="entry">First item</p>
    <p class="entry">Second item</p>
    <p class="entry">Third item</p>
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

    let spec = json!({
        "first_entry": { "selector": ".entry", "kind": "text", "first": true },
        "last_entry": { "selector": ".entry", "kind": "text", "last": true },
        "middle_entry": { "selector": ".entry", "kind": "text", "nth": 1 }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    assert_eq!(
        extracted["data"]["result"]["fields"]["first_entry"], "First item",
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["last_entry"], "Third item",
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["middle_entry"], "Second item",
        "{extracted}"
    );

    cleanup(&home);
}

/// T412c: `extract` should support typed transforms, mapping, and optional defaults.
#[test]
#[ignore]
#[serial]
fn t412c_extract_supports_typed_transforms_and_defaults() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Typed Extract Fixture</title></head>
<body>
  <div class="price"> 12.50 </div>
  <div class="stock">In Stock</div>
  <ul>
    <li class="tag"> Alpha </li>
    <li class="tag"> Beta </li>
  </ul>
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

    let spec = json!({
        "price": {
            "selector": ".price",
            "kind": "text",
            "transform": "parse_float",
            "type": "number"
        },
        "in_stock": {
            "selector": ".stock",
            "kind": "text",
            "map": { "In Stock": true, "Out of Stock": false },
            "type": "boolean"
        },
        "tags": {
            "selector": ".tag",
            "kind": "text",
            "many": true,
            "transform": "trim",
            "type": "array"
        },
        "missing_optional": {
            "selector": ".does-not-exist",
            "kind": "text",
            "required": false,
            "default": []
        }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    assert_eq!(
        extracted["data"]["result"]["fields"]["price"],
        json!(12.5),
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["in_stock"],
        json!(true),
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["tags"],
        json!(["Alpha", "Beta"]),
        "{extracted}"
    );
    assert_eq!(
        extracted["data"]["result"]["fields"]["missing_optional"],
        json!([]),
        "{extracted}"
    );

    cleanup(&home);
}

/// T413: `pipe` should execute a bounded workflow over canonical command surfaces.
#[test]
#[ignore]
#[serial]
fn t413_pipe_executes_bounded_workflow_pipeline() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Pipe Fixture</title></head>
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

    let spec = json!([
        { "command": "open", "args": { "url": server.url() }, "label": "open" },
        { "command": "type", "args": { "selector": "#name", "text": "Grace Hopper", "clear": true }, "label": "name" },
        { "command": "click", "args": { "selector": "#apply", "wait_after": { "text": "Grace Hopper", "timeout_ms": 5000 } }, "label": "apply" },
        { "command": "exec", "args": { "code": "document.getElementById('status').textContent" }, "label": "read" }
    ])
    .to_string();

    let piped = parse_json(&rub_cmd(&home).args(["pipe", &spec]).output().unwrap());
    assert_eq!(piped["success"], true, "{piped}");
    assert_eq!(
        piped["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        4,
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][2]["result"]["wait_after"]["matched"], true,
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][3]["result"]["result"], "Grace Hopper",
        "{piped}"
    );

    cleanup(&home);
}

/// T413b: `pipe --file` should load the workflow from disk and reuse canonical pipe execution.
#[test]
#[ignore]
#[serial]
fn t413b_pipe_file_executes_bounded_workflow_pipeline() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Pipe File Fixture</title></head>
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

    let spec = json!([
        { "command": "open", "args": { "url": server.url() }, "label": "open" },
        { "command": "type", "args": { "selector": "#name", "text": "Ada Lovelace", "clear": true }, "label": "name" },
        { "command": "click", "args": { "selector": "#apply", "wait_after": { "text": "Ada Lovelace", "timeout_ms": 5000 } }, "label": "apply" },
        { "command": "exec", "args": { "code": "document.getElementById('status').textContent" }, "label": "read" }
    ])
    .to_string();
    let spec_path =
        std::env::temp_dir().join(format!("rub-pipe-file-{}.json", uuid::Uuid::now_v7()));
    std::fs::write(&spec_path, spec).unwrap();

    let piped = parse_json(
        &rub_cmd(&home)
            .args(["pipe", "--file", spec_path.to_string_lossy().as_ref()])
            .output()
            .unwrap(),
    );
    assert_eq!(piped["success"], true, "{piped}");
    assert_eq!(
        piped["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        4,
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][3]["result"]["result"], "Ada Lovelace",
        "{piped}"
    );

    let _ = std::fs::remove_file(spec_path);
    cleanup(&home);
}

/// T413c: `pipe --file` should resolve secret references from env / secrets.env
/// while keeping workflow output redacted.
#[test]
#[ignore]
#[serial]
fn t413c_pipe_file_resolves_secret_references_and_redacts_output() {
    let home = unique_home();
    cleanup(&home);
    std::fs::create_dir_all(&home).unwrap();
    write_secure_secrets_env(
        &PathBuf::from(&home).join("secrets.env"),
        "RUB_PASSWORD=file-pass\n",
    );

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Pipe Secret Fixture</title></head>
<body>
  <input id="user" value="" />
  <input id="password" value="" />
  <button id="apply">Apply</button>
  <div id="status">idle</div>
  <script>
    document.getElementById('apply').addEventListener('click', () => {
      document.getElementById('status').textContent =
        document.getElementById('user').value + ':' + document.getElementById('password').value;
    });
  </script>
</body>
</html>"#,
    )]);

    let spec = json!([
        { "command": "open", "args": { "url": server.url() }, "label": "open" },
        { "command": "type", "args": { "selector": "#user", "text": "$RUB_USER", "clear": true }, "label": "user" },
        { "command": "type", "args": { "selector": "#password", "text": "$RUB_PASSWORD", "clear": true }, "label": "password" },
        { "command": "click", "args": { "selector": "#apply" }, "label": "apply" },
        { "command": "exec", "args": { "code": "document.getElementById('status').textContent" }, "label": "read" }
    ])
    .to_string();
    let spec_path = std::env::temp_dir().join(format!(
        "rub-pipe-secret-file-{}.json",
        uuid::Uuid::now_v7()
    ));
    std::fs::write(&spec_path, spec).unwrap();

    let piped = parse_json(
        &rub_cmd_env(&home, &[("RUB_USER", "env-user")])
            .args(["pipe", "--file", spec_path.to_string_lossy().as_ref()])
            .output()
            .unwrap(),
    );
    assert_eq!(piped["success"], true, "{piped}");
    assert_eq!(
        piped["data"]["steps"][1]["result"]["result"]["text"], "$RUB_USER",
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][2]["result"]["result"]["text"], "$RUB_PASSWORD",
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][4]["result"]["result"], "$RUB_USER:$RUB_PASSWORD",
        "{piped}"
    );

    let actual = parse_json(
        &rub_cmd(&home)
            .args([
                "exec",
                "document.getElementById('status').textContent === 'env-user:file-pass'",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(actual["success"], true, "{actual}");
    assert_eq!(actual["data"]["result"], true, "{actual}");

    let _ = std::fs::remove_file(spec_path);
    cleanup(&home);
}

/// T413d: `pipe --file --var` should resolve exact workflow placeholders before canonical pipe execution.
#[test]
#[ignore]
#[serial]
fn t413d_pipe_file_parameterizes_workflow_placeholders() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/param",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Pipe Var Fixture</title></head>
<body>
  <div id="status">idle</div>
  <script>
    document.getElementById('status').textContent = window.location.pathname;
  </script>
</body>
</html>"#,
    )]);

    let spec = json!([
        { "command": "open", "args": { "url": "{{target_url}}" }, "label": "open" },
        { "command": "exec", "args": { "code": "document.getElementById('status').textContent" }, "label": "read" }
    ])
    .to_string();
    let spec_path =
        std::env::temp_dir().join(format!("rub-pipe-vars-file-{}.json", uuid::Uuid::now_v7()));
    std::fs::write(&spec_path, spec).unwrap();

    let piped = parse_json(
        &rub_cmd(&home)
            .args([
                "pipe",
                "--file",
                spec_path.to_string_lossy().as_ref(),
                "--var",
                &format!("target_url={}", server.url_for("/param")),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(piped["success"], true, "{piped}");
    assert_eq!(
        piped["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][1]["result"]["result"], "/param",
        "{piped}"
    );

    let _ = std::fs::remove_file(spec_path);
    cleanup(&home);
}

/// T413e: `pipe` should admit bounded observation steps such as `observe` and `screenshot`.
#[test]
#[ignore]
#[serial]
fn t413e_pipe_supports_observe_and_screenshot_steps() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/pipe-observe",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Pipe Observe Fixture</title></head>
<body>
  <main>
    <h1>Observe Me</h1>
    <button id="advance">Advance</button>
  </main>
</body>
</html>"#,
    )]);

    let observe_path = format!("{home}/observe.png");
    let screenshot_path = format!("{home}/screenshot.png");
    let spec = json!([
        { "command": "open", "args": { "url": server.url_for("/pipe-observe") }, "label": "open" },
        { "command": "observe", "args": { "path": observe_path, "limit": 10 }, "label": "observe" },
        { "command": "screenshot", "args": { "path": screenshot_path, "highlight": true }, "label": "screenshot" }
    ])
    .to_string();

    let piped = parse_json(&rub_cmd(&home).args(["pipe", &spec]).output().unwrap());
    assert_eq!(piped["success"], true, "{piped}");
    assert_eq!(
        piped["data"]["steps"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        3,
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][1]["action"]["command"], "observe",
        "{piped}"
    );
    assert_eq!(
        piped["data"]["steps"][2]["action"]["command"], "screenshot",
        "{piped}"
    );
    assert!(std::fs::metadata(&observe_path).is_ok());
    assert!(std::fs::metadata(&screenshot_path).is_ok());

    cleanup(&home);
}

/// T414: `state --diff` should expose semantic summaries and per-element semantic change kinds.
#[test]
#[ignore]
#[serial]
fn t414_state_diff_reports_semantic_summary() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Semantic Diff Fixture</title></head>
<body>
  <button id="alpha" style="width: 100px">Alpha</button>
</body>
</html>"#,
    )]);

    rub_cmd(&home)
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let base = run_state(&home);
    let snap = snapshot_id(&base);

    rub_cmd(&home)
        .args([
            "exec",
            "const button = document.getElementById('alpha'); button.textContent = 'Alpha Updated'; button.style.width = '200px';",
        ])
        .output()
        .unwrap();

    let diff = parse_json(
        &rub_cmd(&home)
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true, "{diff}");
    assert_eq!(
        diff["data"]["result"]["diff"]["summary"]["content_changes"], 1,
        "{diff}"
    );
    assert_eq!(
        diff["data"]["result"]["diff"]["summary"]["geometry_changes"], 1,
        "{diff}"
    );

    let changed = diff["data"]["result"]["diff"]["changed"]
        .as_array()
        .unwrap();
    assert!(
        changed.iter().any(|element| {
            element["semantic_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind.as_str() == Some("content"))
                && element["semantic_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|kind| kind.as_str() == Some("geometry"))
        }),
        "{diff}"
    );

    cleanup(&home);
}

/// T414b: `find` supports semantic locators plus bounded match disambiguation.
#[test]
#[ignore]
#[serial]
fn t414b_find_supports_semantic_locators_and_disambiguation() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Find Fixture</title></head>
<body>
  <main>
    <button data-testid="save-primary">Save</button>
    <button data-testid="save-secondary">Save</button>
    <button aria-label="Archive item"><span aria-hidden="true">Archive</span></button>
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

    let all_buttons = parse_json(
        &rub_cmd(&home)
            .args(["find", "--role", "button"])
            .output()
            .unwrap(),
    );
    assert_eq!(all_buttons["success"], true, "{all_buttons}");
    assert_eq!(
        all_buttons["data"]["subject"]["kind"], "find_query",
        "{all_buttons}"
    );
    assert_eq!(
        all_buttons["data"]["subject"]["surface"], "interactive_snapshot",
        "{all_buttons}"
    );
    assert_eq!(
        all_buttons["data"]["result"]["match_count"], 3,
        "{all_buttons}"
    );
    let matches = all_buttons["data"]["result"]["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 3, "{all_buttons}");
    assert!(matches.iter().any(|entry| {
        entry["testid"] == "save-primary" && entry["label"] == "Save" && entry["role"] == "button"
    }));

    let nth_button = parse_json(
        &rub_cmd(&home)
            .args(["find", "--role", "button", "--nth", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(nth_button["success"], true, "{nth_button}");
    assert_eq!(
        nth_button["data"]["result"]["match_count"], 1,
        "{nth_button}"
    );
    assert_eq!(
        nth_button["data"]["result"]["returned_count"], 1,
        "{nth_button}"
    );
    assert_eq!(
        nth_button["data"]["result"]["matches"][0]["testid"], "save-secondary",
        "{nth_button}"
    );

    let labeled = parse_json(
        &rub_cmd(&home)
            .args(["find", "--label", "Archive item"])
            .output()
            .unwrap(),
    );
    assert_eq!(labeled["success"], true, "{labeled}");
    assert_eq!(labeled["data"]["result"]["match_count"], 1, "{labeled}");
    assert_eq!(
        labeled["data"]["result"]["matches"][0]["label"],
        "Archive item"
    );
    assert_eq!(labeled["data"]["result"]["matches"][0]["role"], "button");

    let read_text = parse_json(
        &rub_cmd(&home)
            .args(["get", "text", "--testid", "save-secondary"])
            .output()
            .unwrap(),
    );
    assert_eq!(read_text["success"], true, "{read_text}");
    assert_eq!(read_text["data"]["result"]["value"], "Save", "{read_text}");

    cleanup(&home);
}

/// T414l: `find --content` should locate static heading/content anchors without widening the
/// default interactive snapshot find surface.
#[test]
#[ignore]
#[serial]
fn t414l_find_content_supports_static_heading_anchors() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Content Find Fixture</title></head>
<body>
  <main>
    <h2>Overview</h2>
    <p>Intro copy</p>
    <h2>External links</h2>
    <ul><li>Example</li></ul>
    <button>Archive</button>
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

    let interactive_only = parse_json(
        &rub_cmd(&home)
            .args(["find", "--target-text", "External links"])
            .output()
            .unwrap(),
    );
    assert_eq!(interactive_only["success"], false, "{interactive_only}");
    assert_eq!(interactive_only["error"]["code"], "ELEMENT_NOT_FOUND");

    let content = parse_json(
        &rub_cmd(&home)
            .args(["find", "--content", "--target-text", "External links"])
            .output()
            .unwrap(),
    );
    assert_eq!(content["success"], true, "{content}");
    assert_eq!(
        content["data"]["subject"]["surface"], "content",
        "{content}"
    );
    assert_eq!(content["data"]["result"]["match_count"], 1, "{content}");
    assert_eq!(
        content["data"]["result"]["matches"][0]["tag_name"], "h2",
        "{content}"
    );
    assert_eq!(
        content["data"]["result"]["matches"][0]["role"], "heading",
        "{content}"
    );
    assert_eq!(
        content["data"]["result"]["matches"][0]["text"], "External links",
        "{content}"
    );

    let heading = parse_json(
        &rub_cmd(&home)
            .args(["find", "--content", "--role", "heading", "--nth", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(heading["success"], true, "{heading}");
    assert_eq!(heading["data"]["result"]["match_count"], 1, "{heading}");
    assert_eq!(
        heading["data"]["result"]["matches"][0]["text"], "External links",
        "{heading}"
    );

    cleanup(&home);
}

/// T414c: `state --selector` and `observe --selector` scope projections to interactive descendants.
#[test]
#[ignore]
#[serial]
fn t414c_state_and_observe_scope_to_content_root() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Scoped Observation Fixture</title></head>
<body>
  <section id="content">
    <button>Inside Action</button>
    <a href="/terms">Terms</a>
    <p>Read only copy</p>
  </section>
  <aside>
    <button>Outside Action</button>
  </aside>
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

    let scoped_state = parse_json(
        &rub_cmd(&home)
            .args(["state", "--scope-selector", "#content"])
            .output()
            .unwrap(),
    );
    assert_eq!(scoped_state["success"], true, "{scoped_state}");
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope"]["kind"],
        "selector"
    );
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope"]["css"],
        "#content"
    );
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope_filtered"],
        true
    );
    assert_eq!(scoped_state["data"]["result"]["snapshot"]["scope_count"], 2);
    let elements = scoped_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 2, "{scoped_state}");
    assert!(
        elements
            .iter()
            .any(|element| element["text"] == "Inside Action")
    );
    assert!(elements.iter().any(|element| element["text"] == "Terms"));
    assert!(
        !elements
            .iter()
            .any(|element| element["text"] == "Outside Action")
    );

    let screenshot_path = format!("{home}/scoped-observe.png");
    let scoped_observe = parse_json(
        &rub_cmd(&home)
            .args([
                "observe",
                "--scope-selector",
                "#content",
                "--path",
                &screenshot_path,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(scoped_observe["success"], true, "{scoped_observe}");
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope"]["kind"],
        "selector"
    );
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope"]["css"],
        "#content"
    );
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope_filtered"],
        true
    );
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope_count"],
        2
    );
    assert!(std::fs::metadata(&screenshot_path).is_ok());
    let summary = scoped_observe["data"]["result"]["snapshot"]["summary"]["text"]
        .as_str()
        .unwrap();
    assert!(summary.contains("Inside Action"), "{scoped_observe}");
    assert!(summary.contains("Terms"), "{scoped_observe}");
    assert!(!summary.contains("Outside Action"), "{scoped_observe}");
    let map = scoped_observe["data"]["result"]["snapshot"]["element_map"]
        .as_array()
        .unwrap();
    assert_eq!(map.len(), 2, "{scoped_observe}");

    cleanup(&home);
}

/// T414h: semantic observation scope should project interactive descendants of the selected content root.
#[test]
#[ignore]
#[serial]
fn t414h_state_and_observe_support_semantic_scope_roots() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Semantic Scoped Observation Fixture</title></head>
<body>
  <main data-testid="primary-content">
    <button>Inside Main</button>
    <a href="/docs">Docs</a>
  </main>
  <main>
    <button>Other Main</button>
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

    let scoped_state = parse_json(
        &rub_cmd(&home)
            .args(["state", "--scope-role", "main", "--scope-first"])
            .output()
            .unwrap(),
    );
    assert_eq!(scoped_state["success"], true, "{scoped_state}");
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope"]["kind"],
        "role"
    );
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope"]["role"],
        "main"
    );
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope_filtered"],
        true
    );
    assert_eq!(
        scoped_state["data"]["result"]["snapshot"]["scope_match_count"],
        2
    );
    let elements = scoped_state["data"]["result"]["snapshot"]["elements"]
        .as_array()
        .unwrap();
    assert_eq!(elements.len(), 2, "{scoped_state}");
    assert!(
        elements
            .iter()
            .any(|element| element["text"] == "Inside Main")
    );
    assert!(elements.iter().any(|element| element["text"] == "Docs"));
    assert!(
        !elements
            .iter()
            .any(|element| element["text"] == "Other Main")
    );

    let scoped_observe = parse_json(
        &rub_cmd(&home)
            .args(["observe", "--scope-testid", "primary-content"])
            .output()
            .unwrap(),
    );
    assert_eq!(scoped_observe["success"], true, "{scoped_observe}");
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope"]["kind"],
        "test_id"
    );
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope"]["testid"],
        "primary-content"
    );
    assert_eq!(
        scoped_observe["data"]["result"]["snapshot"]["scope_count"],
        2
    );
    let summary = scoped_observe["data"]["result"]["snapshot"]["summary"]["text"]
        .as_str()
        .unwrap();
    assert!(summary.contains("Inside Main"), "{scoped_observe}");
    assert!(summary.contains("Docs"), "{scoped_observe}");
    assert!(!summary.contains("Other Main"), "{scoped_observe}");

    cleanup(&home);
}

/// T414i: compact observation projection and relative depth filtering should reduce scoped noise without changing scope authority.
#[test]
#[ignore]
#[serial]
fn t414i_state_and_observe_support_compact_projection_and_depth() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Compact Observation Fixture</title></head>
<body>
  <main role="main" data-testid="primary-content">
    <button>Top Action</button>
    <section>
      <a href="/docs">Docs</a>
      <div><button>Deep Action</button></div>
    </section>
  </main>
  <aside>
    <button>Outside</button>
  </aside>
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

    let compact_state = parse_json(
        &rub_cmd(&home)
            .args([
                "state",
                "--scope-role",
                "main",
                "--scope-first",
                "--format",
                "compact",
                "--depth",
                "1",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(compact_state["success"], true, "{compact_state}");
    assert_eq!(
        compact_state["data"]["result"]["snapshot"]["observation_projection"]["mode"],
        "compact"
    );
    assert_eq!(
        compact_state["data"]["result"]["snapshot"]["observation_projection"]["depth_limit"],
        1
    );
    assert_eq!(
        compact_state["data"]["result"]["snapshot"]["observation_projection"]["depth_count"],
        1
    );
    let entries = compact_state["data"]["result"]["snapshot"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 1, "{compact_state}");
    assert_eq!(entries[0]["depth"], 1, "{compact_state}");
    assert_eq!(entries[0]["label"], "Top Action", "{compact_state}");
    let compact_text = compact_state["data"]["result"]["snapshot"]["compact_text"]
        .as_str()
        .unwrap();
    assert!(compact_text.contains("Top Action"), "{compact_state}");
    assert!(!compact_text.contains("Docs"), "{compact_state}");
    assert!(!compact_text.contains("Deep Action"), "{compact_state}");
    assert!(!compact_text.contains("Outside"), "{compact_state}");
    assert!(
        compact_text.lines().all(|line| !line.starts_with("  ")),
        "{compact_state}"
    );
    assert!(compact_text.contains("@1]"), "{compact_state}");

    let compact_observe = parse_json(
        &rub_cmd(&home)
            .args([
                "observe",
                "--scope-testid",
                "primary-content",
                "--compact",
                "--depth",
                "2",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(compact_observe["success"], true, "{compact_observe}");
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["observation_projection"]["mode"],
        "compact"
    );
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["observation_projection"]["depth_limit"],
        2
    );
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["observation_projection"]["depth_count"],
        2
    );
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["summary"]["format"],
        "compact"
    );
    let summary = compact_observe["data"]["result"]["snapshot"]["summary"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["compact_text"],
        summary
    );
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["compact_lines"],
        2
    );
    assert_eq!(
        compact_observe["data"]["result"]["snapshot"]["summary"]["line_count"],
        2
    );
    assert!(summary.contains("Top Action"), "{compact_observe}");
    assert!(summary.contains("Docs"), "{compact_observe}");
    assert!(!summary.contains("Deep Action"), "{compact_observe}");
    assert!(
        summary.lines().all(|line| !line.starts_with("  ")),
        "{compact_observe}"
    );
    assert!(
        summary.contains("@1]") || summary.contains("@2]"),
        "{compact_observe}"
    );
    assert!(!summary.contains("Outside"), "{compact_observe}");
    let map = compact_observe["data"]["result"]["snapshot"]["element_map"]
        .as_array()
        .unwrap();
    assert_eq!(map.len(), 2, "{compact_observe}");
    assert_eq!(map[0]["depth"], 1, "{compact_observe}");
    assert_eq!(map[1]["depth"], 2, "{compact_observe}");

    cleanup(&home);
}

/// T414d: `extract` supports repeated collection rows with typed child postprocessing.
#[test]
#[ignore]
#[serial]
fn t414d_extract_supports_repeated_collection_rows() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Collection Extract Fixture</title></head>
<body>
  <article class="card">
    <h2 class="name">Alpha</h2>
    <span class="price" data-price="19.5">$19.50</span>
    <span class="tag">new</span>
    <span class="tag">sale</span>
    <a class="link" href="/alpha">View</a>
  </article>
  <article class="card">
    <h2 class="name">Beta</h2>
    <span class="price" data-price="7.0">$7.00</span>
    <span class="tag">featured</span>
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

    let spec = json!({
        "items": {
            "collection": ".card",
            "fields": {
                "name": { "selector": ".name", "kind": "text" },
                "price": {
                    "selector": ".price",
                    "kind": "attribute",
                    "attribute": "data-price",
                    "transform": "parse_float",
                    "type": "number"
                },
                "tags": {
                    "selector": ".tag",
                    "kind": "text",
                    "many": true,
                    "type": "array"
                },
                "href": {
                    "selector": ".link",
                    "kind": "attribute",
                    "attribute": "href",
                    "required": false,
                    "default": "n/a"
                }
            }
        }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    let items = extracted["data"]["result"]["fields"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 2, "{extracted}");
    assert_eq!(items[0]["name"], "Alpha", "{extracted}");
    assert_eq!(items[0]["price"], 19.5, "{extracted}");
    assert_eq!(items[0]["tags"], json!(["new", "sale"]), "{extracted}");
    assert_eq!(items[0]["href"], "/alpha", "{extracted}");
    assert_eq!(items[1]["name"], "Beta", "{extracted}");
    assert_eq!(items[1]["price"], 7.0, "{extracted}");
    assert_eq!(items[1]["tags"], json!(["featured"]), "{extracted}");
    assert_eq!(items[1]["href"], "n/a", "{extracted}");

    cleanup(&home);
}

/// T414j: dense-page extract failures should publish actionable correction hints for repeated top-level matches.
#[test]
#[ignore]
#[serial]
fn t414j_extract_dense_page_failures_publish_resolution_hints() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Dense Extract Diagnostics Fixture</title></head>
<body>
  <main>
    <a class="headline" href="/alpha">Alpha</a>
    <a class="headline" href="/beta">Beta</a>
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

    let spec = json!({
        "headline": { "selector": ".headline", "kind": "text" }
    })
    .to_string();

    let failed = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(failed["success"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "INVALID_INPUT", "{failed}");
    let suggestion = failed["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(suggestion.contains("many: true"), "{failed}");
    assert!(suggestion.contains("first"), "{failed}");
    assert!(suggestion.contains("nth"), "{failed}");
    assert_eq!(failed["error"]["context"]["field"], "headline", "{failed}");
    assert_eq!(
        failed["error"]["context"]["surface"], "interactive",
        "{failed}"
    );
    assert_eq!(
        failed["error"]["context"]["resolution_examples"]["collect_all"]["many"], true,
        "{failed}"
    );
    assert_eq!(
        failed["error"]["context"]["builder_field_examples"]["pick_first"],
        "headline=text:.headline@first",
        "{failed}"
    );

    cleanup(&home);
}

/// T414e: `extract` supports nested collection children for repeated list structures.
#[test]
#[ignore]
#[serial]
fn t414e_extract_supports_nested_collection_children() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Nested Collection Fixture</title></head>
<body>
  <section class="repo">
    <h2 class="repo-name">rub</h2>
    <ul class="labels">
      <li class="label" data-tone="green"><span class="text">automation</span></li>
      <li class="label" data-tone="blue"><span class="text">rust</span></li>
    </ul>
  </section>
  <section class="repo">
    <h2 class="repo-name">codex</h2>
    <ul class="labels">
      <li class="label" data-tone="purple"><span class="text">agent</span></li>
    </ul>
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

    let spec = json!({
        "repos": {
            "collection": ".repo",
            "fields": {
                "name": { "selector": ".repo-name", "kind": "text" },
                "labels": {
                    "collection": ".label",
                    "fields": {
                        "text": { "selector": ".text", "kind": "text" },
                        "tone": {
                            "kind": "attribute",
                            "attribute": "data-tone",
                            "required": false,
                            "default": "unknown"
                        }
                    }
                }
            }
        }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    let repos = extracted["data"]["result"]["fields"]["repos"]
        .as_array()
        .unwrap();
    assert_eq!(repos.len(), 2, "{extracted}");
    assert_eq!(repos[0]["name"], "rub", "{extracted}");
    assert_eq!(repos[0]["labels"]["item_count"], 2, "{extracted}");
    assert_eq!(
        repos[0]["labels"]["items"],
        json!([
            { "text": "automation", "tone": "green" },
            { "text": "rust", "tone": "blue" }
        ]),
        "{extracted}"
    );
    assert_eq!(repos[1]["name"], "codex", "{extracted}");
    assert_eq!(repos[1]["labels"]["item_count"], 1, "{extracted}");
    assert_eq!(
        repos[1]["labels"]["items"],
        json!([{ "text": "agent", "tone": "purple" }]),
        "{extracted}"
    );

    cleanup(&home);
}

/// T414f: nested collection children support row-scoped semantic locators beyond selector-only fields.
#[test]
#[ignore]
#[serial]
fn t414f_extract_nested_children_support_semantic_locators() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Nested Semantic Collection Fixture</title></head>
<body>
  <section class="repo">
    <h2 class="repo-name">rub</h2>
    <ul class="labels">
      <li class="label">
        <span data-testid="label-text">automation</span>
        <button aria-label="Remove automation">x</button>
      </li>
      <li class="label">
        <span data-testid="label-text">rust</span>
        <button aria-label="Remove rust">x</button>
      </li>
    </ul>
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

    let spec = json!({
        "repos": {
            "collection": ".repo",
            "fields": {
                "name": { "selector": ".repo-name", "kind": "text" },
                "labels": {
                    "collection": ".label",
                    "fields": {
                        "text": { "testid": "label-text", "kind": "text" },
                        "remove_label": {
                            "role": "button",
                            "kind": "attribute",
                            "attribute": "aria-label"
                        }
                    }
                }
            }
        }
    })
    .to_string();

    let extracted = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(extracted["success"], true, "{extracted}");
    let repos = extracted["data"]["result"]["fields"]["repos"]
        .as_array()
        .unwrap();
    assert_eq!(repos.len(), 1, "{extracted}");
    assert_eq!(repos[0]["labels"]["item_count"], 2, "{extracted}");
    assert_eq!(
        repos[0]["labels"]["items"],
        json!([
            { "text": "automation", "remove_label": "Remove automation" },
            { "text": "rust", "remove_label": "Remove rust" }
        ]),
        "{extracted}"
    );

    cleanup(&home);
}

/// T414k: collection-row extract failures should point callers toward row-scoped disambiguation instead of generic selector advice.
#[test]
#[ignore]
#[serial]
fn t414k_extract_collection_row_failures_publish_row_scoped_hints() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Collection Extract Diagnostics Fixture</title></head>
<body>
  <article class="card">
    <a href="/alpha">Primary CTA</a>
    <a href="/alpha/docs">Docs</a>
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

    let spec = json!({
        "items": {
            "collection": ".card",
            "fields": {
                "cta": { "selector": "a", "kind": "text" }
            }
        }
    })
    .to_string();

    let failed = parse_json(&rub_cmd(&home).args(["extract", &spec]).output().unwrap());
    assert_eq!(failed["success"], false, "{failed}");
    assert_eq!(failed["error"]["code"], "INVALID_INPUT", "{failed}");
    let suggestion = failed["error"]["suggestion"].as_str().unwrap_or_default();
    assert!(suggestion.contains("row-scoped"), "{failed}");
    assert!(suggestion.contains("many: true"), "{failed}");
    assert!(suggestion.contains("first"), "{failed}");
    assert_eq!(
        failed["error"]["context"]["collection"], "items",
        "{failed}"
    );
    assert_eq!(failed["error"]["context"]["field"], "cta", "{failed}");
    assert_eq!(
        failed["error"]["context"]["surface"], "collection_row",
        "{failed}"
    );
    assert_eq!(failed["error"]["context"]["row_index"], 0, "{failed}");
    assert_eq!(
        failed["error"]["context"]["resolution_examples"]["pick_first"]["first"], true,
        "{failed}"
    );
    assert_eq!(
        failed["error"]["context"]["builder_field_examples"]["pick_first"], "cta=text:a@first",
        "{failed}"
    );

    cleanup(&home);
}

/// T430: click-triggered downloads should surface canonical download effects on the interaction.
#[test]
#[ignore]
#[serial]
fn t430_click_triggered_download_surfaces_observed_effects() {
    let home = unique_home();
    cleanup(&home);
    let server = DownloadFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#download-fast"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");
    assert_eq!(
        clicked["data"]["interaction"]["interaction_confirmed"], true,
        "{clicked}"
    );

    let downloads = &clicked["data"]["interaction"]["observed_effects"]["downloads"];
    assert!(
        downloads["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty()),
        "{clicked}"
    );
    assert_eq!(
        downloads["last_download"]["suggested_filename"], "report.csv",
        "{clicked}"
    );
    assert_eq!(
        clicked["data"]["interaction_trace"]["observed_effects"]["downloads"], *downloads,
        "{clicked}"
    );

    cleanup(&home);
}

/// T431: `download wait` should complete the last managed download without requiring a manual id.
#[test]
#[ignore]
#[serial]
fn t431_download_wait_completes_managed_download() {
    let home = unique_home();
    cleanup(&home);
    let server = DownloadFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#download-fast"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["download", "wait", "--state", "completed"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");
    assert_eq!(
        waited["data"]["result"]["download"]["state"], "completed",
        "{waited}"
    );
    assert_eq!(
        waited["data"]["result"]["download"]["suggested_filename"], "report.csv",
        "{waited}"
    );
    let final_path = waited["data"]["result"]["download"]["final_path"]
        .as_str()
        .expect("download final path");
    assert!(Path::new(final_path).exists(), "{waited}");
    assert_eq!(waited["data"]["runtime"]["mode"], "managed", "{waited}");

    let downloads = parse_json(&rub_cmd(&home).arg("downloads").output().unwrap());
    assert_eq!(downloads["success"], true, "{downloads}");
    assert_eq!(
        downloads["data"]["result"]["last_download"]["guid"],
        waited["data"]["result"]["download"]["guid"],
        "{downloads}"
    );

    cleanup(&home);
}

/// T431b: timed-out large downloads should report live progress and operator guidance.
#[test]
#[ignore]
#[serial]
fn t431b_download_wait_timeout_reports_progress_and_guidance() {
    let home = unique_home();
    cleanup(&home);
    let server = DownloadFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#download-slow"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "wait",
                "--state",
                "completed",
                "--timeout",
                "150",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], false, "{waited}");
    assert_eq!(waited["error"]["code"], "WAIT_TIMEOUT", "{waited}");
    assert!(
        waited["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("current state is 'in_progress'"),
        "{waited}"
    );
    assert!(
        waited["error"]["suggestion"]
            .as_str()
            .unwrap_or_default()
            .contains("rub downloads"),
        "{waited}"
    );
    assert_eq!(waited["error"]["context"]["kind"], "download", "{waited}");
    assert_eq!(
        waited["error"]["context"]["download_runtime"]["mode"], "managed",
        "{waited}"
    );
    assert_eq!(
        waited["error"]["context"]["download"]["state"], "in_progress",
        "{waited}"
    );

    cleanup(&home);
}

/// T431c: `download save` should batch-save asset URLs from inspect-style JSON rows.
#[test]
#[ignore]
#[serial]
fn t431c_download_save_batches_assets_from_json_rows() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body>asset save</body></html>",
        ),
        ("/assets/a.jpg", "image/jpeg", "AAA"),
        ("/assets/b.png", "image/png", "BBBB"),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{home}/assets.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "fields": {
                "items": [
                    { "note_id": "alpha", "image_url": server.url_for("/assets/a.jpg") },
                    { "note_id": "beta", "image_url": server.url_for("/assets/b.png") }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    let output_dir = format!("{home}/saved");

    let saved = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "save",
                "--file",
                &source_path,
                "--input-field",
                "fields.items",
                "--url-field",
                "image_url",
                "--name-field",
                "note_id",
                "--output-dir",
                &output_dir,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        saved["data"]["result"]["summary"]["complete"], true,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["summary"]["source_count"], 2,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["summary"]["saved_count"], 2,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["summary"]["failed_count"], 0,
        "{saved}"
    );
    assert!(
        Path::new(&format!("{output_dir}/alpha.jpg")).exists(),
        "{saved}"
    );
    assert!(
        Path::new(&format!("{output_dir}/beta.png")).exists(),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/alpha.jpg")).unwrap(),
        "AAA"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/beta.png")).unwrap(),
        "BBBB"
    );

    cleanup(&home);
}

/// T431c2: `download save` should infer a better filename extension from Content-Type when the URL lacks one.
#[test]
#[ignore]
#[serial]
fn t431c2_download_save_infers_extension_from_content_type_for_extensionless_urls() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body>asset save inferred extension</body></html>",
        ),
        ("/assets/noext", "image/webp", "WEBP"),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{home}/assets-noext.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "fields": {
                "items": [
                    { "note_id": "alpha", "image_url": server.url_for("/assets/noext") }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    let output_dir = format!("{home}/saved-noext");

    let saved = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "save",
                "--file",
                &source_path,
                "--input-field",
                "fields.items",
                "--url-field",
                "image_url",
                "--name-field",
                "note_id",
                "--output-dir",
                &output_dir,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        saved["data"]["result"]["summary"]["saved_count"], 1,
        "{saved}"
    );
    assert!(
        Path::new(&format!("{output_dir}/alpha.webp")).exists(),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/alpha.webp")).unwrap(),
        "WEBP"
    );

    cleanup(&home);
}

/// T431c3: `download save` should auto-detect canonical batch roots and accept href as a default URL field.
#[test]
#[ignore]
#[serial]
fn t431c3_download_save_auto_detects_canonical_batch_root_and_href_rows() {
    let home = unique_home();
    cleanup(&home);
    std::fs::create_dir_all(&home).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body>root</body></html>",
        ),
        ("/img/a.webp", "image/webp", "asset-a"),
    ]);

    let source_path = format!("{home}/download-save-autodetect.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "data": {
                "result": {
                    "items": [
                        { "href": format!("{}/img/a.webp", server.url()), "title": "alpha" }
                    ],
                    "item_count": 1
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let output_dir = format!("{home}/saved-assets");
    let saved = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "save",
                "--file",
                &source_path,
                "--output-dir",
                &output_dir,
                "--name-field",
                "title",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        saved["data"]["result"]["summary"]["saved_count"], 1,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["entries"][0]["status"],
        serde_json::json!("saved"),
        "{saved}"
    );

    let files = list_output_files(Path::new(&output_dir));
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].ends_with("alpha.webp"),
        "expected saved filename to use inferred extension: {files:?}"
    );

    cleanup(&home);
}

/// T431c5: `download save` should resolve relative asset URLs when `--base-url` is provided.
#[test]
#[ignore]
#[serial]
fn t431c5_download_save_resolves_relative_urls_with_base_url() {
    let home = unique_home();
    cleanup(&home);
    std::fs::create_dir_all(&home).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body>root</body></html>",
        ),
        ("/img/a.webp", "image/webp", "asset-a"),
    ]);

    let source_path = format!("{home}/download-save-relative.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "data": {
                "result": {
                    "items": [
                        { "image_url": "img/a.webp", "title": "alpha" }
                    ],
                    "item_count": 1
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let output_dir = format!("{home}/saved-relative-assets");
    let saved = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "save",
                "--file",
                &source_path,
                "--output-dir",
                &output_dir,
                "--url-field",
                "image_url",
                "--name-field",
                "title",
                "--base-url",
                &server.url(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        saved["data"]["result"]["summary"]["saved_count"], 1,
        "{saved}"
    );
    assert!(
        Path::new(&format!("{output_dir}/alpha.webp")).exists(),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/alpha.webp")).unwrap(),
        "asset-a"
    );

    cleanup(&home);
}

/// T431c4: `download save` should keep final output names unique when extension inference collides with an existing planned name.
#[test]
#[ignore]
#[serial]
fn t431c4_download_save_preserves_unique_names_after_extension_inference() {
    let home = unique_home();
    cleanup(&home);
    std::fs::create_dir_all(&home).unwrap();

    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body>root</body></html>",
        ),
        ("/img/noext", "image/webp", "asset-a"),
        ("/img/alpha.webp", "image/webp", "asset-b"),
    ]);

    let source_path = format!("{home}/download-save-collision.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "data": {
                "fields": {
                    "items": [
                        { "href": format!("{}/img/noext", server.url()), "title": "alpha" },
                        { "href": format!("{}/img/alpha.webp", server.url()), "title": "alpha.webp" }
                    ]
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let output_dir = format!("{home}/saved-assets-collision");
    let saved = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "save",
                "--file",
                &source_path,
                "--output-dir",
                &output_dir,
                "--name-field",
                "title",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        saved["data"]["result"]["summary"]["saved_count"], 2,
        "{saved}"
    );

    let files = list_output_files(Path::new(&output_dir));
    assert_eq!(
        files,
        vec!["alpha-2.webp".to_string(), "alpha.webp".to_string()],
        "{files:?}"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/alpha.webp")).unwrap(),
        "asset-b"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/alpha-2.webp")).unwrap(),
        "asset-a"
    );

    cleanup(&home);
}

/// T431d: `download save` should truthfully report skips and fetch failures without hiding partial success.
#[test]
#[ignore]
#[serial]
fn t431d_download_save_reports_partial_failures_and_skips_existing() {
    let home = unique_home();
    cleanup(&home);
    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            "<!DOCTYPE html><html><body>asset save partial</body></html>",
        ),
        ("/assets/a.jpg", "image/jpeg", "AAA"),
    ]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let source_path = format!("{home}/assets-partial.json");
    std::fs::write(
        &source_path,
        serde_json::json!({
            "fields": {
                "items": [
                    { "note_id": "alpha", "image_url": server.url_for("/assets/a.jpg") },
                    { "note_id": "gamma", "image_url": server.url_for("/assets/missing.jpg") }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();
    let output_dir = format!("{home}/saved-partial");
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(format!("{output_dir}/alpha.jpg"), "OLD").unwrap();

    let saved = parse_json(
        &rub_cmd(&home)
            .args([
                "download",
                "save",
                "--file",
                &source_path,
                "--input-field",
                "fields.items",
                "--url-field",
                "image_url",
                "--name-field",
                "note_id",
                "--output-dir",
                &output_dir,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        saved["data"]["result"]["summary"]["complete"], true,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["summary"]["saved_count"], 0,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["summary"]["skipped_existing_count"], 1,
        "{saved}"
    );
    assert_eq!(
        saved["data"]["result"]["summary"]["failed_count"], 1,
        "{saved}"
    );
    let assets = saved["data"]["result"]["entries"]
        .as_array()
        .expect("assets");
    assert_eq!(assets[0]["status"], "skipped_existing", "{saved}");
    assert_eq!(assets[1]["status"], "failed", "{saved}");
    assert!(
        assets[1]["error"]
            .as_str()
            .is_some_and(|error| error.contains("http_status:404")),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{output_dir}/alpha.jpg")).unwrap(),
        "OLD"
    );

    cleanup(&home);
}

/// T432: `download cancel` should cancel a managed in-progress download.
#[test]
#[ignore]
#[serial]
fn t432_download_cancel_marks_canceled() {
    let home = unique_home();
    cleanup(&home);
    let server = DownloadFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#download-slow"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let guid = {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let downloads = parse_json(&rub_cmd(&home).arg("downloads").output().unwrap());
            assert_eq!(downloads["success"], true, "{downloads}");
            if let Some(guid) = downloads["data"]["result"]["last_download"]["guid"].as_str() {
                break guid.to_string();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "download guid did not appear in registry: {downloads}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let canceled = parse_json(
        &rub_cmd(&home)
            .args(["download", "cancel", &guid])
            .output()
            .unwrap(),
    );
    assert_eq!(canceled["success"], true, "{canceled}");
    assert_eq!(
        canceled["data"]["result"]["download"]["state"], "canceled",
        "{canceled}"
    );

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "downloads"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert!(
        runtime["data"]["runtime"]["completed_downloads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["guid"].as_str() == Some(guid.as_str())
                && entry["state"] == "canceled"),
        "{runtime}"
    );

    cleanup(&home);
}

/// T433: `runtime downloads` should project the canonical download registry after a completed managed download.
#[test]
#[ignore]
#[serial]
fn t433_runtime_downloads_projects_registry() {
    let home = unique_home();
    cleanup(&home);
    let server = DownloadFixtureServer::start();

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let clicked = parse_json(
        &rub_cmd(&home)
            .args(["click", "--selector", "#download-fast"])
            .output()
            .unwrap(),
    );
    assert_eq!(clicked["success"], true, "{clicked}");

    let waited = parse_json(
        &rub_cmd(&home)
            .args(["download", "wait", "--state", "completed"])
            .output()
            .unwrap(),
    );
    assert_eq!(waited["success"], true, "{waited}");

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "downloads"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "active", "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["mode"], "managed", "{runtime}");
    assert_eq!(
        runtime["data"]["result"]["last_download"]["guid"],
        waited["data"]["result"]["download"]["guid"],
        "{runtime}"
    );
    assert!(
        runtime["data"]["runtime"]["completed_downloads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["guid"] == runtime["data"]["result"]["last_download"]["guid"]),
        "{runtime}"
    );

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        doctor["data"]["runtime"]["last_download"]["guid"],
        runtime["data"]["result"]["last_download"]["guid"],
        "{doctor}"
    );

    cleanup(&home);
}

/// T434: `inspect storage` and `storage get` should expose current-origin storage across areas.
#[test]
#[ignore]
#[serial]
fn t434_inspect_storage_returns_current_origin_snapshot() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Storage Fixture</title></head>
<body><h1>Storage Fixture</h1></body>
</html>"#,
    )]);

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    let local_set = parse_json(
        &rub_cmd(&home)
            .args(["storage", "set", "token", "abc", "--area", "local"])
            .output()
            .unwrap(),
    );
    assert_eq!(local_set["success"], true, "{local_set}");

    let session_set = parse_json(
        &rub_cmd(&home)
            .args(["storage", "set", "token", "xyz", "--area", "session"])
            .output()
            .unwrap(),
    );
    assert_eq!(session_set["success"], true, "{session_set}");

    let inspected = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "storage"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["subject"]["origin"],
        server.url().trim_end_matches('/'),
        "{inspected}"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["local_storage"],
        json!({ "token": "abc" }),
        "{inspected}"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["session_storage"],
        json!({ "token": "xyz" }),
        "{inspected}"
    );
    assert_eq!(
        inspected["data"]["runtime"]["status"], "active",
        "{inspected}"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["local_storage"],
        json!({ "token": "abc" }),
        "{inspected}"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["session_storage"],
        json!({ "token": "xyz" }),
        "{inspected}"
    );

    let local_only = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "storage", "--area", "local"])
            .output()
            .unwrap(),
    );
    assert_eq!(local_only["success"], true, "{local_only}");
    assert_eq!(
        local_only["data"]["subject"]["area"], "local",
        "{local_only}"
    );
    assert_eq!(
        local_only["data"]["result"]["entries"],
        json!({ "token": "abc" })
    );
    assert_eq!(
        local_only["data"]["result"]["snapshot"]["local_storage"],
        json!({ "token": "abc" }),
        "{local_only}"
    );

    let get_key = parse_json(
        &rub_cmd(&home)
            .args(["storage", "get", "token"])
            .output()
            .unwrap(),
    );
    assert_eq!(get_key["success"], true, "{get_key}");
    assert_eq!(get_key["data"]["subject"]["key"], "token", "{get_key}");
    assert_eq!(
        get_key["data"]["result"]["matches"],
        json!([
            { "area": "local", "value": "abc" },
            { "area": "session", "value": "xyz" }
        ]),
        "{get_key}"
    );
    assert_eq!(
        get_key["data"]["result"]["snapshot"]["local_storage"],
        json!({ "token": "abc" }),
        "{get_key}"
    );
    assert_eq!(
        get_key["data"]["result"]["snapshot"]["session_storage"],
        json!({ "token": "xyz" }),
        "{get_key}"
    );

    let runtime = parse_json(
        &rub_cmd(&home)
            .args(["runtime", "storage"])
            .output()
            .unwrap(),
    );
    assert_eq!(runtime["success"], true, "{runtime}");
    assert_eq!(runtime["data"]["runtime"]["status"], "active", "{runtime}");
    assert_eq!(
        runtime["data"]["recent_mutations"]
            .as_array()
            .expect("recent mutations")
            .len(),
        2,
        "{runtime}"
    );

    cleanup(&home);
}

/// T435: `storage export` + `storage import` should round-trip the current origin snapshot.
#[test]
#[ignore]
#[serial]
fn t435_storage_export_import_round_trip() {
    let home = unique_home();
    cleanup(&home);

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>Storage Export Fixture</title></head>
<body><h1>Storage Export Fixture</h1></body>
</html>"#,
    )]);
    let export_path = format!("{home}/storage-export.json");

    let opened = parse_json(
        &rub_cmd(&home)
            .args(["open", &server.url()])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["storage", "set", "theme", "dark", "--area", "local"])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["storage", "set", "csrf", "def", "--area", "session"])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let exported = parse_json(
        &rub_cmd(&home)
            .args(["storage", "export", "--path", &export_path])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(
        exported["data"]["artifact"]["path"], export_path,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["artifact"]["direction"], "output",
        "{exported}"
    );
    assert!(Path::new(&export_path).exists(), "{exported}");

    let cleared = parse_json(&rub_cmd(&home).args(["storage", "clear"]).output().unwrap());
    assert_eq!(cleared["success"], true, "{cleared}");

    let empty = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "storage"])
            .output()
            .unwrap(),
    );
    assert_eq!(empty["success"], true, "{empty}");
    assert_eq!(
        empty["data"]["result"]["snapshot"]["local_storage"],
        json!({}),
        "{empty}"
    );
    assert_eq!(
        empty["data"]["result"]["snapshot"]["session_storage"],
        json!({}),
        "{empty}"
    );

    let imported = parse_json(
        &rub_cmd(&home)
            .args(["storage", "import", &export_path])
            .output()
            .unwrap(),
    );
    assert_eq!(imported["success"], true, "{imported}");
    assert_eq!(
        imported["data"]["artifact"]["path"], export_path,
        "{imported}"
    );
    assert_eq!(
        imported["data"]["artifact"]["direction"], "input",
        "{imported}"
    );
    assert_eq!(imported["data"]["result"]["imported"], true, "{imported}");

    let inspected = parse_json(
        &rub_cmd(&home)
            .args(["inspect", "storage"])
            .output()
            .unwrap(),
    );
    assert_eq!(inspected["success"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["local_storage"],
        json!({ "theme": "dark" }),
        "{inspected}"
    );
    assert_eq!(
        inspected["data"]["result"]["snapshot"]["session_storage"],
        json!({ "csrf": "def" }),
        "{inspected}"
    );

    cleanup(&home);
}

/// T436: `storage import` should reject snapshots captured from a different origin.
#[test]
#[ignore]
#[serial]
fn t436_storage_import_rejects_origin_mismatch() {
    let home = unique_home();
    cleanup(&home);

    let (_rt_a, server_a) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><h1>Origin A</h1></body></html>"#,
    )]);
    let (_rt_b, server_b) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><body><h1>Origin B</h1></body></html>"#,
    )]);
    let export_path = format!("{home}/storage-origin-a.json");

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["open", &server_a.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["storage", "set", "token", "abc", "--area", "local"])
                .output()
                .unwrap()
        )["success"],
        true
    );
    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["storage", "export", "--path", &export_path])
                .output()
                .unwrap()
        )["success"],
        true
    );

    assert_eq!(
        parse_json(
            &rub_cmd(&home)
                .args(["open", &server_b.url()])
                .output()
                .unwrap()
        )["success"],
        true
    );

    let imported = parse_json(
        &rub_cmd(&home)
            .args(["storage", "import", &export_path])
            .output()
            .unwrap(),
    );
    assert_eq!(imported["success"], false, "{imported}");
    assert_eq!(imported["error"]["code"], "INVALID_INPUT", "{imported}");
    assert!(
        imported["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("origin mismatch"),
        "{imported}"
    );

    cleanup(&home);
}
