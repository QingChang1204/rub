//! End-to-end tests for rub CLI.
//! These tests require Chrome to be installed.
//! Run with: cargo test --test e2e -- --ignored

mod support;

use rub_daemon::rub_paths::RubPaths;
use rub_test_harness::server::TestServer;
use serde_json::{Value, json};
use serial_test::serial;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;
use support::download_fixture::DownloadFixtureServer;
use support::network_fixture::NetworkInspectionFixtureServer;

static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);
static CLEANUP_HOOK: Once = Once::new();
static REGISTERED_HOMES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static REGISTERED_EXTERNAL_CHROMES: OnceLock<Mutex<Vec<(u32, PathBuf)>>> = OnceLock::new();

fn registered_homes() -> &'static Mutex<Vec<String>> {
    REGISTERED_HOMES.get_or_init(|| Mutex::new(Vec::new()))
}

fn registered_external_chromes() -> &'static Mutex<Vec<(u32, PathBuf)>> {
    REGISTERED_EXTERNAL_CHROMES.get_or_init(|| Mutex::new(Vec::new()))
}

fn install_cleanup_hook() {
    CLEANUP_HOOK.call_once(|| {
        // Detached rub daemons survive an interrupted test process, so sweep
        // stale test homes from dead owners before registering new ones.
        sweep_stale_test_homes();
        register_process_exit_cleanup();
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            cleanup_registered_artifacts();
            previous(info);
        }));
    });
}

fn register_home(home: &str) {
    install_cleanup_hook();
    let mut homes = registered_homes().lock().unwrap();
    if !homes.iter().any(|existing| existing == home) {
        homes.push(home.to_string());
    }
}

fn unregister_home(home: &str) {
    let mut homes = registered_homes().lock().unwrap();
    homes.retain(|existing| existing != home);
}

fn register_external_chrome(pid: u32, profile_dir: &Path) {
    install_cleanup_hook();
    let mut entries = registered_external_chromes().lock().unwrap();
    if !entries.iter().any(|(existing_pid, _)| *existing_pid == pid) {
        entries.push((pid, profile_dir.to_path_buf()));
    }
}

fn unregister_external_chrome(pid: u32) {
    let mut entries = registered_external_chromes().lock().unwrap();
    entries.retain(|(existing_pid, _)| *existing_pid != pid);
}

fn cleanup_registered_artifacts() {
    let homes = registered_homes().lock().unwrap().clone();
    for home in homes {
        cleanup_impl(&home);
    }

    let external = registered_external_chromes().lock().unwrap().clone();
    for (pid, profile_dir) in external {
        kill_process_tree_from_roots(&[pid]);
        let _ = std::fs::remove_dir_all(profile_dir);
    }

    registered_homes().lock().unwrap().clear();
    registered_external_chromes().lock().unwrap().clear();
}

#[cfg(unix)]
extern "C" fn cleanup_registered_artifacts_atexit() {
    cleanup_registered_artifacts();
}

#[cfg(unix)]
fn register_process_exit_cleanup() {
    // The per-test cleanup helpers cover the success path. This atexit hook is
    // the fallback when the test binary exits early before the individual test
    // teardown runs.
    unsafe {
        libc::atexit(cleanup_registered_artifacts_atexit);
    }
}

#[cfg(not(unix))]
fn register_process_exit_cleanup() {}

