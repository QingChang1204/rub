use super::{CleanupVerification, register_external_chrome, unregister_external_chrome};
use crate::browser_session::cleanup::{kill_process_tree_from_roots, process_command_snapshot};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) fn try_cleanup_external_chrome(
    pid: u32,
    profile_dir: &Path,
) -> Result<CleanupVerification, String> {
    let command_snapshot = process_command_snapshot();
    if external_chrome_pid_matches_profile_in_snapshot(&command_snapshot, pid, profile_dir) {
        kill_process_tree_from_roots(&[pid]);
    }
    let _ = std::fs::remove_dir_all(profile_dir);
    verify_external_chrome_cleanup_complete(pid, profile_dir)
}

pub fn verify_external_chrome_cleanup_complete(
    pid: u32,
    profile_dir: &Path,
) -> Result<CleanupVerification, String> {
    if std::thread::panicking() {
        return Ok(CleanupVerification::SkippedDuringPanic);
    }
    let command_snapshot = process_command_snapshot();
    if external_chrome_pid_matches_profile_in_snapshot(&command_snapshot, pid, profile_dir) {
        return Err(format!(
            "cleanup must not leave external Chrome process residue for profile {}: pid {pid} is still bound to the registered authority",
            profile_dir.display()
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

pub fn external_chrome_pid_matches_profile_in_snapshot(
    snapshot: &str,
    pid: u32,
    profile_dir: &Path,
) -> bool {
    let profile_token = profile_dir.display().to_string();
    snapshot.lines().any(|line| {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        parts
            .next()
            .and_then(|raw_pid| raw_pid.parse::<u32>().ok())
            .is_some_and(|line_pid| line_pid == pid && trimmed.contains(&profile_token))
    })
}

pub fn wait_until<F>(timeout: Duration, mut predicate: F)
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

pub fn free_tcp_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

pub fn wait_for_tcp_endpoint(addr: &str, timeout: Duration) {
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

pub fn wait_for_cdp_http_ready(origin: &str, timeout: Duration) {
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

pub fn probe_cdp_http_ready_once(origin: &str, timeout: Duration) -> bool {
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

pub fn browser_binary_for_external_tests() -> Option<String> {
    rub_daemon::health::detect_browser().1
}

fn spawn_external_chrome_with_urls(
    urls: &[&str],
) -> Option<(std::process::Child, String, PathBuf)> {
    let browser_path = browser_binary_for_external_tests()?;
    let port = free_tcp_port();
    let profile_dir = std::env::temp_dir().join(format!(
        "rub-external-chrome-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
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

pub fn spawn_external_chrome(
    initial_url: Option<&str>,
) -> Option<(std::process::Child, String, PathBuf)> {
    match initial_url {
        Some(url) => spawn_external_chrome_with_urls(&[url]),
        None => spawn_external_chrome_with_urls(&[]),
    }
}

pub fn terminate_external_chrome(child: &mut std::process::Child, profile_dir: &Path) {
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
