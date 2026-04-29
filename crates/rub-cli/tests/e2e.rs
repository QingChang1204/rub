//! End-to-end tests for rub CLI.
//! These tests require Chrome to be installed.
//! They are browser-backed opt-in coverage, not standing default validation.
//! Run with: cargo test --test e2e -- --ignored

use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;
use rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID;
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use rub_test_harness::assert::checked_command_result;
pub(crate) use rub_test_harness::browser_session::*;
use rub_test_harness::fixtures::{DownloadFixtureServer, NetworkInspectionFixtureServer};
use rub_test_harness::server::TestServer;
use serde_json::{Value, json};
use serial_test::serial;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn rub_binary() -> String {
    resolve_test_rub_binary_path(
        std::env::var("RUB_TEST_BINARY").ok().as_deref(),
        std::env::var("CARGO_BIN_EXE_rub").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
    )
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
    let parsed = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON: {e}\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    });
    let _ = checked_command_result(&parsed);
    assert_cli_exit_matches_command_result(output, &parsed);
    parsed
}

fn assert_cli_exit_matches_command_result(output: &std::process::Output, parsed: &Value) {
    let success = parsed
        .get("success")
        .and_then(Value::as_bool)
        .expect("CommandResult success must be a boolean");
    assert_eq!(
        output.status.success(),
        success,
        "CLI process exit status must match CommandResult.success; status={:?}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_poll_success(surface: &str, parsed: &Value) {
    assert_eq!(
        parsed["success"], true,
        "{surface} polling command returned a non-success CommandResult; fail closed instead of waiting through semantic/projection failure: {parsed}"
    );
}

fn doctor_result(json: &serde_json::Value) -> &serde_json::Value {
    &json["data"]["result"]
}

fn doctor_runtime(json: &serde_json::Value) -> &serde_json::Value {
    &json["data"]["runtime"]
}

fn prepare_rub_home(home: &str) {
    std::fs::create_dir_all(home).unwrap();
}

fn prepare_rub_home_secrets(home: &str, contents: &str) {
    prepare_rub_home(home);
    write_secure_secrets_env(&PathBuf::from(home).join("secrets.env"), contents);
}

fn open_and_assert_success(mut cmd: Command, url: &str) -> serde_json::Value {
    let opened = parse_json(&cmd.args(["open", url]).output().unwrap());
    assert_eq!(opened["success"], true, "{opened}");
    opened
}

fn assert_input_secret_references(json: &serde_json::Value, expected: &[(&str, &str)]) {
    let references = &json["data"]["input_secret_references"];
    assert_eq!(references["count"], expected.len(), "{json}");
    let items = references["items"]
        .as_array()
        .expect("input_secret_references.items must be an array");
    assert_eq!(items.len(), expected.len(), "{json}");
    for (index, (reference, effective_source)) in expected.iter().enumerate() {
        assert_eq!(items[index]["reference"], *reference, "{json}");
        assert_eq!(
            items[index]["effective_source"], *effective_source,
            "{json}"
        );
    }
}

fn wait_for_no_live_sessions(home: &str) -> serde_json::Value {
    wait_for_no_live_sessions_with_timeout(home, Duration::from_secs(5))
}

fn wait_for_no_live_sessions_with_timeout(home: &str, timeout: Duration) -> serde_json::Value {
    let mut last_sessions = None;
    let mut last_observed = None;
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let sessions = parse_json(&rub_cmd(home).arg("sessions").output().unwrap());
        assert_poll_success("sessions", &sessions);
        let observed = observe_home_cleanup(home);
        if sessions["data"]["result"]["items"]
            .as_array()
            .is_some_and(|items| items.is_empty())
            && observed.daemon_root_pids.is_empty()
        {
            return sessions;
        }
        last_sessions = Some(sessions);
        last_observed = Some(observed);
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for close --all to release live session authority within {:?}; last_sessions={:?}; last_daemon_root_pids={:?}",
        timeout,
        last_sessions,
        last_observed
            .as_ref()
            .map(|observed| &observed.daemon_root_pids),
    );
}

