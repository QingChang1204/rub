//! End-to-end tests for rub CLI.
//! These tests require Chrome to be installed.
//! Run with: cargo test --test e2e -- --ignored

use rub_daemon::rub_paths::RubPaths;
use rub_test_harness::fixtures::{DownloadFixtureServer, NetworkInspectionFixtureServer};
use rub_test_harness::server::TestServer;
use serde_json::{Value, json};
use serial_test::serial;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

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

#[derive(Clone, Debug)]
struct HomeCleanupObservation {
    daemon_root_pids: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupVerification {
    Verified,
    SkippedDuringPanic,
}

fn cleanup_registered_artifacts() {
    if std::thread::panicking() {
        return;
    }

    let homes = registered_homes().lock().unwrap().clone();
    let mut failed_homes = Vec::new();
    for home in homes {
        match try_cleanup_home(&home) {
            Ok(CleanupVerification::Verified) => {}
            Ok(CleanupVerification::SkippedDuringPanic) | Err(_) => {
                failed_homes.push(home);
            }
        }
    }

    let external = registered_external_chromes().lock().unwrap().clone();
    let mut failed_external = Vec::new();
    for (pid, profile_dir) in external {
        match try_cleanup_external_chrome(pid, &profile_dir) {
            Ok(CleanupVerification::Verified) => {}
            Ok(CleanupVerification::SkippedDuringPanic) | Err(_) => {
                failed_external.push((pid, profile_dir));
            }
        }
    }

    *registered_homes().lock().unwrap() = failed_homes;
    *registered_external_chromes().lock().unwrap() = failed_external;
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
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir());
    let home = temp_root
        .join(format!(
            "rub-e2e-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
        .to_string_lossy()
        .to_string();
    register_home(&home);
    home
}

fn acquire_managed_browser_lock() -> File {
    let lock_path = std::env::temp_dir().join("rub-e2e-browser.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|error| panic!("failed to open browser lock {:?}: {error}", lock_path));
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(
        rc,
        0,
        "failed to acquire browser lock {:?}: {}",
        lock_path,
        std::io::Error::last_os_error()
    );
    file
}

struct ManagedBrowserSession {
    home: String,
    _browser_lock: File,
}

impl ManagedBrowserSession {
    fn new() -> Self {
        let browser_lock = acquire_managed_browser_lock();
        let home = unique_home();
        prepare_home(&home);
        Self {
            home,
            _browser_lock: browser_lock,
        }
    }

    fn home(&self) -> &str {
        &self.home
    }

    fn cmd(&self) -> Command {
        rub_cmd(self.home())
    }
}

impl Drop for ManagedBrowserSession {
    fn drop(&mut self) {
        cleanup(&self.home);
    }
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

fn default_session_pid_path(home: &str) -> PathBuf {
    RubPaths::new(home).session("default").pid_path()
}

fn session_pid_path(home: &str, session: &str) -> PathBuf {
    RubPaths::new(home).session(session).pid_path()
}

fn cleanup(home: &str) {
    match try_cleanup_home(home) {
        Ok(CleanupVerification::Verified) => {}
        Ok(CleanupVerification::SkippedDuringPanic) => {}
        Err(message) => panic!("{message}"),
    }
}

fn prepare_home(home: &str) {
    if let Err(message) = try_prepare_home(home) {
        panic!("{message}");
    }
}

fn try_prepare_home(home: &str) -> Result<CleanupVerification, String> {
    let observed = observe_home_cleanup(home);
    cleanup_impl(home);
    verify_home_cleanup_complete(home, &observed)
}

fn try_cleanup_home(home: &str) -> Result<CleanupVerification, String> {
    let verification = try_prepare_home(home)?;
    if matches!(verification, CleanupVerification::Verified) {
        unregister_home(home);
    }
    Ok(verification)
}

fn cleanup_impl(home: &str) {
    if !std::path::Path::new(home).exists() {
        return;
    }
    kill_home_process_tree(home);
    if wait_for_home_processes_to_exit(home, Duration::from_secs(5)) {
        let _ = std::fs::remove_dir_all(home);
    }
}

fn observe_home_cleanup(home: &str) -> HomeCleanupObservation {
    let mut daemon_root_pids = daemon_root_pids_for_home(home);
    daemon_root_pids.extend(home_artifact_daemon_root_pids(home));
    daemon_root_pids.sort_unstable();
    daemon_root_pids.dedup();
    HomeCleanupObservation { daemon_root_pids }
}

fn home_artifact_daemon_root_pids(home: &str) -> Vec<u32> {
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

    collect_pid_file_values(Path::new(home), &mut roots);

    roots
}

fn collect_pid_file_values(root: &Path, pids: &mut Vec<u32>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pid_file_values(&path, pids);
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("pid") {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(&path)
            && let Ok(pid) = contents.trim().parse::<u32>()
        {
            pids.push(pid);
        }
    }
}

fn verify_home_cleanup_complete(
    home: &str,
    observed: &HomeCleanupObservation,
) -> Result<CleanupVerification, String> {
    if std::thread::panicking() {
        return Ok(CleanupVerification::SkippedDuringPanic);
    }

    let residues = daemon_processes_for_home(home);
    if !residues.is_empty() {
        return Err(format!(
            "cleanup must not leave daemon residue for home {home}: {:#?}",
            residues
        ));
    }

    let managed_browser_authority_pids = observed_managed_browser_authority_pids(home, observed);
    let browser_residue = managed_browser_authority_pids
        .iter()
        .filter_map(|daemon_pid| {
            let residue = browser_processes_for_daemon_pid(*daemon_pid);
            (!residue.is_empty()).then_some((*daemon_pid, residue))
        })
        .collect::<Vec<_>>();
    if !browser_residue.is_empty() {
        return Err(format!(
            "cleanup must not leave managed browser residue for home {home}: {browser_residue:#?}"
        ));
    }

    let managed_profile_residue = managed_browser_authority_pids
        .iter()
        .map(|daemon_pid| managed_browser_profile_dir_for_daemon(*daemon_pid))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if !managed_profile_residue.is_empty() {
        return Err(format!(
            "cleanup must remove managed browser profile residue for home {home}: {managed_profile_residue:#?}"
        ));
    }

    if Path::new(home).exists() {
        return Err(format!("cleanup must remove test home directory {home}"));
    }

    Ok(CleanupVerification::Verified)
}

fn observed_managed_browser_authority_pids(
    home: &str,
    observed: &HomeCleanupObservation,
) -> Vec<u32> {
    let daemon_snapshot = process_command_snapshot();
    observed
        .daemon_root_pids
        .iter()
        .copied()
        .filter(|daemon_pid| {
            daemon_pid_matches_home_in_snapshot(&daemon_snapshot, *daemon_pid, home)
                || !browser_processes_for_daemon_pid(*daemon_pid).is_empty()
                || managed_browser_profile_dir_for_daemon(*daemon_pid).exists()
        })
        .collect()
}

fn managed_browser_profile_dir_for_daemon(daemon_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("rub-chrome-{daemon_pid}"))
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
    let mut roots = home_artifact_daemon_root_pids(home);
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
            prepare_home(path.to_string_lossy().as_ref());
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

fn try_cleanup_external_chrome(
    pid: u32,
    profile_dir: &Path,
) -> Result<CleanupVerification, String> {
    kill_process_tree_from_roots(&[pid]);
    let _ = std::fs::remove_dir_all(profile_dir);
    verify_external_chrome_cleanup_complete(pid, profile_dir)
}

fn verify_external_chrome_cleanup_complete(
    pid: u32,
    profile_dir: &Path,
) -> Result<CleanupVerification, String> {
    if std::thread::panicking() {
        return Ok(CleanupVerification::SkippedDuringPanic);
    }
    if process_alive(pid) {
        return Err(format!(
            "cleanup must not leave external Chrome process residue: pid {pid} still appears alive"
        ));
    }
    let process_residue = external_chrome_processes_for_profile(profile_dir);
    if !process_residue.is_empty() {
        return Err(format!(
            "cleanup must not leave external Chrome process residue for profile {}: {process_residue:#?}",
            profile_dir.display()
        ));
    }
    if profile_dir.exists() {
        return Err(format!(
            "cleanup must remove external Chrome profile directory {}",
            profile_dir.display()
        ));
    }
    Ok(CleanupVerification::Verified)
}

fn external_chrome_processes_for_profile(profile_dir: &Path) -> Vec<String> {
    let profile_token = profile_dir.display().to_string();
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
            trimmed
                .contains(&profile_token)
                .then_some(trimmed.to_string())
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
    use std::io::{ErrorKind, Read};

    let request = format!(
        "GET /json/version HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        origin.trim_start_matches("http://")
    );
    TcpStream::connect(origin.trim_start_matches("http://"))
        .and_then(|mut stream| {
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            stream.write_all(request.as_bytes())?;
            let deadline = std::time::Instant::now() + timeout;
            let mut response = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        response.extend_from_slice(&chunk[..read]);
                        let text = String::from_utf8_lossy(&response);
                        if text.contains(" 200 ")
                            && text.contains("webSocketDebuggerUrl")
                            && text.contains("Browser")
                        {
                            break;
                        }
                    }
                    Err(error)
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                    }
                    Err(error) => return Err(error),
                }

                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
            Ok(String::from_utf8_lossy(&response).into_owned())
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

fn spawn_external_chrome_with_urls(
    urls: &[&str],
) -> Option<(std::process::Child, String, PathBuf)> {
    let browser_path = browser_binary_for_external_tests()?;
    let port = free_tcp_port();
    let profile_dir = std::env::temp_dir().join(format!(
        "rub-external-chrome-{}-{}",
        std::process::id(),
        HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut command = Command::new(browser_path);
    command.args([
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
    ]);
    if urls.is_empty() {
        command.arg("about:blank");
    } else {
        command.args(urls);
    }
    let child = command
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

fn spawn_external_chrome(
    initial_url: Option<&str>,
) -> Option<(std::process::Child, String, PathBuf)> {
    match initial_url {
        Some(url) => spawn_external_chrome_with_urls(&[url]),
        None => spawn_external_chrome_with_urls(&[]),
    }
}

fn terminate_external_chrome(child: &mut std::process::Child, profile_dir: &Path) {
    let pid = child.id();
    let _ = child.kill();
    for _ in 0..20 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    let _ = std::fs::remove_dir_all(profile_dir);
    match verify_external_chrome_cleanup_complete(pid, profile_dir) {
        Ok(CleanupVerification::Verified) => unregister_external_chrome(pid),
        Ok(CleanupVerification::SkippedDuringPanic) => {}
        Err(message) => panic!("{message}"),
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
