use super::*;
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;

fn external_cdp_http_request(cdp_origin: &str, method: &str, path: &str) -> Option<String> {
    use std::io::{ErrorKind, Read};

    let addr = cdp_origin.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).ok()?;
    let timeout = Duration::from_secs(2);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                break;
            }
            Err(_) => return None,
        }
    }
    let response = String::from_utf8_lossy(&response).into_owned();
    if !response.contains(" 200 ") {
        return None;
    }
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
}

fn external_cdp_page_target_id(cdp_origin: &str, title: &str) -> Option<String> {
    let body = external_cdp_http_request(cdp_origin, "GET", "/json/list")?;
    let targets: serde_json::Value = serde_json::from_str(&body).ok()?;
    targets.as_array()?.iter().find_map(|target| {
        (target["type"].as_str() == Some("page") && target["title"].as_str() == Some(title))
            .then(|| target["id"].as_str().map(str::to_string))
            .flatten()
    })
}

fn open_external_cdp_page(cdp_origin: &str, url: &str) -> Option<String> {
    let body = external_cdp_http_request(
        cdp_origin,
        "PUT",
        &format!("/json/new?{}", percent_encode_url_component(url)),
    )?;
    let target: serde_json::Value = serde_json::from_str(&body).ok()?;
    target["id"].as_str().map(str::to_string)
}

fn percent_encode_url_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn activate_external_cdp_page(cdp_origin: &str, target_id: &str) -> bool {
    external_cdp_http_request(cdp_origin, "PUT", &format!("/json/activate/{target_id}")).is_some()
}

fn ensure_external_cdp_page_active(cdp_origin: &str, url: &str, title: &str, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_target_id = None;
    while std::time::Instant::now() < deadline {
        let target_id = external_cdp_page_target_id(cdp_origin, title)
            .or_else(|| open_external_cdp_page(cdp_origin, url));
        if let Some(target_id) = target_id {
            last_target_id = Some(target_id.clone());
            if activate_external_cdp_page(cdp_origin, &target_id) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out preparing external CDP page authority for title {title}; last_target_id={last_target_id:?}"
    );
}

fn inject_replay_failing_batch_close_session(
    home: &str,
    session_name: &str,
) -> (String, PathBuf, JoinHandle<()>) {
    let session_id = format!("sess-{session_name}");
    let runtime = rub_daemon::rub_paths::RubPaths::new(home).session_runtime(session_name, &session_id);
    let socket_path = runtime.actual_socket_path();
    let _ = std::fs::remove_file(&socket_path);
    std::fs::create_dir_all(
        socket_path
            .parent()
            .expect("replay-failing fake socket should have parent"),
    )
    .expect("create replay-failing fake socket parent");
    let listener = StdUnixListener::bind(&socket_path).expect("bind replay-failing fake socket");
    let session_id_for_server = session_id.clone();
    let socket_path_for_server = socket_path.clone();
    let server = std::thread::spawn(move || {
        loop {
            let (mut stream, _) = listener
                .accept()
                .expect("accept replay-failing fake daemon connection");
            let mut reader = BufReader::new(
                stream
                    .try_clone()
                    .expect("clone fake daemon stream for read"),
            );
            let request: rub_ipc::protocol::IpcRequest =
                rub_ipc::codec::NdJsonCodec::read_blocking(&mut reader)
                    .expect("read fake daemon request")
                    .expect("fake daemon request frame");
            match request.command.as_str() {
                "_handshake" => {
                    let response = rub_ipc::protocol::IpcResponse::success(
                        "handshake",
                        serde_json::json!({
                            "daemon_session_id": session_id_for_server,
                            "launch_policy": {
                                "headless": true,
                                "ignore_cert_errors": false,
                                "hide_infobars": false
                            }
                        }),
                    )
                    .with_command_id(
                        request
                            .command_id
                            .clone()
                            .expect("handshake probe must carry command_id"),
                    )
                    .expect("probe command_id must be valid")
                    .with_daemon_session_id(&session_id_for_server)
                    .expect("daemon_session_id must be valid");
                    stream
                        .write_all(
                            &rub_ipc::codec::NdJsonCodec::encode(&response)
                                .expect("encode fake handshake response"),
                        )
                        .expect("write fake handshake response");
                    stream.flush().expect("flush fake handshake response");
                }
                "close" => {
                    std::thread::sleep(Duration::from_millis(5_000));
                    drop(reader);
                    drop(stream);
                    let _ = std::fs::remove_file(&socket_path_for_server);
                    break;
                }
                other => panic!("unexpected fake daemon command: {other} ({request:?})"),
            }
        }
    });

    std::fs::create_dir_all(runtime.session_dir())
        .expect("create replay-failing runtime session dir");
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup committed parent for replay-failing session"),
    )
    .expect("create replay-failing startup committed parent");
    std::fs::write(runtime.pid_path(), std::process::id().to_string())
        .expect("write replay-failing runtime pid path");
    std::fs::write(runtime.startup_committed_path(), &session_id)
        .expect("write replay-failing startup committed marker");

    let mut registry = rub_daemon::session::read_registry(std::path::Path::new(home))
        .expect("read live registry for replay-failing batch close injection");
    registry.sessions.push(rub_daemon::session::RegistryEntry {
        session_id: session_id.clone(),
        session_name: session_name.to_string(),
        pid: std::process::id(),
        socket_path: socket_path.display().to_string(),
        created_at: "2026-04-23T00:00:00Z".to_string(),
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    });
    rub_daemon::session::write_registry(std::path::Path::new(home), &registry)
        .expect("write registry with replay-failing batch close session");

    (session_id, socket_path, server)
}

fn assert_session_post_commit_recovery_contract(
    recovery_contract: &serde_json::Value,
    home: &str,
    session_name: &str,
    session_id: &str,
    debug: &serde_json::Value,
) {
    assert_eq!(
        recovery_contract["kind"],
        "session_post_commit_journal",
        "{debug}"
    );
    assert_eq!(
        recovery_contract["scope"],
        "daemon_rollover_recovery",
        "{debug}"
    );
    assert_eq!(
        recovery_contract["session_name"],
        session_name,
        "{debug}"
    );
    assert_eq!(
        recovery_contract["daemon_session_id"],
        session_id,
        "{debug}"
    );
    assert_eq!(
        recovery_contract["reader_contract"],
        "ndjson_post_commit_journal",
        "{debug}"
    );
    assert_eq!(
        recovery_contract["committed_truth_may_exist"],
        true,
        "{debug}"
    );
    assert_eq!(
        recovery_contract["safe_to_rerun_with_new_command_id"],
        false,
        "{debug}"
    );
    assert_eq!(
        recovery_contract["journal_path"],
        rub_daemon::rub_paths::RubPaths::new(home)
            .session_runtime(session_name, session_id)
            .post_commit_journal_path()
            .display()
            .to_string(),
        "{debug}"
    );
}

// ── v1.1: US6 Cookie Management ─────────────────────────────────────