fn teardown_and_cleanup(home: &str) {
    let torn_down = parse_json(
        &rub_cmd(home)
            .args(["--timeout", "90000", "teardown"])
            .output()
            .unwrap(),
    );
    assert_eq!(torn_down["success"], true, "{torn_down}");
    cleanup(home);
}

fn remove_orchestration_rule(home: &str, session: &str, rule_id: u64) {
    let removed = parse_json(
        &rub_cmd(home)
            .args([
                "--session",
                session,
                "orchestration",
                "remove",
                &rule_id.to_string(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["success"], true, "{removed}");
}

fn bind_current_and_remember_alias(
    mut bind_cmd: Command,
    mut remember_cmd: Command,
    binding_alias: &str,
    remembered_alias: &str,
    kind: &str,
) {
    let bound = parse_json(
        &bind_cmd
            .args(["binding", "bind-current", binding_alias])
            .output()
            .unwrap(),
    );
    assert_eq!(bound["success"], true, "{bound}");

    let remembered = parse_json(
        &remember_cmd
            .args([
                "binding",
                "remember",
                remembered_alias,
                "--binding",
                binding_alias,
                "--kind",
                kind,
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(remembered["success"], true, "{remembered}");
}

fn assert_binding_result_auth(
    json: &serde_json::Value,
    mode: &str,
    alias: &str,
    created_via: &str,
    auth_input_mode: &str,
    capture_fence: Option<&str>,
) {
    assert_eq!(json["success"], true, "{json}");
    assert_eq!(json["data"]["result"]["mode"], mode, "{json}");
    assert_eq!(json["data"]["result"]["binding"]["alias"], alias, "{json}");
    assert_eq!(
        json["data"]["result"]["binding"]["auth_provenance"]["created_via"], created_via,
        "{json}"
    );
    assert_eq!(
        json["data"]["result"]["binding"]["auth_provenance"]["auth_input_mode"], auth_input_mode,
        "{json}"
    );
    match capture_fence {
        Some(expected) => assert_eq!(
            json["data"]["result"]["binding"]["auth_provenance"]["capture_fence"], expected,
            "{json}"
        ),
        None => assert!(
            json["data"]["result"]["binding"]["auth_provenance"]["capture_fence"].is_null(),
            "{json}"
        ),
    }
}

fn assert_binding_capture_candidate(
    json: &serde_json::Value,
    status: &str,
    capture_fence: Option<&str>,
    persistence_policy: Option<&str>,
    durability_scope: Option<&str>,
    reattachment_mode: Option<&str>,
) {
    assert_eq!(
        json["data"]["result"]["capture_candidate"]["capture_fence"]["status"], status,
        "{json}"
    );
    match capture_fence {
        Some(expected) => assert_eq!(
            json["data"]["result"]["capture_candidate"]["capture_fence"]["capture_fence"], expected,
            "{json}"
        ),
        None => assert!(
            json["data"]["result"]["capture_candidate"]["capture_fence"]["capture_fence"].is_null(),
            "{json}"
        ),
    }
    if let Some(expected) = persistence_policy {
        assert_eq!(
            json["data"]["result"]["capture_candidate"]["durability"]["persistence_policy"],
            expected,
            "{json}"
        );
    }
    if let Some(expected) = durability_scope {
        assert_eq!(
            json["data"]["result"]["capture_candidate"]["durability"]["durability_scope"], expected,
            "{json}"
        );
    }
    if let Some(expected) = reattachment_mode {
        assert_eq!(
            json["data"]["result"]["capture_candidate"]["durability"]["reattachment_mode"],
            expected,
            "{json}"
        );
    }
}

fn wait_for_pending_dialog(home: &str) -> serde_json::Value {
    for _ in 0..40 {
        let out = rub_cmd(home).arg("dialog").output().unwrap();
        let json = parse_json(&out);
        assert_poll_success("dialog", &json);
        if !json["data"]["runtime"]["pending_dialog"].is_null() {
            return json;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("Timed out waiting for pending dialog");
}

fn wait_for_tabs_count(home: &str, count: u64) -> serde_json::Value {
    for _ in 0..50 {
        let out = parse_json(&rub_cmd(home).arg("tabs").output().unwrap());
        assert_poll_success("tabs", &out);
        if out["data"]["result"]["items"]
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
        assert_poll_success("trigger list", &out);
        if let Some(trigger) = out["data"]["result"]["items"]
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

fn wait_for_trigger_last_action_status(home: &str, id: u64, expected: &str) -> serde_json::Value {
    for _ in 0..80 {
        let out = parse_json(&rub_cmd(home).args(["trigger", "list"]).output().unwrap());
        assert_poll_success("trigger list", &out);
        if let Some(trigger) = out["data"]["result"]["items"]
            .as_array()
            .and_then(|triggers| {
                triggers
                    .iter()
                    .find(|trigger| trigger["id"].as_u64() == Some(id))
            })
            && trigger["last_action_result"]["status"].as_str() == Some(expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Timed out waiting for trigger {id} to publish last_action_result.status '{expected}'");
}

fn wait_for_trigger_unavailable_reason(home: &str, id: u64, expected: &str) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..80 {
        let out = parse_json(&rub_cmd(home).args(["trigger", "list"]).output().unwrap());
        assert_poll_success("trigger list", &out);
        last = out.clone();
        if let Some(trigger) = out["data"]["result"]["items"]
            .as_array()
            .and_then(|triggers| {
                triggers
                    .iter()
                    .find(|trigger| trigger["id"].as_u64() == Some(id))
            })
            && trigger_unavailable_reason_matches(trigger["unavailable_reason"].as_str(), expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for trigger {id} to publish unavailable_reason '{expected}'; last output: {last}"
    );
}

fn trigger_unavailable_reason_matches(actual: Option<&str>, expected: &str) -> bool {
    actual == Some(expected)
        || (expected == "target_tab_missing"
            && actual == Some("source_tab_projection_degraded_and_target_missing"))
}

fn assert_trigger_status_remains(
    home: &str,
    id: u64,
    expected: &str,
    duration: Duration,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + duration;
    loop {
        let out = parse_json(&rub_cmd(home).args(["trigger", "list"]).output().unwrap());
        assert_poll_success("trigger list", &out);
        let trigger = out["data"]["result"]["items"]
            .as_array()
            .and_then(|triggers| {
                triggers
                    .iter()
                    .find(|trigger| trigger["id"].as_u64() == Some(id))
            })
            .unwrap_or_else(|| {
                panic!("trigger {id} should remain present while verifying steady status: {out}")
            });
        assert_eq!(trigger["status"].as_str(), Some(expected), "{out}");
        if std::time::Instant::now() >= deadline {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn assert_trigger_remains_unavailable_without_action(
    home: &str,
    id: u64,
    expected_reason: &str,
    duration: Duration,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + duration;
    loop {
        let out = parse_json(&rub_cmd(home).args(["trigger", "list"]).output().unwrap());
        assert_poll_success("trigger list", &out);
        let trigger = out["data"]["result"]["items"]
            .as_array()
            .and_then(|triggers| triggers.iter().find(|trigger| trigger["id"].as_u64() == Some(id)))
            .unwrap_or_else(|| {
                panic!("trigger {id} should remain present while verifying unavailable continuity: {out}")
            });
        assert_eq!(trigger["status"].as_str(), Some("armed"), "{out}");
        assert!(
            trigger_unavailable_reason_matches(
                trigger["unavailable_reason"].as_str(),
                expected_reason
            ),
            "{out}"
        );
        assert!(trigger["last_action_result"].is_null(), "{out}");
        if std::time::Instant::now() >= deadline {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_runtime_frame_degraded_reason(
    home: &str,
    expected_status: &str,
    expected_reason: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let out = parse_json(&rub_cmd(home).args(["runtime", "frame"]).output().unwrap());
        assert_poll_success("runtime frame", &out);
        if out["data"]["runtime"]["status"].as_str() == Some(expected_status)
            && out["data"]["runtime"]["degraded_reason"].as_str() == Some(expected_reason)
        {
            return out;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "Timed out waiting for runtime frame status '{expected_status}' with reason '{expected_reason}'"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn wait_for_pid_exit(pid: i32, timeout: Duration) {
    wait_until(timeout, || {
        let result = unsafe { libc::kill(pid, 0) };
        if result == 0 {
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    });
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
        assert_poll_success("orchestration list", &out);
        last = out.clone();
        if let Some(rule) = out["data"]["result"]["items"]
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
        assert_poll_success("orchestration list", &out);
        if let Some(rule) = out["data"]["result"]["items"]
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

fn wait_for_orchestration_condition_evidence_summary(
    home: &str,
    session: &str,
    id: u64,
    expected_status: &str,
    expected_summary: Option<&str>,
) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..80 {
        let out = parse_json(
            &rub_cmd(home)
                .args(["--session", session, "orchestration", "list"])
                .output()
                .unwrap(),
        );
        assert_poll_success("orchestration list", &out);
        last = out.clone();
        if let Some(rule) = out["data"]["result"]["items"]
            .as_array()
            .and_then(|rules| rules.iter().find(|rule| rule["id"].as_u64() == Some(id)))
            && rule["status"].as_str() == Some(expected_status)
        {
            let actual_summary = rule["last_condition_evidence"]["summary"].as_str();
            if actual_summary == expected_summary {
                return out;
            }
            if expected_summary.is_none() && rule["last_condition_evidence"].is_null() {
                return out;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} in session '{session}' to reach status '{expected_status}' with last_condition_evidence {:?}; last list output: {last}",
        expected_summary
    );
}

fn wait_for_orchestration_cooldown_to_expire(
    home: &str,
    session: &str,
    id: u64,
) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..80 {
        let out = parse_json(
            &rub_cmd(home)
                .args(["--session", session, "orchestration", "list"])
                .output()
                .unwrap(),
        );
        assert_poll_success("orchestration list", &out);
        last = out.clone();
        if let Some(rule) = out["data"]["result"]["items"]
            .as_array()
            .and_then(|rules| rules.iter().find(|rule| rule["id"].as_u64() == Some(id)))
        {
            let cooldown_expired = rule["execution_policy"]["cooldown_until_ms"]
                .as_u64()
                .is_none_or(|deadline| {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_millis() as u64)
                        .unwrap_or(u64::MAX);
                    now_ms >= deadline
                });
            if cooldown_expired {
                return out;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} in session '{session}' to leave cooldown; last list output: {last}"
    );
}

fn wait_for_orchestration_cooldown_to_renew(
    home: &str,
    session: &str,
    id: u64,
    previous_deadline_ms: u64,
) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..80 {
        let out = parse_json(
            &rub_cmd(home)
                .args(["--session", session, "orchestration", "list"])
                .output()
                .unwrap(),
        );
        assert_poll_success("orchestration list", &out);
        last = out.clone();
        if let Some(rule) = out["data"]["result"]["items"]
            .as_array()
            .and_then(|rules| rules.iter().find(|rule| rule["id"].as_u64() == Some(id)))
            && rule["last_result"]["status"].as_str() == Some("fired")
            && rule["execution_policy"]["cooldown_until_ms"]
                .as_u64()
                .is_some_and(|deadline| deadline > previous_deadline_ms)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} in session '{session}' to renew cooldown after prior deadline {previous_deadline_ms}; last list output: {last}"
    );
}

fn wait_for_session_in_flight_count(
    runtime: &tokio::runtime::Runtime,
    home: &str,
    session_id: &str,
    expected: u64,
    timeout: Duration,
) -> IpcResponse {
    let socket_path = registry_socket_path_by_session_id(home, session_id);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let request = IpcRequest::new("_handshake", serde_json::json!({}), 1_000)
            .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
            .expect("fixed handshake probe command_id should stay valid");
        let response = send_bound_ipc_request(runtime, &socket_path, session_id, &request);
        if response.status == rub_ipc::protocol::ResponseStatus::Success
            && response
                .data
                .as_ref()
                .and_then(|data| data.get("in_flight_count"))
                .and_then(serde_json::Value::as_u64)
                == Some(expected)
        {
            return response;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "Timed out waiting for session '{session_id}' to publish in_flight_count={expected}; last handshake response: {response:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[allow(clippy::too_many_arguments)]
fn wait_for_orchestration_probe_match(
    runtime: &tokio::runtime::Runtime,
    home: &str,
    session_id: &str,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: serde_json::Value,
    expected_matched: bool,
    timeout: Duration,
) -> IpcResponse {
    let socket_path = registry_socket_path_by_session_id(home, session_id);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let request = IpcRequest::new(
            "_orchestration_probe",
            json!({
                "tab_target_id": tab_target_id,
                "frame_id": frame_id,
                "condition": condition,
                "after_sequence": 0,
                "last_observed_drop_count": 0,
            }),
            1_000,
        );
        let response = send_bound_ipc_request(runtime, &socket_path, session_id, &request);
        if response.status == rub_ipc::protocol::ResponseStatus::Success
            && response
                .data
                .as_ref()
                .and_then(|data| data.get("matched"))
                .and_then(serde_json::Value::as_bool)
                == Some(expected_matched)
        {
            return response;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "Timed out waiting for _orchestration_probe in session '{session_id}' to report matched={expected_matched}; last response: {response:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

fn registry_socket_path_by_session_id(home: &str, session_id: &str) -> String {
    rub_daemon::session::read_registry(Path::new(home))
        .unwrap_or_else(|error| panic!("registry should be readable for {home}: {error}"))
        .sessions
        .into_iter()
        .find(|entry| entry.session_id == session_id)
        .unwrap_or_else(|| panic!("session '{session_id}' should be present in registry"))
        .socket_path
}

fn assert_no_startup_session_residue(home: &str, session_name: &str) {
    let registry = rub_daemon::session::read_registry(Path::new(home))
        .unwrap_or_else(|error| panic!("registry should remain readable for {home}: {error}"));
    assert!(
        registry.sessions.is_empty(),
        "failed startup must not leave registry authority behind for home {home}: {registry:#?}"
    );

    let session_paths = RubPaths::new(home).session(session_name);
    assert!(
        !session_paths.session_dir().exists(),
        "failed startup must not leave session directory residue for home {home}: {}",
        session_paths.session_dir().display()
    );
    assert!(
        !session_paths.projection_dir().exists(),
        "failed startup must not leave projection directory residue for home {home}: {}",
        session_paths.projection_dir().display()
    );
    for socket_path in session_paths.socket_paths() {
        assert!(
            !socket_path.exists(),
            "failed startup must not leave socket residue for home {home}: {}",
            socket_path.display()
        );
    }
    for pid_path in session_paths.pid_paths() {
        assert!(
            !pid_path.exists(),
            "failed startup must not leave pid residue for home {home}: {}",
            pid_path.display()
        );
    }
    for lock_path in session_paths.lock_paths() {
        assert!(
            !lock_path.exists(),
            "failed startup must not leave lock residue for home {home}: {}",
            lock_path.display()
        );
    }
}

fn send_bound_ipc_request(
    runtime: &tokio::runtime::Runtime,
    socket_path: &str,
    daemon_session_id: &str,
    request: &IpcRequest,
) -> IpcResponse {
    runtime.block_on(async {
        let mut client = IpcClient::connect(Path::new(socket_path))
            .await
            .unwrap_or_else(|error| panic!("ipc connect should succeed for {socket_path}: {error}"))
            .bind_daemon_session_id(daemon_session_id.to_string())
            .unwrap_or_else(|error| {
                panic!("ipc client should bind daemon_session_id '{daemon_session_id}': {error}")
            });
        client.send(request).await.unwrap_or_else(|error| {
            panic!("ipc send should succeed for daemon session '{daemon_session_id}': {error}")
        })
    })
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
        e2e_home_owner_pid(Path::new("/tmp/rub-temp-owned-e2e-12345-019d4e48-a9a3")),
        Some(12345)
    );
    assert_eq!(
        e2e_home_owner_pid(Path::new("/tmp/rub-e2e-12345-019d4e48-a9a3")),
        Some(12345),
        "stale cleanup must still identify legacy E2E homes"
    );
    assert_eq!(e2e_home_owner_pid(Path::new("/tmp/not-rub-home")), None);
    assert_eq!(e2e_home_owner_pid(Path::new("/tmp/rub-e2e-bad-uuid")), None);
}

#[test]
fn daemon_root_pids_for_home_falls_back_to_process_table_shape() {
    let ps_home = format!("/tmp/rub-temp-owned-e2e-{}-test", std::process::id());
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
        managed_profile_dirs: Vec::new(),
    };

    let verification = verify_home_cleanup_complete(&home_string, &observed)
        .expect("unrelated pid should not fail cleanup verification");
    assert_eq!(verification, CleanupVerification::Verified);
}

#[test]
fn verify_home_cleanup_complete_detects_managed_profile_residue_for_observed_daemon() {
    let fake_pid = 400_000u32 + (uuid::Uuid::now_v7().as_u128() % 100_000) as u32;
    let home = format!("/tmp/rub-e2e-cleanup-residue-{fake_pid}");
    let profile_dir = rub_core::managed_profile::projected_managed_profile_path_for_session(
        "sess-cleanup-residue",
    );
    let _ = std::fs::remove_dir_all(&profile_dir);
    rub_core::managed_profile::sync_temp_owned_managed_profile_marker(&profile_dir, true).unwrap();

    let result = verify_home_cleanup_complete(
        &home,
        &HomeCleanupObservation {
            daemon_root_pids: vec![fake_pid],
            managed_profile_dirs: vec![profile_dir.clone()],
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

fn mounted_e2e_modules() -> std::collections::BTreeSet<String> {
    MOUNTED_E2E_MODULES
        .iter()
        .map(|module| (*module).to_string())
        .collect()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rub-cli crate should live under workspace/crates")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn read_workspace_file(path: &str) -> String {
    let path = workspace_root().join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn ci_e2e_shard_modules(ci_workflow: &str) -> std::collections::BTreeSet<String> {
    let modules = ci_workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("modules:"))
        .flat_map(|raw| {
            raw.trim()
                .trim_matches('"')
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(
        !modules.is_empty(),
        "CI workflow should declare E2E modules"
    );
    let unique = modules
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        modules.len(),
        "CI E2E shard modules must not be duplicated"
    );
    unique
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
            "state_workflow",
            "t310a_external_attach_accepts_multi_tab_browser_with_unique_active_tab_authority",
        ),
        (
            "state_workflow",
            "t310b_failed_external_attach_does_not_leave_daemon_residue",
        ),
        ("state_workflow", "t360_mutual_exclusion"),
        (
            "state_workflow",
            "t362_new_session_invalid_cdp_url_reports_connection_failure",
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
            "t310a_external_attach_accepts_multi_tab_browser_with_unique_active_tab_authority",
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

#[test]
fn browser_backed_e2e_source_files_match_mounted_modules() {
    let scanned = browser_backed_e2e_functions()
        .into_iter()
        .map(|function| function.module)
        .collect::<std::collections::BTreeSet<_>>();
    let mounted = mounted_e2e_modules();
    assert_eq!(
        scanned, mounted,
        "tests/e2e/*.rs source files must exactly match the mounted #[path] module list so new browser-backed suites cannot drift out of compilation"
    );
}

#[test]
fn e2e_guardrail_browser_backed_e2e_ci_shards_match_mounted_modules() {
    let ci_modules = ci_e2e_shard_modules(&read_workspace_file(".github/workflows/ci.yml"));
    let mounted = mounted_e2e_modules();
    assert_eq!(
        ci_modules, mounted,
        "CI E2E shard modules must match the mounted #[path] module list so ignored browser suites cannot drift out of CI"
    );
}

#[test]
fn e2e_guardrail_ci_retry_classifier_fails_closed_for_semantic_failures() {
    let ci = read_workspace_file(".github/workflows/ci.yml");
    assert!(ci.contains("classify_e2e_failure"), "{ci}");
    assert!(ci.contains("semantic_or_assertion"), "{ci}");
    assert!(ci.contains("retryable_chrome_setup"), "{ci}");
    assert!(ci.contains("NON_RETRYABLE_FAILURE=1"), "{ci}");
    assert!(
        !ci.contains("FAILED_TESTS=") && !ci.contains("Retrying failed tests"),
        "CI must not retry failed test names because assertion/projection/idempotency failures must fail closed"
    );
}

#[test]
fn e2e_guardrail_release_runs_frozen_baseline_suite_before_dist() {
    let release = read_workspace_file(".github/workflows/release.yml");
    assert!(release.contains("frozen-baseline-guardrails"), "{release}");
    assert!(
        release.contains("cargo test -p rub-cli --bin rub doc_contract"),
        "{release}"
    );
    assert!(
        release.contains("cargo test -p rub-cli --test e2e e2e_guardrail"),
        "{release}"
    );
    assert!(
        release.contains("cargo test -p rub-test-harness root_fixture"),
        "{release}"
    );
    assert!(
        release.contains("plan:\n    needs:\n      - frozen-baseline-guardrails"),
        "release dist plan must be fenced behind frozen-baseline guardrails"
    );
}

#[test]
fn e2e_guardrail_polling_helpers_fail_closed_on_non_success_results() {
    let e2e_source = read_workspace_file("crates/rub-cli/tests/e2e.rs");
    let trigger_runtime = read_workspace_file("crates/rub-cli/tests/e2e/trigger_runtime.rs");
    assert!(
        e2e_source.contains("fn assert_poll_success"),
        "{e2e_source}"
    );
    assert!(
        trigger_runtime.contains("assert_poll_success(\"tabs\", &tabs)"),
        "{trigger_runtime}"
    );
    assert!(
        !trigger_runtime.contains("tabs[\"success\"] != true"),
        "trigger_runtime tab polling must not sleep through non-success CommandResult"
    );
    assert!(
        !e2e_source.contains("out[\"success\"] == true\n            && let Some(rule)"),
        "orchestration polling helpers must not hide non-success CommandResult while waiting for final success"
    );
}

#[test]
fn wait_for_no_live_sessions_guardrail_uses_observed_authority_release() {
    let source =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e.rs"))
            .expect("e2e source should be readable");
    assert!(
        source.contains("observe_home_cleanup(home)")
            && source.contains(".is_some_and(|items| items.is_empty())")
            && source.contains("observed.daemon_root_pids.is_empty()"),
        "wait_for_no_live_sessions guardrail must require both empty sessions projection and observed daemon authority release"
    );
}

#[test]
fn professional_workflow_docs_keep_manual_non_regression_disclaimer() {
    let workflow_readme = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("rub-cli crate should live under workspace/crates")
            .parent()
            .expect("workspace root")
            .join("tests/professional-workflows/README.md"),
    )
    .expect("professional workflow readme should be readable");
    assert!(
        workflow_readme.contains("manual workflow assets")
            && (workflow_readme.contains("not cargo-managed regression tests")
                || workflow_readme.contains("standing CI closure proof"))
            && workflow_readme.contains(
                "same thing as proving the product-level `close` / `cleanup` / `teardown` fence"
            ),
        "manual workflow docs must stay explicit that they are not standing regression proof"
    );

    let professional_plan = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("rub-cli crate should live under workspace/crates")
            .parent()
            .expect("workspace root")
            .join("docs/antigravity/rub-professional-test-plan.md"),
    )
    .expect("professional test plan should be readable");
    assert!(
        (professional_plan.contains("参考") || professional_plan.contains("手工"))
            && professional_plan.contains("不是默认 CI standing regression guardrail")
            && professional_plan.contains("自动化 closure proof"),
        "professional test plan must describe these workflows as manual/reference assets instead of automated regression proof"
    );
}

macro_rules! mount_e2e_modules {
    ($(($module:ident, $path:literal)),+ $(,)?) => {
        const MOUNTED_E2E_MODULES: &[&str] = &[$(stringify!($module)),+];
        $(
            #[path = $path]
            mod $module;
        )+
    };
}

mount_e2e_modules!(
    (foundation, "e2e/foundation.rs"),
    (integration, "e2e/integration.rs"),
    (state_workflow, "e2e/state_workflow.rs"),
    (runtime_integration, "e2e/runtime_integration.rs"),
    (workflow_extract_storage, "e2e/workflow_extract_storage.rs"),
    (trigger_runtime, "e2e/trigger_runtime.rs"),
    (orchestration_runtime, "e2e/orchestration_runtime.rs"),
);