#[cfg(unix)]
fn write_secure_secrets_env(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn write_secure_secrets_env(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

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

fn unique_home() -> String {
    install_cleanup_hook();
    let home = format!(
        "/tmp/rub-e2e-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    );
    register_home(&home);
    home
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

fn wait_for_orchestration_unavailable_reason(
    home: &str,
    id: u64,
    expected: &str,
) -> serde_json::Value {
    for _ in 0..80 {
        let out = parse_json(
            &rub_cmd(home)
                .args(["--session", "source", "orchestration", "list"])
                .output()
                .unwrap(),
        );
        if out["success"] == true
            && let Some(rule) = out["data"]["result"]["items"]
                .as_array()
                .and_then(|rules| rules.iter().find(|rule| rule["id"].as_u64() == Some(id)))
            && rule["unavailable_reason"].as_str() == Some(expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} to publish unavailable_reason '{expected}'"
    );
}

fn wait_for_orchestration_status(
    home: &str,
    session: &str,
    id: u64,
    expected: &str,
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
            && rule["status"].as_str() == Some(expected)
        {
            return out;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "Timed out waiting for orchestration rule {id} in session '{session}' to reach status '{expected}'"
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

fn cleanup(home: &str) {
    cleanup_impl(home);
}

fn default_session_pid_path(home: &str) -> PathBuf {
    RubPaths::new(home).session("default").pid_path()
}

fn cleanup_impl(home: &str) {
    unregister_home(home);
    if !std::path::Path::new(home).exists() {
        return;
    }
    kill_home_process_tree(home);
    if wait_for_home_processes_to_exit(home, Duration::from_secs(5)) {
        let _ = std::fs::remove_dir_all(home);
    }
}

fn wait_for_home_processes_to_exit(home: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        kill_home_process_tree(home);
        if daemon_processes_for_home(home).is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn kill_home_process_tree(home: &str) {
    let mut roots = Vec::new();
    let registry_path = format!("{home}/registry.json");
    if let Ok(contents) = std::fs::read_to_string(&registry_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents)
        && let Some(sessions) = json["sessions"].as_array()
    {
        for session in sessions {
            if let Some(pid) = session["pid"].as_u64() {
                roots.push(pid as u32);
            }
        }
    }

    let pid_file = default_session_pid_path(home);
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = pid_str.trim().parse::<u32>()
    {
        roots.push(pid);
    }

    // Startup can fail before the registry or pid file commits. Fall back to
    // the live process table so interrupted tests still reclaim detached
    // daemons for the owning RUB_HOME.
    roots.extend(daemon_root_pids_for_home(home));

    if roots.is_empty() {
        return;
    }

    roots.sort_unstable();
    roots.dedup();
    let command_snapshot = process_command_snapshot();
    roots.retain(|pid| daemon_pid_matches_home_in_snapshot(&command_snapshot, *pid, home));
    kill_process_tree_from_roots(&roots);
}

fn daemon_processes_for_home(home: &str) -> Vec<String> {
    let output = Command::new("ps")
        .args(["-Ao", "pid=,command="])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.contains("rub __daemon") && trimmed.contains(home) {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn daemon_root_pids_for_home(home: &str) -> Vec<u32> {
    daemon_processes_for_home(home)
        .into_iter()
        .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
        .collect()
}

fn daemon_pid_matches_home_in_snapshot(snapshot: &str, pid: u32, home: &str) -> bool {
    snapshot.lines().any(|line| {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        parts
            .next()
            .and_then(|raw_pid| raw_pid.parse::<u32>().ok())
            .is_some_and(|line_pid| {
                line_pid == pid && trimmed.contains("rub __daemon") && trimmed.contains(home)
            })
    })
}

fn sweep_stale_test_homes() {
    let mut seen = std::collections::HashSet::new();
    for root in rub_daemon::rub_paths::temp_roots() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("rub-e2e-") && !rub_daemon::rub_paths::is_temp_owned_home(&path) {
                continue;
            }
            let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
            if !seen.insert(canonical) {
                continue;
            }
            let owner_pid = rub_daemon::rub_paths::read_temp_home_owner_pid(&path)
                .or_else(|| e2e_home_owner_pid(&path));
            if owner_pid.is_some_and(process_alive) {
                continue;
            }
            let path_str = path.to_string_lossy();
            if owner_pid.is_none() && !daemon_root_pids_for_home(path_str.as_ref()).is_empty() {
                continue;
            }
            cleanup_impl(path.to_string_lossy().as_ref());
        }
    }
}

fn e2e_home_owner_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let suffix = name.strip_prefix("rub-e2e-")?;
    let pid = suffix.split('-').next()?;
    pid.parse::<u32>().ok()
}

fn kill_process_tree_from_roots(roots: &[u32]) {
    if roots.is_empty() {
        return;
    }

    let snapshot = process_snapshot();
    let children_by_parent = {
        let mut map: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
        for (pid, ppid) in &snapshot {
            map.entry(*ppid).or_default().push(*pid);
        }
        map
    };

    let mut all_pids = std::collections::BTreeSet::new();
    let mut stack = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if !all_pids.insert(pid) {
            continue;
        }
        if let Some(children) = children_by_parent.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }

    for pid in &all_pids {
        unsafe {
            libc::kill(*pid as i32, libc::SIGTERM);
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    for pid in &all_pids {
        if process_alive(*pid) {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }
}

fn process_snapshot() -> Vec<(u32, u32)> {
    let Ok(output) = Command::new("ps")
        .args(["-Ao", "pid=,ppid=,command="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse::<u32>().ok()?;
            let ppid = parts.next()?.parse::<u32>().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

fn process_command_snapshot() -> String {
    let Ok(output) = Command::new("ps")
        .args(["-Ao", "pid=,command="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return String::new();
    };
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn browser_processes_for_daemon_pid(daemon_pid: u32) -> Vec<u32> {
    let profile_token = format!("rub-chrome-{daemon_pid}");
    let Ok(output) = Command::new("ps")
        .args(["-Ao", "pid=,command="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.contains(&profile_token) {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let pid = parts.next()?.parse::<u32>().ok()?;
            Some(pid)
        })
        .collect()
}

fn wait_until<F>(timeout: Duration, mut predicate: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(predicate(), "Timed out waiting for condition");
}

fn free_tcp_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn wait_for_tcp_endpoint(addr: &str, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for endpoint {addr}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_cdp_http_ready(origin: &str, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    let url = origin.trim_end_matches('/').to_string();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let per_attempt_timeout = remaining.min(Duration::from_millis(500));
        let ready = probe_cdp_http_ready_once(&url, per_attempt_timeout);

        if ready {
            return;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for CDP discovery endpoint {origin}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn probe_cdp_http_ready_once(origin: &str, timeout: Duration) -> bool {
    let request = format!(
        "GET /json/version HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        origin.trim_start_matches("http://")
    );
    TcpStream::connect(origin.trim_start_matches("http://"))
        .and_then(|mut stream| {
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            stream.write_all(request.as_bytes())?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            Ok(response)
        })
        .ok()
        .is_some_and(|response| {
            response.contains(" 200 ")
                && response.contains("webSocketDebuggerUrl")
                && response.contains("Browser")
        })
}

fn browser_binary_for_external_tests() -> Option<String> {
    rub_daemon::health::detect_browser().1
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

fn prepare_fake_profile_env() -> (PathBuf, PathBuf, Vec<(String, String)>) {
    let base = std::env::temp_dir().join(format!(
        "rub-profile-fixture-{}-{}",
        std::process::id(),
        HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let root = profile_root_for_test_base(&base);
    std::fs::create_dir_all(root.join("Default")).unwrap();
    std::fs::write(
        root.join("Local State"),
        r#"{
  "profile": {
    "info_cache": {
      "Default": {
        "name": "Default"
      }
    }
  }
}"#,
    )
    .unwrap();
    let envs = profile_envs_for_test_base(&base);
    (base, root.join("Default"), envs)
}

#[cfg(target_os = "macos")]
fn profile_root_for_test_base(base: &Path) -> PathBuf {
    base.join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome")
}

#[cfg(target_os = "linux")]
fn profile_root_for_test_base(base: &Path) -> PathBuf {
    base.join("xdg").join("google-chrome")
}

#[cfg(target_os = "windows")]
fn profile_root_for_test_base(base: &Path) -> PathBuf {
    base.join("LocalAppData")
        .join("Google")
        .join("Chrome")
        .join("User Data")
}

#[cfg(target_os = "macos")]
fn profile_envs_for_test_base(base: &Path) -> Vec<(String, String)> {
    vec![("HOME".to_string(), base.display().to_string())]
}

#[cfg(target_os = "linux")]
fn profile_envs_for_test_base(base: &Path) -> Vec<(String, String)> {
    vec![(
        "XDG_CONFIG_HOME".to_string(),
        base.join("xdg").display().to_string(),
    )]
}

#[cfg(target_os = "windows")]
fn profile_envs_for_test_base(base: &Path) -> Vec<(String, String)> {
    vec![(
        "LOCALAPPDATA".to_string(),
        base.join("LocalAppData").display().to_string(),
    )]
}

fn spawn_external_chrome(
    initial_url: Option<&str>,
) -> Option<(std::process::Child, String, PathBuf)> {
    let browser_path = browser_binary_for_external_tests()?;
    let port = free_tcp_port();
    let profile_dir = std::env::temp_dir().join(format!(
        "rub-external-chrome-{}-{}",
        std::process::id(),
        HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let child = Command::new(browser_path)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-component-update",
            "--disable-background-networking",
            "--remote-debugging-address=127.0.0.1",
            &format!("--remote-debugging-port={port}"),
            &format!("--user-data-dir={}", profile_dir.display()),
            initial_url.unwrap_or("about:blank"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let cdp_origin = format!("http://127.0.0.1:{port}");
    wait_for_tcp_endpoint(&format!("127.0.0.1:{port}"), Duration::from_secs(15));
    wait_for_cdp_http_ready(&cdp_origin, Duration::from_secs(15));
    register_external_chrome(child.id(), &profile_dir);
    Some((child, cdp_origin, profile_dir))
}

fn terminate_external_chrome(child: &mut std::process::Child) {
    let pid = child.id();
    let _ = child.kill();
    for _ in 0..20 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    unregister_external_chrome(pid);
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
