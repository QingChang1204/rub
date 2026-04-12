//! End-to-end tests for rub CLI.
//! These tests require Chrome to be installed.
//! Run with: cargo test --test e2e -- --ignored

use rub_daemon::rub_paths::RubPaths;
pub(crate) use rub_test_harness::browser_session::*;
use rub_test_harness::fixtures::{DownloadFixtureServer, NetworkInspectionFixtureServer};
use rub_test_harness::server::TestServer;
use serde_json::{Value, json};
use serial_test::serial;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn rub_binary() -> String {
    // Try env var set by cargo for integration tests
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_rub") {
        return path;
    }
    // Fallback: walk up from crate manifest dir to workspace target
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let workspace = std::path::Path::new(&manifest)
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(std::path::Path::new("."));
    let path = workspace.join("target/debug/rub");
    path.to_string_lossy().to_string()
}

fn rub_cmd(rub_home: &str) -> Command {
    let mut cmd = Command::new(rub_binary());
    cmd.arg("--rub-home").arg(rub_home);
    cmd
}

fn rub_cmd_env(rub_home: &str, envs: &[(&str, &str)]) -> Command {
    let mut cmd = rub_cmd(rub_home);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd
}

fn parse_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
}

fn doctor_result(json: &serde_json::Value) -> &serde_json::Value {
    &json["data"]["result"]
}

fn doctor_runtime(json: &serde_json::Value) -> &serde_json::Value {
    &json["data"]["runtime"]
}