/// T056a-T056c: cookie round-trip and clear flows should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t250_252_cookies_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![(
        "/",
        "text/html",
        r#"<!DOCTYPE html><html><head><title>Cookie Fixture</title></head><body>ok</body></html>"#,
    )]);
    let ip_url = server.url();
    let localhost_url = ip_url.replace("127.0.0.1", "localhost");

    let opened = parse_json(&session.cmd().args(["open", &ip_url]).output().unwrap());
    assert_eq!(opened["success"], true, "{opened}");

    let set_cookie = parse_json(
        &session
            .cmd()
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
            .unwrap(),
    );
    assert_eq!(set_cookie["success"], true, "{set_cookie}");

    let got = parse_json(&session.cmd().args(["cookies", "get"]).output().unwrap());
    assert_eq!(got["success"], true, "{got}");
    assert_eq!(got["data"]["subject"]["kind"], "cookies", "{got}");
    let cookies = got["data"]["result"]["cookies"].as_array().unwrap();
    assert!(cookies.iter().any(|c| {
        c["name"] == "test_cookie"
            && c["value"] == "abc123"
            && c["same_site"] == "Lax"
            && c["expires"].is_number()
    }));

    let set_temp = parse_json(
        &session
            .cmd()
            .args(["cookies", "set", "temp", "val"])
            .output()
            .unwrap(),
    );
    assert_eq!(set_temp["success"], true, "{set_temp}");

    let cleared_all = parse_json(&session.cmd().args(["cookies", "clear"]).output().unwrap());
    assert_eq!(cleared_all["success"], true, "{cleared_all}");
    let after_clear = parse_json(&session.cmd().args(["cookies", "get"]).output().unwrap());
    assert_eq!(
        after_clear["data"]["result"]["cookies"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        0,
        "{after_clear}"
    );

    let reopened_ip = parse_json(&session.cmd().args(["open", &ip_url]).output().unwrap());
    assert_eq!(reopened_ip["success"], true, "{reopened_ip}");
    let set_ip = parse_json(
        &session
            .cmd()
            .args(["cookies", "set", "ip_cookie", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(set_ip["success"], true, "{set_ip}");

    let opened_localhost = parse_json(
        &session
            .cmd()
            .args(["open", &localhost_url])
            .output()
            .unwrap(),
    );
    assert_eq!(opened_localhost["success"], true, "{opened_localhost}");
    let set_host = parse_json(
        &session
            .cmd()
            .args(["cookies", "set", "host_cookie", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(set_host["success"], true, "{set_host}");

    let ip_cookies = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
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

    let cleared_scoped = parse_json(
        &session
            .cmd()
            .args(["cookies", "clear", "--url", &ip_url])
            .output()
            .unwrap(),
    );
    assert_eq!(cleared_scoped["success"], true, "{cleared_scoped}");
    assert_eq!(
        cleared_scoped["data"]["subject"]["url"], ip_url,
        "{cleared_scoped}"
    );

    let reopened_localhost = parse_json(
        &session
            .cmd()
            .args(["open", &localhost_url])
            .output()
            .unwrap(),
    );
    assert_eq!(reopened_localhost["success"], true, "{reopened_localhost}");
    let host_cookies = parse_json(&session.cmd().args(["cookies", "get"]).output().unwrap());
    let host_names = host_cookies["data"]["result"]["cookies"]
        .as_array()
        .unwrap();
    assert!(
        host_names
            .iter()
            .any(|cookie| cookie["name"] == "host_cookie")
    );

    let reopened_ip_again = parse_json(&session.cmd().args(["open", &ip_url]).output().unwrap());
    assert_eq!(reopened_ip_again["success"], true, "{reopened_ip_again}");
    let ip_cookies = parse_json(&session.cmd().args(["cookies", "get"]).output().unwrap());
    let ip_names = ip_cookies["data"]["result"]["cookies"].as_array().unwrap();
    assert!(!ip_names.iter().any(|cookie| cookie["name"] == "ip_cookie"));
}

/// T021e: standalone wait holds the FIFO request fence while polling.
#[test]
#[ignore]
#[serial]
fn t214_wait_holds_queue_request_fence() {
    let session = ManagedBrowserSession::new();
    let home = session.home().to_string();

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
    let queued = parse_json(
        &rub_cmd(&home)
            .args(["--timeout", "200", "get", "title"])
            .output()
            .unwrap(),
    );
    assert_eq!(queued["success"], false, "{queued}");
    assert_eq!(queued["error"]["code"], "IPC_TIMEOUT", "{queued}");
    assert_eq!(queued["error"]["context"]["phase"], "queue", "{queued}");
    assert_eq!(queued["error"]["context"]["command"], "get", "{queued}");

    let wait_json = parse_json(&waiter.join().unwrap());
    assert_eq!(wait_json["success"], true);
    assert_eq!(wait_json["data"]["result"]["matched"], true);
}

#[test]
#[ignore]
#[serial]
fn t215_concurrent_first_command_serializes_startup() {
    let home = unique_home();
    prepare_home(&home);

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

    let _ = std::fs::remove_dir_all(&home);
}

/// T030d: a popup tab appears in `tabs` and can be switched to.
#[test]
#[ignore]
#[serial]
fn t223_popup_tab_lifecycle() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

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

    session
        .cmd()
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let state_json = run_state(home);
    let snap_id = snapshot_id(&state_json);
    let popup_button = find_element_index(&state_json, |element| {
        element["text"].as_str() == Some("Open Popup")
    });

    let out = session
        .cmd()
        .args(["click", &popup_button.to_string(), "--snapshot", &snap_id])
        .output()
        .unwrap();
    let clicked = parse_json(&out);
    assert_eq!(clicked["success"], false, "{clicked}");
    assert_eq!(
        clicked["error"]["code"],
        "INTERACTION_NOT_CONFIRMED",
        "{clicked}"
    );
    assert_eq!(
        clicked["error"]["context"]["committed_response_projection"]["interaction"]["interference"]
            ["after"]["current_interference"]["kind"],
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
    assert_eq!(tabs_json["success"], true);
    assert!(
        tabs_json["data"]["result"]["items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default()
            >= 2
    );
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
    let popup_index = tabs_json["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["title"].as_str() == Some("Popup Target"))
        .and_then(|tab| tab["index"].as_u64())
        .unwrap()
        .to_string();

    let out = session
        .cmd()
        .args(["switch", &popup_index])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(
        json["data"]["result"]["active_tab"]["title"],
        "Popup Target"
    );
}

/// T072a-T072e: state a11y and observe projections should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t233a_d_state_a11y_and_observe_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home();

    let (_rt, server) = start_test_server(vec![
        (
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
        ),
        (
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
        ),
        (
            "/a11y-format-diff",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>Format Diff Fixture</title></head><body><button>Go</button></body></html>"#,
        ),
        (
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
        ),
    ]);

    rub_cmd(home)
        .args(["open", &server.url_for("/a11y")])
        .output()
        .unwrap();

    let base_state = run_state(home);
    let a11y_state = parse_json(&rub_cmd(home).args(["state", "--a11y"]).output().unwrap());

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

    rub_cmd(home)
        .args(["open", &server.url_for("/a11y-format")])
        .output()
        .unwrap();

    let out = rub_cmd(home)
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

    rub_cmd(home)
        .args(["open", &server.url_for("/a11y-format-diff")])
        .output()
        .unwrap();
    let state = run_state(home);
    let snapshot = snapshot_id(&state);

    let out = rub_cmd(home)
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

    rub_cmd(home)
        .args(["open", &server.url_for("/observe")])
        .output()
        .unwrap();

    let screenshot_path = format!("{home}/observe.png");
    let out = rub_cmd(home)
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
    assert_eq!(
        json["data"]["artifact"]["artifact_state"]["truth_level"],
        "command_artifact"
    );
    assert_eq!(
        json["data"]["artifact"]["artifact_state"]["artifact_authority"],
        "router.observe_capture_artifact"
    );
    assert_eq!(
        json["data"]["artifact"]["artifact_state"]["upstream_truth"],
        "observe_capture_artifact"
    );
    assert_eq!(
        json["data"]["artifact"]["artifact_state"]["durability"],
        "durable"
    );
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
}

/// T233e-T233i: history/export flows should share one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t233e_i_history_export_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
            "/history",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>History Fixture</title></head><body>ok</body></html>"#,
        ),
        (
            "/history-export",
            "text/html",
            r#"<!DOCTYPE html><html><body><input id="name" value=""><button id="apply">Apply</button><div id="status">idle</div><script>document.getElementById('apply').addEventListener('click',()=>{document.getElementById('status').textContent=document.getElementById('name').value||'idle';});</script></body></html>"#,
        ),
        (
            "/history-observe",
            "text/html",
            r#"<!DOCTYPE html><html><body><div id="status">ok</div></body></html>"#,
        ),
        (
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
        ),
        (
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
        ),
    ]);

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", &server.url_for("/history")])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");
    let opened_registry_entry =
        rub_daemon::session::read_registry(std::path::Path::new(session.home()))
            .unwrap_or_else(|error| {
                panic!(
                    "registry should be readable for {}: {error}",
                    session.home()
                )
            })
            .sessions
            .into_iter()
            .find(|entry| entry.session_name == "default")
            .unwrap_or_else(|| panic!("default session should be present for {}", session.home()));
    let executed = parse_json(
        &session
            .cmd()
            .args(["exec", "document.title"])
            .output()
            .unwrap(),
    );
    assert_eq!(executed["success"], true, "{executed}");
    assert_eq!(executed["data"]["result"], "History Fixture", "{executed}");
    let executed_registry_entry =
        rub_daemon::session::read_registry(std::path::Path::new(session.home()))
            .unwrap_or_else(|error| {
                panic!(
                    "registry should stay readable for {}: {error}",
                    session.home()
                )
            })
            .sessions
            .into_iter()
            .find(|entry| entry.session_name == "default")
            .unwrap_or_else(|| {
                panic!(
                    "default session should remain present for {}",
                    session.home()
                )
            });
    assert_eq!(
        executed_registry_entry.session_id, opened_registry_entry.session_id,
        "{executed}"
    );
    assert_eq!(
        executed_registry_entry.socket_path, opened_registry_entry.socket_path,
        "{executed}"
    );
    assert_eq!(
        executed_registry_entry.pid, opened_registry_entry.pid,
        "{executed}"
    );
    let daemon_pids_after_exec = daemon_processes_for_home(session.home());
    assert_eq!(
        daemon_pids_after_exec.len(),
        1,
        "{daemon_pids_after_exec:?}"
    );
    let exec_journal_path = RubPaths::new(session.home())
        .session_runtime("default", &executed_registry_entry.session_id)
        .post_commit_journal_path();
    let exec_journal_entries = std::fs::read_to_string(&exec_journal_path)
        .unwrap_or_else(|error| {
            panic!(
                "journal should be readable at {:?}: {error}",
                exec_journal_path
            )
        })
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                panic!("journal line should decode as json: {error}: {line}")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        exec_journal_entries
            .last()
            .and_then(|entry| entry.get("command"))
            .and_then(|value| value.as_str()),
        Some("exec"),
        "{exec_journal_entries:?}"
    );

    let history = parse_json(
        &session
            .cmd()
            .args(["history", "--last", "2"])
            .output()
            .unwrap(),
    );
    assert_eq!(history["success"], true, "{history}");
    let entries = history["data"]["result"]["items"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{history}");
    assert_eq!(entries[0]["command"], "open", "{history}");
    assert_eq!(entries[0]["success"], true, "{history}");
    assert_eq!(entries[1]["command"], "exec", "{history}");
    assert_eq!(entries[1]["summary"], "success", "{history}");

    session
        .cmd()
        .args(["open", &server.url_for("/history-export")])
        .output()
        .unwrap();
    session
        .cmd()
        .args(["type", "--selector", "#name", "Ada"])
        .output()
        .unwrap();
    session
        .cmd()
        .args(["click", "--selector", "#apply"])
        .output()
        .unwrap();
    session.cmd().args(["observe"]).output().unwrap();

    let exported = parse_json(
        &session
            .cmd()
            .args(["history", "--last", "4", "--export-pipe"])
            .output()
            .unwrap(),
    );
    assert_eq!(exported["success"], true, "{exported}");
    assert_eq!(exported["data"]["result"]["format"], "pipe", "{exported}");
    assert_eq!(
        exported["data"]["result"]["projection_state"]["surface"], "workflow_capture_export",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["projection_kind"],
        "bounded_post_commit_projection",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["truth_level"], "operator_projection",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["projection_authority"],
        "session.workflow_capture",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["upstream_commit_truth"],
        "daemon_response_committed",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["control_role"], "display_only",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["durability"], "best_effort",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["projection_state"]["lossy"], false,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["entries"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        3,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["entries"][0]["command"], "open",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["entries"][1]["command"], "type",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["entries"][2]["command"], "click",
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["skipped"]["observation"], 1,
        "{exported}"
    );
    assert_eq!(
        exported["data"]["result"]["entries"][1]["args"]["text"], "Ada",
        "{exported}"
    );

    let failing_output_dir = std::env::temp_dir().join(format!(
        "rub-history-export-followup-failure-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&failing_output_dir).unwrap();
    let post_commit_output = session
        .cmd()
        .args([
            "history",
            "--last",
            "2",
            "--export-pipe",
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
        post_commit_failure["error"]["context"]["reason"], "post_commit_history_export_failed",
        "{post_commit_failure}"
    );
    assert_eq!(
        post_commit_failure["error"]["context"]["daemon_request_committed"], true,
        "{post_commit_failure}"
    );
    assert_eq!(
        post_commit_failure["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["surface"],
        "workflow_capture_export",
        "{post_commit_failure}"
    );
    assert!(
        post_commit_failure["data"].is_null(),
        "{post_commit_failure}"
    );

    let spec =
        serde_json::to_string(exported["data"]["result"]["steps"].as_array().unwrap()).unwrap();

    session
        .cmd()
        .args(["open", &server.url_for("/history-observe")])
        .output()
        .unwrap();
    let observe_registry_entry =
        rub_daemon::session::read_registry(std::path::Path::new(session.home()))
            .unwrap_or_else(|error| {
                panic!(
                    "registry should be readable for {}: {error}",
                    session.home()
                )
            })
            .sessions
            .into_iter()
            .find(|entry| entry.session_name == "default")
            .unwrap_or_else(|| panic!("default session should be present for {}", session.home()));
    let observed = parse_json(
        &session
            .cmd()
            .args(["get", "text", "--selector", "#status"])
            .output()
            .unwrap(),
    );
    assert_eq!(observed["success"], true, "{observed}");
    assert_eq!(observed["data"]["result"]["value"], "ok", "{observed}");
    let observed_registry_entry =
        rub_daemon::session::read_registry(std::path::Path::new(session.home()))
            .unwrap_or_else(|error| {
                panic!(
                    "registry should stay readable for {}: {error}",
                    session.home()
                )
            })
            .sessions
            .into_iter()
            .find(|entry| entry.session_name == "default")
            .unwrap_or_else(|| {
                panic!(
                    "default session should remain present for {}",
                    session.home()
                )
            });
    assert_eq!(
        observed_registry_entry.session_id, observe_registry_entry.session_id,
        "{observed}"
    );
    assert_eq!(
        observed_registry_entry.socket_path, observe_registry_entry.socket_path,
        "{observed}"
    );
    assert_eq!(
        observed_registry_entry.pid, observe_registry_entry.pid,
        "{observed}"
    );
    let daemon_pids_after_get = daemon_processes_for_home(session.home());
    assert_eq!(daemon_pids_after_get.len(), 1, "{daemon_pids_after_get:?}");
    let journal_path = RubPaths::new(session.home())
        .session_runtime("default", &observed_registry_entry.session_id)
        .post_commit_journal_path();
    let journal_entries = std::fs::read_to_string(&journal_path)
        .unwrap_or_else(|error| panic!("journal should be readable at {:?}: {error}", journal_path))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                panic!("journal line should decode as json: {error}: {line}")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        journal_entries
            .last()
            .and_then(|entry| entry.get("command"))
            .and_then(|value| value.as_str()),
        Some("get"),
        "{journal_entries:?}"
    );

    let observe_export = parse_json(
        &session
            .cmd()
            .args([
                "history",
                "--last",
                "2",
                "--export-pipe",
                "--include-observation",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(observe_export["success"], true, "{observe_export}");
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["surface"], "workflow_capture_export",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["projection_kind"],
        "bounded_post_commit_projection",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["truth_level"], "operator_projection",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["projection_authority"],
        "session.workflow_capture",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["upstream_commit_truth"],
        "daemon_response_committed",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["control_role"], "display_only",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["durability"], "best_effort",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["projection_state"]["lossy"], false,
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["entries"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["entries"][1]["command"], "get",
        "{observe_export}"
    );
    assert_eq!(
        observe_export["data"]["result"]["skipped"]["observation"], 0,
        "{observe_export}"
    );

    session
        .cmd()
        .args(["open", &server.url_for("/history-export-script")])
        .output()
        .unwrap();
    let script_registry_entry =
        rub_daemon::session::read_registry(std::path::Path::new(session.home()))
            .unwrap_or_else(|error| {
                panic!(
                    "registry should be readable for {}: {error}",
                    session.home()
                )
            })
            .sessions
            .into_iter()
            .find(|entry| entry.session_name == "default")
            .unwrap_or_else(|| panic!("default session should be present for {}", session.home()));
    session
        .cmd()
        .args(["type", "--selector", "#name", "--clear", "Grace Hopper"])
        .output()
        .unwrap();
    session
        .cmd()
        .args(["click", "--selector", "#apply"])
        .output()
        .unwrap();
    let clicked_registry_entry =
        rub_daemon::session::read_registry(std::path::Path::new(session.home()))
            .unwrap_or_else(|error| {
                panic!(
                    "registry should stay readable for {}: {error}",
                    session.home()
                )
            })
            .sessions
            .into_iter()
            .find(|entry| entry.session_name == "default")
            .unwrap_or_else(|| {
                panic!(
                    "default session should remain present for {}",
                    session.home()
                )
            });
    assert_eq!(
        clicked_registry_entry.session_id,
        script_registry_entry.session_id
    );
    assert_eq!(
        clicked_registry_entry.socket_path,
        script_registry_entry.socket_path
    );
    assert_eq!(clicked_registry_entry.pid, script_registry_entry.pid);
    let daemon_pids_after_click = daemon_processes_for_home(session.home());
    assert_eq!(
        daemon_pids_after_click.len(),
        1,
        "{daemon_pids_after_click:?}"
    );
    let click_journal_path = RubPaths::new(session.home())
        .session_runtime("default", &clicked_registry_entry.session_id)
        .post_commit_journal_path();
    let click_journal_entries = std::fs::read_to_string(&click_journal_path)
        .unwrap_or_else(|error| {
            panic!(
                "journal should be readable at {:?}: {error}",
                click_journal_path
            )
        })
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                panic!("journal line should decode as json: {error}: {line}")
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        click_journal_entries
            .last()
            .and_then(|entry| entry.get("command"))
            .and_then(|value| value.as_str()),
        Some("click"),
        "{click_journal_entries:?}"
    );

    let script_export = parse_json(
        &session
            .cmd()
            .args(["history", "--last", "3", "--export-script"])
            .output()
            .unwrap(),
    );
    assert_eq!(script_export["success"], true, "{script_export}");
    assert_eq!(
        script_export["data"]["result"]["format"], "script",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["surface"], "workflow_capture_export",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["projection_kind"],
        "bounded_post_commit_projection",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["truth_level"], "operator_projection",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["projection_authority"],
        "session.workflow_capture",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["upstream_commit_truth"],
        "daemon_response_committed",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["control_role"], "display_only",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["durability"], "best_effort",
        "{script_export}"
    );
    assert_eq!(
        script_export["data"]["result"]["projection_state"]["lossy"], false,
        "{script_export}"
    );
    let script = script_export["data"]["result"]["export"]["content"]
        .as_str()
        .unwrap();

    let replay_home = unique_home();
    prepare_home(&replay_home);
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

    let workflow_save_baseline =
        script_export["data"]["result"]["capture_window"]["newest_retained_sequence"]
            .as_u64()
            .expect("script export should publish newest retained sequence");

    session
        .cmd()
        .args(["open", &server.url_for("/workflow-save")])
        .output()
        .unwrap();
    session
        .cmd()
        .args(["type", "--selector", "#name", "--clear", "Ada"])
        .output()
        .unwrap();
    session
        .cmd()
        .args(["click", "--selector", "#apply"])
        .output()
        .unwrap();
    let exec = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec["success"], true, "{exec}");
    assert_eq!(exec["data"]["result"], "Ada", "{exec}");

    let saved_export_output = session
        .cmd()
        .args([
            "history",
            "--export-pipe",
            "--from",
            &(workflow_save_baseline + 1).to_string(),
            "--to",
            &(workflow_save_baseline + 4).to_string(),
            "--save-as",
            "login_flow",
        ])
        .output()
        .unwrap();
    let saved_export = parse_json(&saved_export_output);
    assert!(!saved_export_output.status.success(), "{saved_export}");
    assert_eq!(saved_export["success"], false, "{saved_export}");
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["projection_kind"],
        "bounded_post_commit_projection",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["truth_level"],
        "operator_projection",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["projection_authority"],
        "session.workflow_capture",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["upstream_commit_truth"],
        "daemon_response_committed",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["control_role"],
        "display_only",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["durability"],
        "best_effort",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["projection_state"]
            ["lossy"],
        false,
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["selection"]["from"],
        workflow_save_baseline + 1,
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["selection"]["to"],
        workflow_save_baseline + 4,
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["committed_response_projection"]["result"]["entries"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        3,
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["code"], "INVALID_INPUT",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["reason"], "post_commit_history_export_failed",
        "{saved_export}"
    );
    assert_eq!(
        saved_export["error"]["context"]["daemon_request_committed"], true,
        "{saved_export}"
    );
    assert!(saved_export["data"].is_null(), "{saved_export}");

    let saved_path = RubPaths::new(session.home())
        .workflows_dir()
        .join("login_flow.json");
    std::fs::create_dir_all(saved_path.parent().unwrap()).unwrap();
    std::fs::write(
        &saved_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "steps": saved_export["error"]["context"]["committed_response_projection"]["result"]["steps"].clone(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(saved_path.exists(), "{saved_export}");
    let saved_value: Value = serde_json::from_str(&std::fs::read_to_string(&saved_path).unwrap())
        .expect("saved workflow json");
    assert!(saved_value["steps"].is_array(), "{saved_value}");
    assert_eq!(
        saved_value["steps"].as_array().unwrap().len(),
        3,
        "{saved_value}"
    );
    assert_eq!(saved_value["steps"][0]["command"], "open", "{saved_value}");
    assert_eq!(saved_value["steps"][2]["command"], "click", "{saved_value}");

    let listed = parse_json(
        &session
            .cmd()
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
        &session
            .cmd()
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
        3,
        "{replayed}"
    );
    let replay_actual = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('status').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay_actual["success"], true, "{replay_actual}");
    assert_eq!(replay_actual["data"]["result"], "Ada", "{replay_actual}");

    let replay_home = unique_home();
    prepare_home(&replay_home);
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
}

/// T065a/T061a: select/upload interaction variants should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t260_261_select_and_upload_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let (_rt, server) = start_test_server(vec![
        (
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
        ),
        (
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
        ),
        (
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
        ),
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
        (
            "/select-disabled",
            "text/html",
            r#"<!doctype html>
<html><body>
  <select name="region" disabled>
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
</body></html>"#,
        ),
        (
            "/select-aria-disabled",
            "text/html",
            r#"<!doctype html>
<html><body>
  <select name="region" aria-disabled="true">
    <option value="CA">California</option>
    <option value="NY">New York</option>
  </select>
</body></html>"#,
        ),
        (
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
        ),
        (
            "/upload-disabled",
            "text/html",
            r#"<!doctype html>
<html><body>
  <input type="file" name="resume" disabled />
</body></html>"#,
        ),
        (
            "/upload-fieldset-disabled",
            "text/html",
            r#"<!doctype html>
<html><body>
  <fieldset disabled>
    <input type="file" name="resume" />
  </fieldset>
</body></html>"#,
        ),
    ]);

    let select_on_opened_page = |session: &ManagedBrowserSession, choice: &str| {
        let state_json = run_state(session.home());
        let snap_id = snapshot_id(&state_json);
        let select_index = find_element_index(&state_json, |element| element["tag"] == "select");
        parse_json(
            &session
                .cmd()
                .args([
                    "select",
                    &select_index.to_string(),
                    choice,
                    "--snapshot",
                    &snap_id,
                ])
                .output()
                .unwrap(),
        )
    };

    let upload_on_opened_page = |session: &ManagedBrowserSession, file_path: &str| {
        let state_json = run_state(session.home());
        let snap_id = snapshot_id(&state_json);
        let upload_index = find_element_index(&state_json, |element| {
            element["attributes"]["type"].as_str() == Some("file")
        });
        parse_json(
            &session
                .cmd()
                .args([
                    "upload",
                    &upload_index.to_string(),
                    file_path,
                    "--snapshot",
                    &snap_id,
                ])
                .output()
                .unwrap(),
        )
    };

    let open = |session: &ManagedBrowserSession, path: &str| {
        let opened = parse_json(
            &session
                .cmd()
                .args(["open", &server.url_for(path)])
                .output()
                .unwrap(),
        );
        assert_eq!(opened["success"], true, "{opened}");
    };

    open(&session, "/select");
    let json = select_on_opened_page(&session, "California");
    assert_eq!(json["success"], true, "{json}");
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
    let exec_json = parse_json(
        &session
            .cmd()
            .args(["exec", "document.querySelector('select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_json["success"], true, "{exec_json}");
    assert_eq!(exec_json["data"]["result"], "CA", "{exec_json}");

    open(&session, "/select-explicit-value");
    let json = parse_json(
        &session
            .cmd()
            .args([
                "select",
                "--selector",
                "select[name=region]",
                "--value",
                "California",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["value"], "CA", "{json}");
    assert_eq!(json["data"]["result"]["text"], "California", "{json}");
    let exec_json = parse_json(
        &session
            .cmd()
            .args(["exec", "document.querySelector('select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_json["success"], true, "{exec_json}");
    assert_eq!(exec_json["data"]["result"], "CA", "{exec_json}");

    open(&session, "/select-contradicted");
    let json = select_on_opened_page(&session, "California");
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "INTERACTION_NOT_CONFIRMED", "{json}");
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["semantic_class"],
        "select_choice"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["interaction_confirmed"],
        false
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_status"],
        "contradicted"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_kind"],
        "selection_applied"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_details"]
            ["observed"]["value"],
        "NY",
        "{json}"
    );
    let exec_json = parse_json(
        &session
            .cmd()
            .args(["exec", "document.querySelector('select').value"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_json["success"], true, "{exec_json}");
    assert_eq!(exec_json["data"]["result"], "NY", "{exec_json}");

    open(&session, "/select-degraded");
    let json = select_on_opened_page(&session, "California");
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "INTERACTION_NOT_CONFIRMED", "{json}");
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["semantic_class"],
        "select_choice"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["interaction_confirmed"],
        false
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_status"],
        "degraded"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_kind"],
        "selection_applied"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_details"]
            ["context_changed"],
        false,
        "{json}"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["confirmation_details"]
            ["after_page"]["context_replaced"],
        true,
        "{json}"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["context_turnover"]
            ["context_changed"],
        false,
        "{json}"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["context_turnover"]
            ["context_replaced"],
        true,
        "{json}"
    );
    assert_eq!(
        json["error"]["context"]["committed_response_projection"]["interaction"]["context_turnover"]
            ["context_replaced"],
        true,
        "{json}"
    );
    let title_json = parse_json(&session.cmd().args(["get", "title"]).output().unwrap());
    assert_eq!(title_json["success"], true, "{title_json}");
    assert_eq!(
        title_json["data"]["result"]["value"], "After Select",
        "{title_json}"
    );

    open(&session, "/select-disabled");
    let json = select_on_opened_page(&session, "CA");
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE", "{json}");

    open(&session, "/select-aria-disabled");
    let json = select_on_opened_page(&session, "CA");
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE", "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("aria-disabled"),
        "{json}"
    );

    let upload_file = format!("/tmp/rub-upload-{}.txt", std::process::id());
    let upload_disabled_file = format!("/tmp/rub-upload-disabled-{}.txt", std::process::id());
    let upload_fieldset_file = format!(
        "/tmp/rub-upload-fieldset-disabled-{}.txt",
        std::process::id()
    );
    std::fs::write(&upload_file, b"resume").unwrap();
    std::fs::write(&upload_disabled_file, "disabled").unwrap();
    std::fs::write(&upload_fieldset_file, "disabled").unwrap();

    open(&session, "/upload");
    let json = upload_on_opened_page(&session, &upload_file);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(
        json["data"]["interaction"]["semantic_class"], "set_value",
        "{json}"
    );
    assert_eq!(
        json["data"]["interaction"]["interaction_confirmed"], true,
        "{json}"
    );
    assert_eq!(
        json["data"]["interaction"]["confirmation_kind"], "files_attached",
        "{json}"
    );
    let uploaded_path = json["data"]["result"]["path"].as_str().unwrap();
    assert!(
        uploaded_path.ends_with(&format!("rub-upload-{}.txt", std::process::id())),
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["path_state"]["truth_level"], "input_path_reference",
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["path_state"]["path_authority"], "router.upload.input_path",
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["path_state"]["upstream_truth"], "upload_command_request",
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["path_state"]["path_kind"], "external_input_file",
        "{json}"
    );
    let exec_json = parse_json(
        &session
            .cmd()
            .args(["exec", "document.getElementById('filename').textContent"])
            .output()
            .unwrap(),
    );
    assert_eq!(exec_json["success"], true, "{exec_json}");
    assert_eq!(
        exec_json["data"]["result"],
        format!("rub-upload-{}.txt", std::process::id()),
        "{exec_json}"
    );

    open(&session, "/upload-disabled");
    let json = upload_on_opened_page(&session, &upload_disabled_file);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE", "{json}");

    open(&session, "/upload-fieldset-disabled");
    let json = upload_on_opened_page(&session, &upload_fieldset_file);
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "ELEMENT_NOT_INTERACTABLE", "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("disabled"),
        "{json}"
    );

    let _ = std::fs::remove_file(&upload_file);
    let _ = std::fs::remove_file(&upload_disabled_file);
    let _ = std::fs::remove_file(&upload_fieldset_file);
}

/// T270/T271/T280/T281: JS context and viewport flows should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t270_281_js_context_and_viewport_grouped_scenario() {
    let session = ManagedBrowserSession::new();

    let viewport_fixture = (0..30)
        .map(|idx| {
            format!(
                "<div style=\"height: 180px\"></div><button type=\"button\">Viewport Button {idx}</button>"
            )
        })
        .collect::<String>();
    let viewport_html: &'static str = Box::leak(
        format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Viewport Fixture</title></head>
<body style="margin: 0">{viewport_fixture}</body>
</html>"#
        )
        .into_boxed_str(),
    );
    let viewport_index_fixture = (0..24)
        .map(|idx| {
            format!(
                "<div style=\"height: 220px\"></div><button type=\"button\">Preserve Index {idx}</button>"
            )
        })
        .collect::<String>();
    let viewport_index_html: &'static str = Box::leak(
        format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Viewport Index Fixture</title></head>
<body style="margin: 0">{viewport_index_fixture}</body>
</html>"#
        )
        .into_boxed_str(),
    );
    let (_rt, server) = start_test_server(vec![
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html><html><body><div>Persistent JS Fixture</div></body></html>"#,
        ),
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
        ("/viewport", "text/html", viewport_html),
        ("/viewport-index", "text/html", viewport_index_html),
    ]);

    let open = |session: &ManagedBrowserSession, path: &str| {
        let opened = parse_json(
            &session
                .cmd()
                .args(["open", &server.url_for(path)])
                .output()
                .unwrap(),
        );
        assert_eq!(opened["success"], true, "{opened}");
    };

    open(&session, "/");
    let out = session
        .cmd()
        .args(["exec", "window.__rub_state = 42"])
        .output()
        .unwrap();
    assert_eq!(parse_json(&out)["success"], true);
    let out = session
        .cmd()
        .args(["exec", "window.__rub_state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"], 42, "{json}");

    open(&session, "/first");
    session
        .cmd()
        .args(["exec", "window.__rub_state = 42"])
        .output()
        .unwrap();
    open(&session, "/second");
    let out = session
        .cmd()
        .args(["exec", "window.__rub_state ?? null"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true, "{json}");
    assert!(json["data"]["result"].is_null(), "{json}");

    open(&session, "/viewport");
    let full_state = run_state(session.home());
    let viewport_state = parse_json(
        &session
            .cmd()
            .args(["state", "--viewport"])
            .output()
            .unwrap(),
    );
    assert_eq!(viewport_state["success"], true, "{viewport_state}");
    assert_eq!(
        viewport_state["data"]["result"]["snapshot"]["viewport_filtered"], true,
        "{viewport_state}"
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

    open(&session, "/viewport-index");
    let full_state = run_state(session.home());
    session
        .cmd()
        .args(["scroll", "down", "--amount", "1500"])
        .output()
        .unwrap();
    let viewport_state = parse_json(
        &session
            .cmd()
            .args(["state", "--viewport"])
            .output()
            .unwrap(),
    );
    assert_eq!(viewport_state["success"], true, "{viewport_state}");
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
}

/// T290-T293: state diff variants should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t290_293_state_diff_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let (_rt, server) = start_test_server(vec![
        (
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
        ),
        (
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
        ),
        (
            "/diff-change",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Diff Change Fixture</title></head>
<body>
  <button id="alpha">Alpha</button>
</body>
</html>"#,
        ),
        (
            "/",
            "text/html",
            r#"<!DOCTYPE html><html><body><div>Diff Fixture</div></body></html>"#,
        ),
    ]);

    let open = |path: &str| {
        let opened = parse_json(
            &session
                .cmd()
                .args(["open", &server.url_for(path)])
                .output()
                .unwrap(),
        );
        assert_eq!(opened["success"], true, "{opened}");
    };

    open("/diff");
    let base = run_state(session.home());
    let snap = snapshot_id(&base);
    session
        .cmd()
        .args([
            "exec",
            "document.body.insertAdjacentHTML('beforeend', '<button id=\"gamma\">Gamma</button>')",
        ])
        .output()
        .unwrap();
    let diff = parse_json(
        &session
            .cmd()
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true, "{diff}");
    assert_eq!(
        diff["data"]["result"]["diff"]["has_changes"], true,
        "{diff}"
    );
    assert!(
        diff["data"]["result"]["diff"]["added"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["text"] == "Gamma"),
        "{diff}"
    );

    open("/diff-remove");
    let base = run_state(session.home());
    let snap = snapshot_id(&base);
    session
        .cmd()
        .args(["exec", "document.getElementById('beta').remove()"])
        .output()
        .unwrap();
    let diff = parse_json(
        &session
            .cmd()
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true, "{diff}");
    assert!(
        diff["data"]["result"]["diff"]["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element["text"] == "Beta"),
        "{diff}"
    );

    open("/diff-change");
    let base = run_state(session.home());
    let snap = snapshot_id(&base);
    session
        .cmd()
        .args([
            "exec",
            "document.getElementById('alpha').textContent = 'Alpha Updated'",
        ])
        .output()
        .unwrap();
    let diff = parse_json(
        &session
            .cmd()
            .args(["state", "--diff", &snap])
            .output()
            .unwrap(),
    );
    assert_eq!(diff["success"], true, "{diff}");
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

    open("/");
    let json = parse_json(
        &session
            .cmd()
            .args(["state", "--diff", "snapshot-does-not-exist"])
            .output()
            .unwrap(),
    );
    assert_eq!(json["success"], false, "{json}");
    assert_eq!(json["error"]["code"], "STALE_SNAPSHOT", "{json}");
}

/// T310/T311: attach to an external Chrome, inspect state, then close without killing the browser.
#[test]
#[ignore]
#[serial]
fn t310_311_external_attach_lifecycle_grouped_scenario() {
    let (_rt, server) = start_test_server(vec![(
        "/external",
        "text/html",
        r#"<!DOCTYPE html>
<html>
<head><title>External CDP Fixture</title></head>
<body><button>External Ready</button></body>
</html>"#,
    )]);
    let (mut chrome, cdp_origin, profile_dir) =
        match spawn_external_chrome(Some(&server.url_for("/external"))) {
            Ok(Some(spawned)) => spawned,
            Ok(None) => {
                eprintln!("Skipping external CDP test because no Chrome/Chromium binary was found");
                return;
            }
            Err(error) => panic!("external CDP helper launch/readiness failed: {error}"),
        };
    let home = unique_home();
    prepare_home(&home);

    let state = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "state"])
            .output()
            .unwrap(),
    );
    assert_eq!(state["success"], true, "{state}");
    assert_eq!(
        state["data"]["result"]["snapshot"]["title"],
        "External CDP Fixture"
    );
    let tabs = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "tabs"])
            .output()
            .unwrap(),
    );
    assert_eq!(tabs["success"], true, "{tabs}");
    let active_tab = tabs["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["active"] == true)
        .expect("external attach should project one active tab");
    assert_eq!(active_tab["active_authority"], "browser_truth");

    let closed = parse_json(&rub_cmd(&home).arg("close").output().unwrap());
    assert_eq!(closed["success"], true, "{closed}");

    let doctor = parse_json(&rub_cmd(&home).arg("doctor").output().unwrap());
    let report = doctor_result(&doctor);
    assert_eq!(doctor["success"], true, "{doctor}");
    assert_eq!(
        report["launch_policy"]["connection_target"]["source"],
        "cdp_url"
    );
    let canonical_url = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(rub_cdp::attachment::canonical_external_browser_identity(
            &cdp_origin,
        ))
        .expect("external CDP origin should canonicalize");
    assert_eq!(
        report["launch_policy"]["connection_target"]["url"],
        canonical_url
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

    terminate_external_chrome(&mut chrome, &profile_dir);
    teardown_and_cleanup(&home);
}

/// T310a: external attach should accept a multi-tab external browser when active-tab authority is unique.
#[test]
#[ignore]
#[serial]
fn t310a_external_attach_accepts_multi_tab_browser_with_unique_active_tab_authority() {
    let (_rt, server) = start_test_server(vec![
        (
            "/external-one",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>External One</title></head>
<body>
  <button>One</button>
  <script>
    setTimeout(() => {
      window.open('/external-two', '_blank');
    }, 50);
  </script>
</body>
</html>"#,
        ),
        (
            "/external-two",
            "text/html",
            r#"<!DOCTYPE html><html><head><title>External Two</title></head><body><button>Two</button></body></html>"#,
        ),
    ]);
    let (mut chrome, cdp_origin, profile_dir) = match spawn_external_chrome(Some(
        &server.url_for("/external-one"),
    )) {
        Ok(Some(spawned)) => spawned,
        Ok(None) => {
            eprintln!(
                "Skipping multi-tab external CDP test because no Chrome/Chromium binary was found"
            );
            return;
        }
        Err(error) => panic!(
            "multi-tab external CDP helper launch/readiness failed before residue proof: {error}"
        ),
    };
    let home = unique_home();
    prepare_home(&home);
    let external_two_url = server.url_for("/external-two");
    ensure_external_cdp_page_active(
        &cdp_origin,
        &external_two_url,
        "External Two",
        Duration::from_secs(10),
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let state = loop {
        let out = parse_json(
            &rub_cmd(&home)
                .args(["--timeout", "10000", "--cdp-url", &cdp_origin, "state"])
                .output()
                .unwrap(),
        );
        assert_poll_success("state", &out);
        if out["data"]["result"]["snapshot"]["title"] == "External Two"
        {
            break out;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for multi-tab external attach success: {out}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(state["success"], true, "{state}");
    let tabs = parse_json(
        &rub_cmd(&home)
            .args(["--cdp-url", &cdp_origin, "tabs"])
            .output()
            .unwrap(),
    );
    assert_eq!(tabs["success"], true, "{tabs}");
    let items = tabs["data"]["result"]["items"].as_array().unwrap();
    assert!(items.len() >= 2, "{tabs}");
    let active_tab = items
        .iter()
        .find(|tab| tab["active"] == true)
        .expect("multi-tab external attach should keep one active tab");
    assert_eq!(active_tab["title"], "External Two");
    assert_eq!(active_tab["active_authority"], "browser_truth");
    let closed = parse_json(&rub_cmd(&home).arg("close").output().unwrap());
    assert_eq!(closed["success"], true, "{closed}");
    assert!(
        chrome.try_wait().unwrap().is_none(),
        "external browser should still be alive after rub close"
    );
    let addr = cdp_origin.trim_start_matches("http://");
    assert!(
        TcpStream::connect(addr).is_ok(),
        "external CDP port should still accept connections after rub close"
    );

    terminate_external_chrome(&mut chrome, &profile_dir);
    teardown_and_cleanup(&home);
}

/// T310b: failed external attach must not leave a startup daemon residue behind.
#[test]
#[ignore]
#[serial]
fn t310b_failed_external_attach_does_not_leave_daemon_residue() {
    let home = unique_home();
    prepare_home(&home);

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
    assert_eq!(json["error"]["code"], "CDP_CONNECTION_FAILED");

    let residues = daemon_processes_for_home(&home);
    assert!(
        residues.is_empty(),
        "startup failure must not leave daemon residue for home {home}: {residues:#?}"
    );
    assert_no_startup_session_residue(&home, "default");

    let _ = std::fs::remove_dir_all(&home);
}

// T311 is covered by `t310_311_external_attach_lifecycle_grouped_scenario`.

/// T320: `--profile` resolves a named Chrome profile and projects it in doctor output.
#[test]
#[ignore]
#[serial]
fn t320_profile_resolve() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (fake_home, resolved_profile, envs_owned) = prepare_fake_profile_env();
    let (_rt, server) = start_standard_site_fixture();
    let envs = envs_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();

    let out = rub_cmd_env(home, &envs)
        .args(["--profile", "Default", "open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);

    let out = rub_cmd_env(home, &envs).arg("doctor").output().unwrap();
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
    let reported_path = std::fs::canonicalize(
        report["launch_policy"]["connection_target"]["resolved_path"]
            .as_str()
            .expect("doctor resolved path"),
    )
    .expect("canonicalize reported profile path");
    assert_eq!(reported_path, resolved_profile);
    assert_eq!(
        report["launch_policy"]["connection_target"]["resolved_path_state"]["truth_level"],
        "operator_path_reference"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["resolved_path_state"]["path_authority"],
        "router.doctor.launch_policy.connection_target.resolved_path"
    );
    assert_eq!(
        report["launch_policy"]["connection_target"]["resolved_path_state"]["path_kind"],
        "profile_directory_reference"
    );
    let _ = std::fs::remove_dir_all(fake_home);
}

/// T330-T333: close/cleanup lifecycle flows should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t330_333_close_and_cleanup_grouped_scenario() {
    let session = ManagedBrowserSession::new();
    let home = session.home().to_string();
    let (_rt, server) = start_standard_site_fixture();

    let open_default = || {
        let out = session
            .cmd()
            .args(["open", &server.url()])
            .output()
            .unwrap();
        let json = parse_json(&out);
        assert_eq!(json["success"], true, "{json}");
    };
    let open_work = || {
        let out = session
            .cmd()
            .args(["--session", "work", "open", &server.url()])
            .output()
            .unwrap();
        let json = parse_json(&out);
        assert_eq!(json["success"], true, "{json}");
    };

    open_default();
    open_work();
    let json = parse_json(&session.cmd().args(["close", "--all"]).output().unwrap());
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(
        json["data"]["result"]["closed"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "{json}"
    );
    let sessions = parse_json(&session.cmd().arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    assert_eq!(
        sessions["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "{sessions}"
    );

    open_default();
    let daemon_pid: u32 = std::fs::read_to_string(default_session_pid_path(session.home()))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        !browser_processes_for_daemon_pid(daemon_pid).is_empty(),
        "managed browser processes should exist before close --all"
    );
    let json = parse_json(&session.cmd().args(["close", "--all"]).output().unwrap());
    assert_eq!(json["success"], true, "{json}");
    wait_until(Duration::from_secs(5), || {
        browser_processes_for_daemon_pid(daemon_pid).is_empty()
    });

    open_default();
    open_work();
    let work_pid: i32 = std::fs::read_to_string(session_pid_path(session.home(), "work"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(work_pid, libc::SIGKILL);
    }
    wait_for_pid_exit(work_pid, Duration::from_secs(5));
    let json = parse_json(&session.cmd().args(["close", "--all"]).output().unwrap());
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(
        json["data"]["result"]["cleaned_stale"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        1,
        "{json}"
    );
    assert!(
        json["data"]["result"]["cleaned_stale"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "work"),
        "{json}"
    );
    assert_eq!(
        json["data"]["subject"]["rub_home_state"]["path_authority"],
        "cli.close_all.subject.rub_home",
        "{json}"
    );

    open_default();
    let pid: i32 = std::fs::read_to_string(default_session_pid_path(session.home()))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    wait_for_pid_exit(pid, Duration::from_secs(5));
    let json = parse_json(&session.cmd().arg("cleanup").output().unwrap());
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(
        json["data"]["subject"]["rub_home_state"]["path_authority"], "cli.cleanup.subject.rub_home",
        "{json}"
    );
    assert!(
        json["data"]["result"]["cleaned_stale_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "default"),
        "{json}"
    );
    let sessions = parse_json(&session.cmd().arg("sessions").output().unwrap());
    assert_eq!(sessions["success"], true, "{sessions}");
    assert_eq!(
        sessions["data"]["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "{sessions}"
    );

    open_default();
    let json = parse_json(&session.cmd().arg("cleanup").output().unwrap());
    assert_eq!(json["success"], true, "{json}");
    assert!(
        json["data"]["result"]["kept_active_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "default"),
        "{json}"
    );
    let doctor = parse_json(&session.cmd().arg("doctor").output().unwrap());
    assert_eq!(doctor["success"], true, "{doctor}");

    let (broken_close_session_id, broken_close_socket, broken_close_server) =
        inject_replay_failing_batch_close_session(&home, "broken-close");
    let close_all_failed = parse_json(&session.cmd().args(["close", "--all"]).output().unwrap());
    assert_eq!(close_all_failed["success"], false, "{close_all_failed}");
    assert_eq!(
        close_all_failed["error"]["code"],
        "IPC_PROTOCOL_ERROR",
        "{close_all_failed}"
    );
    assert_eq!(
        close_all_failed["error"]["context"]["reason"],
        "close_all_partial_failure",
        "{close_all_failed}"
    );
    assert!(
        close_all_failed["error"]["context"]["failed_sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "broken-close"),
        "{close_all_failed}"
    );
    assert_eq!(
        close_all_failed["error"]["context"]["session_error_details"][0]["session"],
        "broken-close",
        "{close_all_failed}"
    );
    let close_recovery_contract =
        &close_all_failed["error"]["context"]["session_error_details"][0]["error"]["context"]
            ["recovery_contract"];
    assert_session_post_commit_recovery_contract(
        close_recovery_contract,
        &home,
        "broken-close",
        &broken_close_session_id,
        &close_all_failed,
    );
    broken_close_server
        .join()
        .expect("close --all replay-failing fake daemon thread should join");
    let _ = std::fs::remove_file(&broken_close_socket);
    rub_daemon::session::deregister_session(
        std::path::Path::new(&home),
        &broken_close_session_id,
    )
    .expect("remove injected replay-failing close --all session");
    let _ = std::fs::remove_dir_all(
        rub_daemon::rub_paths::RubPaths::new(&home)
            .session_runtime("broken-close", &broken_close_session_id)
            .session_dir(),
    );

    open_default();
    let (broken_teardown_session_id, broken_teardown_socket, broken_teardown_server) =
        inject_replay_failing_batch_close_session(&home, "broken-teardown");
    let teardown_failed = parse_json(&session.cmd().arg("teardown").output().unwrap());
    assert_eq!(teardown_failed["success"], false, "{teardown_failed}");
    assert_eq!(
        teardown_failed["error"]["code"],
        "IPC_TIMEOUT",
        "{teardown_failed}"
    );
    assert_eq!(
        teardown_failed["error"]["context"]["reason"],
        "command_deadline_exhausted",
        "{teardown_failed}"
    );
    assert_eq!(
        teardown_failed["error"]["context"]["teardown_close_all"]["result"]["session_error_details"]
            [0]["session"],
        "broken-teardown",
        "{teardown_failed}"
    );
    let teardown_recovery_contract =
        &teardown_failed["error"]["context"]["teardown_close_all"]["result"]
            ["session_error_details"][0]["error"]["context"]["recovery_contract"];
    assert_session_post_commit_recovery_contract(
        teardown_recovery_contract,
        &home,
        "broken-teardown",
        &broken_teardown_session_id,
        &teardown_failed,
    );
    broken_teardown_server
        .join()
        .expect("teardown replay-failing fake daemon thread should join");
    let _ = std::fs::remove_file(&broken_teardown_socket);
    rub_daemon::session::deregister_session(
        std::path::Path::new(&home),
        &broken_teardown_session_id,
    )
    .expect("remove injected replay-failing teardown session");
    let _ = std::fs::remove_dir_all(
        rub_daemon::rub_paths::RubPaths::new(&home)
            .session_runtime("broken-teardown", &broken_teardown_session_id)
            .session_dir(),
    );

    let cleanup_observation = observe_home_cleanup(&home);
    teardown_and_cleanup(&home);
    drop(session);
    wait_until(Duration::from_secs(5), || !std::path::Path::new(&home).exists());
    assert_eq!(
        verify_home_cleanup_complete(&home, &cleanup_observation),
        Ok(CleanupVerification::Verified)
    );
}

/// T350: `RUB_SESSION` sets the default session, but explicit `--session` overrides it.
#[test]
#[ignore]
#[serial]
fn t350_rub_session_env() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_standard_site_fixture();

    let out = rub_cmd_env(home, &[("RUB_SESSION", "alt")])
        .args(["open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["session"], "alt");

    let out = rub_cmd_env(home, &[("RUB_SESSION", "alt")])
        .args(["--session", "explicit", "open", &server.url()])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], true);
    assert_eq!(json["session"], "explicit");

    let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
    let names = sessions["data"]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|session| session["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"alt"));
    assert!(names.contains(&"explicit"));

    teardown_and_cleanup(home);
}

/// T360: connection mode flags are mutually exclusive.
#[test]
#[ignore]
#[serial]
fn t360_mutual_exclusion() {
    let home = unique_home();
    prepare_home(&home);

    let out = rub_cmd(&home)
        .args(["--cdp-url", "http://127.0.0.1:9222", "--connect", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "CONFLICTING_CONNECT_OPTIONS");

    let _ = std::fs::remove_dir_all(&home);
}

/// T300/T301: screenshot highlight flows should reuse one browser-backed scenario.
#[test]
#[ignore]
#[serial]
fn t300_301_screenshot_highlight_grouped_scenario() {
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
    let json = parse_json(
        &session
            .cmd()
            .args(["screenshot", "--highlight"])
            .output()
            .unwrap(),
    );
    assert_eq!(json["success"], true, "{json}");
    assert!(
        json["data"]["result"]["highlight"]["highlighted_count"]
            .as_u64()
            .unwrap()
            > 0,
        "highlighted_count should reflect overlays injected from the snapshot"
    );
    assert_eq!(
        json["data"]["result"]["highlight"]["cleanup"], true,
        "{json}"
    );

    let opened = parse_json(
        &session
            .cmd()
            .args(["open", "chrome://newtab"])
            .output()
            .unwrap(),
    );
    assert_eq!(opened["success"], true, "{opened}");
    let json = parse_json(
        &session
            .cmd()
            .args(["screenshot", "--highlight"])
            .output()
            .unwrap(),
    );
    assert_eq!(json["success"], true, "{json}");
    assert!(
        json["data"]["result"]["highlight"]["highlighted_count"].is_number(),
        "highlighted_count should still be projected on Trusted Types pages"
    );
    assert_eq!(
        json["data"]["result"]["highlight"]["cleanup"], true,
        "{json}"
    );
}

/// T362: a fresh session with `--cdp-url` must fail on the requested connection,
/// not on an attempted launch-policy probe against a non-existent daemon.
#[test]
#[ignore]
#[serial]
fn t362_new_session_invalid_cdp_url_reports_connection_failure() {
    let home = unique_home();
    prepare_home(&home);

    let out = rub_cmd(&home)
        .args(["--cdp-url", "http://127.0.0.1:1", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "CDP_CONNECTION_FAILED");
    let residues = daemon_processes_for_home(&home);
    assert!(
        residues.is_empty(),
        "fresh attach failure must not leave daemon residue for home {home}: {residues:#?}"
    );
    assert_no_startup_session_residue(&home, "default");

    let _ = std::fs::remove_dir_all(&home);
}

/// T361: explicit connection override must not be silently ignored by an existing session.
#[test]
#[ignore]
#[serial]
fn t361_existing_session_rejects_connection_override() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(home)
        .args(["--cdp-url", "http://127.0.0.1:1", "state"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");
}

/// T363: explicit session policy overrides must not be silently ignored by an existing session.
#[test]
#[ignore]
#[serial]
fn t363_existing_session_rejects_session_policy_override() {
    let session = ManagedBrowserSession::new();
    let home = session.home();
    let (_rt, server) = start_standard_site_fixture();

    rub_cmd(home)
        .args(["open", &server.url()])
        .output()
        .unwrap();

    let out = rub_cmd(home)
        .args(["--humanize", "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");

    let out = rub_cmd(home)
        .args(["--no-stealth", "doctor"])
        .output()
        .unwrap();
    let json = parse_json(&out);
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");
}