fn wait_for_pending_dialog(home: &str) -> serde_json::Value {
    for _ in 0..40 {
        let out = rub_cmd(home).arg("dialog").output().unwrap();
        let json = parse_json(&out);
        if json["success"] == true && !json["data"]["runtime"]["pending_dialog"].is_null() {
            return json;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("Timed out waiting for pending dialog");
}

fn wait_for_tabs_count(home: &str, count: u64) -> serde_json::Value {
    for _ in 0..50 {
        let out = parse_json(&rub_cmd(home).arg("tabs").output().unwrap());
        if out["success"] == true
            && out["data"]["result"]["items"]
                .as_array()
                .map(|items| items.len() as u64)
                .unwrap_or(0)
                >= count
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Timed out waiting for at least {count} tabs");
}

fn wait_for_trigger_status(home: &str, id: u64, expected: &str) -> serde_json::Value {
    for _ in 0..80 {
        let out = parse_json(&rub_cmd(home).args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"]
                .as_array()
                .and_then(|triggers| {
                    triggers
                        .iter()
                        .find(|trigger| trigger["id"].as_u64() == Some(id))
                })
            && trigger["status"].as_str() == Some(expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Timed out waiting for trigger {id} to reach status '{expected}'");
}

fn wait_for_trigger_unavailable_reason(home: &str, id: u64, expected: &str) -> serde_json::Value {
    for _ in 0..80 {
        let out = parse_json(&rub_cmd(home).args(["trigger", "list"]).output().unwrap());
        if out["success"] == true
            && let Some(trigger) = out["data"]["result"]["items"]
                .as_array()
                .and_then(|triggers| {
                    triggers
                        .iter()
                        .find(|trigger| trigger["id"].as_u64() == Some(id))
                })
            && trigger["unavailable_reason"].as_str() == Some(expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Timed out waiting for trigger {id} to publish unavailable_reason '{expected}'");
}

fn wait_for_orchestration_status(
    home: &str,
    session: &str,
    id: u64,
    expected: &str,
) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..80 {
        let out = parse_json(
            &rub_cmd(home)
                .args(["--session", session, "orchestration", "list"])
                .output()
                .unwrap(),
        );
        last = out.clone();
        if out["success"] == true
            && let Some(rule) = out["data"]["result"]["items"]
                .as_array()
                .and_then(|rules| rules.iter().find(|rule| rule["id"].as_u64() == Some(id)))
            && rule["status"].as_str() == Some(expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} in session '{session}' to reach status '{expected}'; last list output: {last}"
    );
}

fn wait_for_orchestration_rule_result(
    home: &str,
    session: &str,
    id: u64,
    expected_status: &str,
    expected_result_status: &str,
) -> serde_json::Value {
    for _ in 0..80 {
        let out = parse_json(
            &rub_cmd(home)
                .args(["--session", session, "orchestration", "list"])
                .output()
                .unwrap(),
        );
        if out["success"] == true
            && let Some(rule) = out["data"]["result"]["items"]
                .as_array()
                .and_then(|rules| rules.iter().find(|rule| rule["id"].as_u64() == Some(id)))
            && rule["status"].as_str() == Some(expected_status)
            && rule["last_result"]["status"].as_str() == Some(expected_result_status)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} in session '{session}' to reach status '{expected_status}' with last_result '{expected_result_status}'"
    );
}

fn wait_for_text_in_session(
    home: &str,
    session: &str,
    selector: &str,
    expected: &str,
    timeout: Duration,
) -> String {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let out = parse_json(
            &rub_cmd(home)
                .args([
                    "--session",
                    session,
                    "inspect",
                    "text",
                    "--selector",
                    selector,
                ])
                .output()
                .unwrap(),
        );
        if out["success"] == true
            && let Some(text) = out["data"]["result"]["value"].as_str()
            && text == expected
        {
            return text.to_string();
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "Timed out waiting for selector '{selector}' in session '{session}' to equal '{expected}'"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn session_id_by_name(sessions_json: &serde_json::Value, name: &str) -> String {
    sessions_json["data"]["result"]["items"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["name"].as_str() == Some(name))
        })
        .and_then(|session| session["id"].as_str())
        .unwrap_or_else(|| panic!("session '{name}' should be present in sessions output"))
        .to_string()
}

fn frame_id_by_name(frames_json: &serde_json::Value, name: &str) -> String {
    frames_json["data"]["result"]["items"]
        .as_array()
        .and_then(|frames| {
            frames
                .iter()
                .find(|entry| entry["frame"]["name"].as_str() == Some(name))
        })
        .and_then(|entry| entry["frame"]["frame_id"].as_str())
        .unwrap_or_else(|| panic!("frame '{name}' should be present in frames output"))
        .to_string()
}

fn start_test_server(
    routes: Vec<(&'static str, &'static str, &'static str)>,
) -> (tokio::runtime::Runtime, TestServer) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(TestServer::start(routes));
    (runtime, server)
}

fn start_standard_site_fixture() -> (tokio::runtime::Runtime, TestServer) {
    start_test_server(vec![
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
            "/click",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Click Fixture</title></head>
<body>
  <button id="advance" onclick="document.body.dataset.clicked='yes'">Advance</button>
</body>
</html>"#,
        ),
        (
            "/forms/post",
            "text/html",
            r#"<!DOCTYPE html>
<html>
<head><title>Form Fixture</title></head>
<body>
  <form>
    <input name="custname" />
    <input name="custtel" />
    <input name="custemail" />
    <select name="size">
      <option value="small">Small</option>
      <option value="medium">Medium</option>
      <option value="large">Large</option>
    </select>
    <textarea name="comments"></textarea>
  </form>
  <div style="height: 2200px"></div>
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
    ])
}

fn start_header_fixture_server() -> (
    String,
    std::sync::mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        for _ in 0..4 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");

            let (status, content_type, body) = match path {
                "/app" => (
                    "200 OK",
                    "text/html",
                    r#"<!DOCTYPE html>
<html>
<body>
  <div id="status">loading</div>
  <script>
    fetch('/capture')
      .then((r) => r.text())
      .then((text) => { document.getElementById('status').textContent = text; })
      .catch((error) => { document.getElementById('status').textContent = 'error:' + error; });
  </script>
</body>
</html>"#,
                ),
                "/capture" => {
                    let _ = tx.send(request.clone());
                    ("200 OK", "text/plain", "ok")
                }
                _ => ("404 Not Found", "text/plain", "missing"),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            if path == "/capture" {
                break;
            }
        }
    });
    (format!("http://{}", addr), rx, handle)
}

fn start_header_capture_server() -> (
    String,
    std::sync::mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = tx.send(request);
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{}", addr), rx, handle)
}

fn run_state(home: &str) -> Value {
    let output = rub_cmd(home).arg("state").output().unwrap();
    let json = parse_json(&output);
    assert_eq!(json["success"], true, "state should succeed");
    json
}

fn state_snapshot(state_json: &Value) -> &Value {
    &state_json["data"]["result"]["snapshot"]
}

fn snapshot_id(state_json: &Value) -> String {
    state_snapshot(state_json)["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn find_element_index<F>(state_json: &Value, predicate: F) -> u32
where
    F: Fn(&Value) -> bool,
{
    state_snapshot(state_json)["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| predicate(element))
        .and_then(|element| element["index"].as_u64())
        .unwrap() as u32
}

fn find_element_ref<F>(state_json: &Value, predicate: F) -> String
where
    F: Fn(&Value) -> bool,
{
    state_snapshot(state_json)["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| predicate(element))
        .and_then(|element| element["element_ref"].as_str())
        .unwrap()
        .to_string()
}

#[test]
fn e2e_home_owner_pid_parses_expected_home_shape() {
    assert_eq!(
        e2e_home_owner_pid(Path::new("/tmp/rub-e2e-12345-019d4e48-a9a3")),
        Some(12345)
    );
    assert_eq!(e2e_home_owner_pid(Path::new("/tmp/not-rub-home")), None);
    assert_eq!(e2e_home_owner_pid(Path::new("/tmp/rub-e2e-bad-uuid")), None);
}

#[test]
fn daemon_root_pids_for_home_falls_back_to_process_table_shape() {
    let ps_home = format!("/tmp/rub-e2e-{}-test", std::process::id());
    let fake = format!(
        "101 /Users/test/problem/target/debug/rub __daemon --session default --rub-home {ps_home}\n\
         202 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --user-data-dir=/tmp/rub-chrome-101\n\
         303 /Users/test/problem/target/debug/rub __daemon --session other --rub-home /tmp/elsewhere\n"
    );
    let parsed: Vec<u32> = fake
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.contains("rub __daemon") && trimmed.contains(&ps_home) {
                trimmed.split_whitespace().next()?.parse::<u32>().ok()
            } else {
                None
            }
        })
        .collect();
    assert_eq!(parsed, vec![101]);
}

#[test]
fn daemon_pid_match_rejects_reused_pid_for_other_home() {
    let snapshot = "\
101 /Users/test/problem/target/debug/rub __daemon --session default --rub-home /tmp/rub-e2e-101-home\n\
202 /Users/test/problem/target/debug/rub __daemon --session default --rub-home /tmp/other-home\n";

    assert!(daemon_pid_matches_home_in_snapshot(
        snapshot,
        101,
        "/tmp/rub-e2e-101-home"
    ));
    assert!(!daemon_pid_matches_home_in_snapshot(
        snapshot,
        202,
        "/tmp/rub-e2e-101-home"
    ));
}

#[test]
fn external_chrome_pid_match_rejects_reused_pid_for_other_profile() {
    let snapshot = "\
101 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --user-data-dir=/tmp/rub-external-chrome-keep\n\
202 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --user-data-dir=/tmp/rub-external-chrome-other\n";

    assert!(external_chrome_pid_matches_profile_in_snapshot(
        snapshot,
        101,
        Path::new("/tmp/rub-external-chrome-keep")
    ));
    assert!(!external_chrome_pid_matches_profile_in_snapshot(
        snapshot,
        202,
        Path::new("/tmp/rub-external-chrome-keep")
    ));
}

#[test]
fn verify_external_chrome_cleanup_ignores_live_reused_pid_without_profile_authority() {
    let profile_dir = std::env::temp_dir().join(format!(
        "rub-external-chrome-authority-mismatch-{}",
        uuid::Uuid::now_v7()
    ));
    let _ = std::fs::remove_dir_all(&profile_dir);

    let verification = verify_external_chrome_cleanup_complete(std::process::id(), &profile_dir)
        .expect("unrelated live pid should not fail external chrome cleanup verification");
    assert_eq!(verification, CleanupVerification::Verified);
}

#[test]
#[serial]
fn prepare_home_preserves_registered_cleanup_authority() {
    let home = unique_home();
    prepare_home(&home);
    assert!(
        registered_homes()
            .lock()
            .unwrap()
            .iter()
            .any(|existing| existing == &home),
        "prepare_home should not unregister the active test home"
    );
    unregister_home(&home);
}

#[test]
#[serial]
fn cleanup_registered_artifacts_retains_unverified_entries_during_panic_unwind() {
    struct CleanupDuringPanic;

    impl Drop for CleanupDuringPanic {
        fn drop(&mut self) {
            cleanup_registered_artifacts();
        }
    }

    let home = unique_home();
    std::fs::create_dir_all(&home).unwrap();
    let _ = std::panic::catch_unwind(|| {
        let _guard = CleanupDuringPanic;
        panic!("trigger cleanup while panicking");
    });

    assert!(
        registered_homes()
            .lock()
            .unwrap()
            .iter()
            .any(|existing| existing == &home),
        "panic-path cleanup should retain the home for atexit retry authority"
    );
    assert!(
        Path::new(&home).exists(),
        "panic-path cleanup must not destructively remove registered homes during unwind"
    );

    unregister_home(&home);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn observe_home_cleanup_includes_pid_file_residue_when_daemon_is_already_gone() {
    let home = std::env::temp_dir().join(format!(
        "rub-e2e-observe-home-cleanup-{}",
        uuid::Uuid::now_v7()
    ));
    let pid_path = RubPaths::new(&home)
        .session_runtime("default", "sess-dead")
        .pid_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&pid_path, "4242\n").unwrap();

    let observed = observe_home_cleanup(home.to_string_lossy().as_ref());

    assert!(
        observed.daemon_root_pids.contains(&4242),
        "cleanup observation should keep artifact-based daemon pid authority even after the live daemon is gone"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn verify_home_cleanup_ignores_unrelated_pid_artifacts_without_browser_authority() {
    let home = std::env::temp_dir().join(format!(
        "rub-e2e-cleanup-false-positive-{}",
        uuid::Uuid::now_v7()
    ));
    let home_string = home.to_string_lossy().to_string();
    let observed = HomeCleanupObservation {
        daemon_root_pids: vec![9_999_991],
    };

    let verification = verify_home_cleanup_complete(&home_string, &observed)
        .expect("unrelated pid should not fail cleanup verification");
    assert_eq!(verification, CleanupVerification::Verified);
}

#[test]
fn verify_home_cleanup_complete_detects_managed_profile_residue_for_observed_daemon() {
    let fake_pid = 424_242u32;
    let home = format!("/tmp/rub-e2e-cleanup-residue-{fake_pid}");
    let profile_dir = managed_browser_profile_dir_for_daemon(fake_pid);
    let _ = std::fs::remove_dir_all(&profile_dir);
    std::fs::create_dir_all(&profile_dir).unwrap();

    let result = verify_home_cleanup_complete(
        &home,
        &HomeCleanupObservation {
            daemon_root_pids: vec![fake_pid],
        },
    );

    assert!(
        result
            .err()
            .is_some_and(|message| message.contains("managed browser profile residue")),
        "cleanup verification should fail closed when a managed profile directory remains"
    );

    let _ = std::fs::remove_dir_all(profile_dir);
}

#[test]
fn probe_cdp_http_ready_once_times_out_on_half_open_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept connection");
        std::thread::sleep(Duration::from_millis(300));
        drop(stream);
    });

    let start = std::time::Instant::now();
    let ready = probe_cdp_http_ready_once(&format!("http://{addr}"), Duration::from_millis(100));
    let elapsed = start.elapsed();

    server.join().expect("server thread should join");
    assert!(!ready);
    assert!(elapsed < Duration::from_millis(250));
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct E2eSourceFunction {
    module: String,
    name: String,
    body: String,
}

fn parse_top_level_functions(module: &str, source: &str) -> Vec<E2eSourceFunction> {
    let starts = std::iter::once(0usize)
        .chain(
            source
                .match_indices('\n')
                .map(|(index, _)| index + 1)
                .filter(|start| *start < source.len()),
        )
        .filter(|start| source[*start..].starts_with("fn "))
        .collect::<Vec<_>>();

    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts.get(index + 1).copied().unwrap_or(source.len());
            let body = &source[*start..end];
            let after_fn = &body["fn ".len()..];
            let name_end = after_fn
                .find('(')
                .unwrap_or_else(|| panic!("function signature should include '(' in {module}"));
            let name = &after_fn[..name_end];
            E2eSourceFunction {
                module: module.to_string(),
                name: name.to_string(),
                body: body.to_string(),
            }
        })
        .collect()
}

fn browser_backed_e2e_functions() -> Vec<E2eSourceFunction> {
    let e2e_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let mut modules = std::fs::read_dir(&e2e_dir)
        .unwrap_or_else(|error| panic!("failed to enumerate {}: {error}", e2e_dir.display()))
        .map(|entry| entry.expect("e2e module entry should be readable").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .collect::<Vec<_>>();
    modules.sort();

    modules
        .into_iter()
        .flat_map(|path| {
            let module = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_else(|| {
                    panic!("e2e module path must have a valid stem: {}", path.display())
                })
                .to_string();
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            parse_top_level_functions(&module, &source)
        })
        .collect()
}

fn qualified_function_name(function: &E2eSourceFunction) -> String {
    format!("{}::{}", function.module, function.name)
}

fn expected_function_set(entries: &[(&str, &str)]) -> std::collections::BTreeSet<String> {
    entries
        .iter()
        .map(|(module, name)| format!("{module}::{name}"))
        .collect()
}

#[test]
fn browser_backed_unique_home_usage_matches_exception_whitelist() {
    let expected = expected_function_set(&[
        ("foundation", "t382_humanize_click_reports_delay_in_timing"),
        (
            "state_workflow",
            "t215_concurrent_first_command_serializes_startup",
        ),
        ("state_workflow", "t233e_i_history_export_grouped_scenario"),
        (
            "state_workflow",
            "t310_311_external_attach_lifecycle_grouped_scenario",
        ),
        (
            "trigger_runtime",
            "t437_trigger_text_present_fires_cross_tab_click",
        ),
        (
            "trigger_runtime",
            "t437b_trigger_records_blocked_outcome_when_target_action_fails",
        ),
        (
            "trigger_runtime",
            "t437c_trigger_resume_ignores_stale_network_evidence_and_fires_on_new_request",
        ),
        (
            "trigger_runtime",
            "t437d_trigger_reports_target_missing_and_does_not_fire",
        ),
        (
            "trigger_runtime",
            "t437e_trigger_trace_projects_recent_lifecycle_and_outcome_events",
        ),
        (
            "trigger_runtime",
            "t437f_trigger_degrades_when_target_selected_frame_becomes_stale",
        ),
    ]);
    let actual = browser_backed_e2e_functions()
        .into_iter()
        .filter(|function| function.body.contains("unique_home()"))
        .map(|function| qualified_function_name(&function))
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "browser-backed E2E must only use unique_home() in explicit authority-isolated exceptions"
    );
}

#[test]
fn browser_backed_external_attach_usage_matches_exception_whitelist() {
    let expected = expected_function_set(&[
        (
            "runtime_integration",
            "t388_389c_external_handoff_and_takeover_grouped_scenario",
        ),
        (
            "state_workflow",
            "t310_311_external_attach_lifecycle_grouped_scenario",
        ),
        (
            "state_workflow",
            "t310a_external_attach_rejects_ambiguous_page_authority",
        ),
        (
            "state_workflow",
            "t310b_failed_external_attach_does_not_leave_daemon_residue",
        ),
        ("state_workflow", "t360_mutual_exclusion"),
        (
            "state_workflow",
            "t361_existing_session_rejects_connection_override",
        ),
        (
            "state_workflow",
            "t362_new_session_invalid_cdp_url_reports_connection_failure",
        ),
    ]);
    let actual = browser_backed_e2e_functions()
        .into_iter()
        .filter(|function| {
            function.body.contains("spawn_external_chrome(")
                || function.body.contains("\"--cdp-url\"")
        })
        .map(|function| qualified_function_name(&function))
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "browser-backed E2E must keep external Chrome and external attach coverage inside the explicit exception whitelist"
    );
}

#[test]
fn trigger_runtime_unique_home_usage_matches_authority_isolated_whitelist() {
    let expected = expected_function_set(&[
        (
            "trigger_runtime",
            "t437_trigger_text_present_fires_cross_tab_click",
        ),
        (
            "trigger_runtime",
            "t437b_trigger_records_blocked_outcome_when_target_action_fails",
        ),
        (
            "trigger_runtime",
            "t437c_trigger_resume_ignores_stale_network_evidence_and_fires_on_new_request",
        ),
        (
            "trigger_runtime",
            "t437d_trigger_reports_target_missing_and_does_not_fire",
        ),
        (
            "trigger_runtime",
            "t437e_trigger_trace_projects_recent_lifecycle_and_outcome_events",
        ),
        (
            "trigger_runtime",
            "t437f_trigger_degrades_when_target_selected_frame_becomes_stale",
        ),
    ]);
    let actual = parse_top_level_functions(
        "trigger_runtime",
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/trigger_runtime.rs"),
        )
        .expect("trigger_runtime source should be readable"),
    )
    .into_iter()
    .filter(|function| function.body.contains("unique_home()"))
    .map(|function| qualified_function_name(&function))
    .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "trigger_runtime should only bypass the grouped browser template for the authority-isolated trigger cases"
    );
}

#[test]
fn grouped_browser_scenarios_default_to_one_managed_session() {
    let zero_managed_session_exceptions = expected_function_set(&[(
        "state_workflow",
        "t310_311_external_attach_lifecycle_grouped_scenario",
    )]);

    for function in browser_backed_e2e_functions()
        .into_iter()
        .filter(|function| function.name.contains("_grouped_"))
    {
        let managed_session_count = function
            .body
            .matches("ManagedBrowserSession::new()")
            .count();
        let qualified = qualified_function_name(&function);
        if zero_managed_session_exceptions.contains(&qualified) {
            assert_eq!(
                managed_session_count, 0,
                "{qualified} is a whitelisted external-attach scenario and must not bootstrap a managed browser"
            );
            continue;
        }
        assert_eq!(
            managed_session_count, 1,
            "{qualified} must reuse exactly one managed browser session so grouped scenarios keep a single cold start by default"
        );
    }
}

#[path = "e2e/foundation.rs"]
mod foundation;

#[path = "e2e/integration.rs"]
mod integration;

#[path = "e2e/state_workflow.rs"]
mod state_workflow;

#[path = "e2e/runtime_integration.rs"]
mod runtime_integration;

#[path = "e2e/workflow_extract_storage.rs"]
mod workflow_extract_storage;

#[path = "e2e/trigger_runtime.rs"]
mod trigger_runtime;

#[path = "e2e/orchestration_runtime.rs"]
mod orchestration_runtime;
