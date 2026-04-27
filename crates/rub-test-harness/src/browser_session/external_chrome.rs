use super::{CleanupVerification, register_external_chrome, unregister_external_chrome};
use crate::browser_session::cleanup::{kill_process_tree_from_roots, process_command_snapshot};
use rub_core::process::{extract_flag_value, is_chromium_process_command};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) fn try_cleanup_external_chrome(
    pid: u32,
    profile_dir: &Path,
) -> Result<CleanupVerification, String> {
    if std::thread::panicking() {
        return Ok(CleanupVerification::SkippedDuringPanic);
    }
    reap_external_chrome_processes_for_profile(profile_dir);
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
            let (_, command) = parse_pid_command_line(trimmed)?;
            command_uses_profile_dir(command, profile_dir).then_some(trimmed.to_string())
        })
        .collect()
}

fn reap_external_chrome_processes_for_profile(profile_dir: &Path) {
    let command_snapshot = process_command_snapshot();
    let profile_pids = external_chrome_pids_for_profile_in_snapshot(&command_snapshot, profile_dir);
    if !profile_pids.is_empty() {
        kill_process_tree_from_roots(&profile_pids);
    }
}

pub fn external_chrome_pid_matches_profile_in_snapshot(
    snapshot: &str,
    pid: u32,
    profile_dir: &Path,
) -> bool {
    snapshot.lines().any(|line| {
        let trimmed = line.trim();
        parse_pid_command_line(trimmed).is_some_and(|(line_pid, command)| {
            line_pid == pid && command_uses_profile_dir(command, profile_dir)
        })
    })
}

fn external_chrome_pids_for_profile_in_snapshot(snapshot: &str, profile_dir: &Path) -> Vec<u32> {
    snapshot
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let (line_pid, command) = parse_pid_command_line(trimmed)?;
            command_uses_profile_dir(command, profile_dir).then_some(line_pid)
        })
        .collect()
}

fn parse_pid_command_line(line: &str) -> Option<(u32, &str)> {
    let trimmed = line.trim();
    let split_at = trimmed.find(char::is_whitespace)?;
    let (pid_raw, command) = trimmed.split_at(split_at);
    let pid = pid_raw.parse::<u32>().ok()?;
    Some((pid, command.trim_start()))
}

fn command_uses_profile_dir(command: &str, profile_dir: &Path) -> bool {
    if !is_chromium_process_command(command) {
        return false;
    }
    let Some(user_data_dir) = extract_flag_value(command, "--user-data-dir") else {
        return false;
    };
    external_profile_paths_equivalent(Path::new(&user_data_dir), profile_dir)
}

fn external_profile_paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    if std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
    {
        return true;
    }
    normalize_private_alias(left) == normalize_private_alias(right)
}

fn normalize_private_alias(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("/private") {
        PathBuf::from("/").join(stripped)
    } else {
        path.to_path_buf()
    }
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

fn devtools_active_port_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("DevToolsActivePort")
}

fn browser_selected_cdp_origin(profile_dir: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(devtools_active_port_path(profile_dir)).ok()?;
    let mut lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let port = lines.next()?.parse::<u16>().ok()?;
    Some(format!("http://127.0.0.1:{port}"))
}

fn wait_for_cdp_endpoint_authority(
    profile_dir: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(origin) = browser_selected_cdp_origin(profile_dir) {
            return Ok(origin);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for browser-authored CDP endpoint for profile {}",
                profile_dir.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_cdp_http_ready(origin: &str, timeout: Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let url = origin.trim_end_matches('/').to_string();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let per_attempt_timeout = remaining.min(Duration::from_millis(500));
        let ready = probe_cdp_http_ready_once(&url, per_attempt_timeout);

        if ready {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for CDP discovery endpoint {origin}"
            ));
        }
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

fn spawn_external_chrome_with_urls(
    urls: &[&str],
) -> Result<Option<(std::process::Child, String, PathBuf)>, String> {
    let Some(browser_path) = browser_binary_for_external_tests() else {
        return Ok(None);
    };
    let profile_dir = std::env::temp_dir().join(format!(
        "rub-external-chrome-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let mut command = Command::new(&browser_path);
    command.args([
        "--headless=new",
        "--disable-gpu",
        "--disable-popup-blocking",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-component-update",
        "--disable-background-networking",
        "--remote-debugging-address=127.0.0.1",
        "--remote-debugging-port=0",
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
        .map_err(|error| {
            format!("Failed to launch helper-owned external Chrome at {browser_path}: {error}")
        })?;

    finalize_external_chrome_spawn(
        child,
        profile_dir,
        wait_for_cdp_endpoint_authority,
        wait_for_cdp_http_ready,
    )
    .map(Some)
}

fn cleanup_failed_external_chrome_spawn(
    child: &mut std::process::Child,
    profile_dir: &Path,
) -> Result<(), String> {
    let pid = child.id();
    let _ = child.kill();
    for _ in 0..20 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    reap_external_chrome_processes_for_profile(profile_dir);
    let _ = std::fs::remove_dir_all(profile_dir);
    finalize_failed_external_cleanup_tracking(
        pid,
        verify_external_chrome_cleanup_complete(pid, profile_dir),
    )
}

fn finalize_failed_external_cleanup_tracking(
    pid: u32,
    verification: Result<CleanupVerification, String>,
) -> Result<(), String> {
    let verification = verification?;
    apply_external_cleanup_tracking(pid, verification);
    Ok(())
}

fn finalize_external_chrome_spawn<WOrigin, WCdp>(
    mut child: std::process::Child,
    profile_dir: PathBuf,
    resolve_origin: WOrigin,
    wait_cdp: WCdp,
) -> Result<(std::process::Child, String, PathBuf), String>
where
    WOrigin: FnOnce(&Path, Duration) -> Result<String, String>,
    WCdp: FnOnce(&str, Duration) -> Result<(), String>,
{
    let pid = child.id();
    register_external_chrome(pid, &profile_dir);
    let readiness_result =
        resolve_origin(&profile_dir, Duration::from_secs(15)).and_then(|cdp_origin| {
            wait_cdp(&cdp_origin, Duration::from_secs(15))?;
            Ok(cdp_origin)
        });
    match readiness_result {
        Ok(cdp_origin) => Ok((child, cdp_origin, profile_dir)),
        Err(error) => {
            let cleanup_result = cleanup_failed_external_chrome_spawn(&mut child, &profile_dir);
            match cleanup_result {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; cleanup after helper-owned external Chrome failure also failed: {cleanup_error}"
                )),
            }
        }
    }
}

pub fn spawn_external_chrome(
    initial_url: Option<&str>,
) -> Result<Option<(std::process::Child, String, PathBuf)>, String> {
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
    reap_external_chrome_processes_for_profile(profile_dir);
    let _ = std::fs::remove_dir_all(profile_dir);
    match verify_external_chrome_cleanup_complete(pid, profile_dir) {
        Ok(verification) => apply_external_cleanup_tracking(pid, verification),
        Err(message) => panic!("{message}"),
    }
}

fn apply_external_cleanup_tracking(pid: u32, verification: CleanupVerification) {
    if matches!(verification, CleanupVerification::Verified) {
        unregister_external_chrome(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CleanupVerification, apply_external_cleanup_tracking, browser_selected_cdp_origin,
        devtools_active_port_path, external_chrome_pid_matches_profile_in_snapshot,
        external_chrome_pids_for_profile_in_snapshot, finalize_external_chrome_spawn,
        finalize_failed_external_cleanup_tracking, probe_cdp_http_ready_once,
        try_cleanup_external_chrome,
    };
    use crate::browser_session::registered_external_chromes;
    use std::io::Write;
    use std::net::TcpListener;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn panic_path_external_cleanup_retains_profile_dir_for_retry_authority() {
        struct CleanupDuringPanic {
            pid: u32,
            profile_dir: std::path::PathBuf,
            result: std::sync::Arc<std::sync::Mutex<Option<CleanupVerification>>>,
        }

        impl Drop for CleanupDuringPanic {
            fn drop(&mut self) {
                let verification = try_cleanup_external_chrome(self.pid, &self.profile_dir)
                    .expect("panic-path external cleanup should not fail");
                *self.result.lock().expect("result lock") = Some(verification);
            }
        }

        let profile_dir = std::env::temp_dir().join(format!(
            "rub-external-chrome-panic-cleanup-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&profile_dir).expect("create external cleanup test profile");
        let verification = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed = verification.clone();

        let _ = std::panic::catch_unwind(|| {
            let _guard = CleanupDuringPanic {
                pid: 41,
                profile_dir: profile_dir.clone(),
                result: observed,
            };
            panic!("trigger external cleanup while panicking");
        });

        assert_eq!(
            *verification.lock().expect("result lock"),
            Some(CleanupVerification::SkippedDuringPanic)
        );
        assert!(
            Path::new(&profile_dir).exists(),
            "panic-path external cleanup must not destructively remove registered profiles during unwind"
        );

        let _ = std::fs::remove_dir_all(profile_dir);
    }

    #[test]
    fn external_chrome_registration_happens_before_readiness_wait() {
        let profile_dir = std::env::temp_dir().join(format!(
            "rub-external-chrome-readiness-registration-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&profile_dir).expect("create readiness registration profile");
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn placeholder external chrome child");
        let pid = child.id();
        let result = finalize_external_chrome_spawn(
            child,
            profile_dir.clone(),
            |resolved_profile_dir, _timeout| {
                let registered = {
                    let entries = registered_external_chromes()
                        .lock()
                        .expect("external entries lock");
                    entries.iter().any(|(entry_pid, entry_profile)| {
                        *entry_pid == pid && entry_profile == &profile_dir
                    })
                };
                assert!(
                    registered,
                    "spawned child must be registered before readiness wait starts"
                );
                assert_eq!(resolved_profile_dir, profile_dir.as_path());
                Err("stop after registration check".to_string())
            },
            |_origin, _timeout| unreachable!("readiness must stop at first wait"),
        );

        assert_eq!(
            result.expect_err("test readiness hook should stop the spawn"),
            "stop after registration check"
        );
        assert!(
            !registered_external_chromes()
                .lock()
                .expect("external entries lock")
                .iter()
                .any(
                    |(entry_pid, entry_profile)| *entry_pid == pid && entry_profile == &profile_dir
                ),
            "failed readiness must not retain registered external chrome authority"
        );
        assert!(
            !profile_dir.exists(),
            "failed readiness must clean up helper-owned external chrome profile residue"
        );
    }

    #[test]
    fn browser_selected_origin_reads_devtools_active_port_file() {
        let profile_dir = std::env::temp_dir().join(format!(
            "rub-external-chrome-devtools-port-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&profile_dir).expect("create devtools port profile");
        std::fs::write(
            devtools_active_port_path(&profile_dir),
            "39291\n/devtools/browser/authority\n",
        )
        .expect("write DevToolsActivePort");

        let origin =
            browser_selected_cdp_origin(&profile_dir).expect("browser-authored endpoint authority");
        assert_eq!(origin, "http://127.0.0.1:39291");

        let _ = std::fs::remove_dir_all(profile_dir);
    }

    #[test]
    fn external_chrome_profile_match_uses_structured_user_data_dir_flag() {
        let profile_dir = std::env::temp_dir().join("rub external chrome structured profile");
        let snapshot = format!(
            r#"  451 Google Chrome --user-data-dir="{}" --remote-debugging-port=0"#,
            profile_dir.display()
        );

        assert!(external_chrome_pid_matches_profile_in_snapshot(
            &snapshot,
            451,
            &profile_dir
        ));
    }

    #[test]
    fn external_chrome_profile_match_ignores_url_or_prefix_substrings_but_keeps_helpers() {
        let profile_dir = std::env::temp_dir().join("rub-external-profile-authority");
        let prefix_collision = std::env::temp_dir().join("rub-external-profile-authority-extra");
        let snapshot = format!(
            r#"
              451 Google Chrome --remote-debugging-port=0 https://example.test/?profile={}
              452 Google Chrome --user-data-dir={} --remote-debugging-port=0
              453 Google Chrome Helper --user-data-dir={} --type=renderer
            "#,
            profile_dir.display(),
            prefix_collision.display(),
            profile_dir.display()
        );

        assert!(!external_chrome_pid_matches_profile_in_snapshot(
            &snapshot,
            451,
            &profile_dir
        ));
        assert!(!external_chrome_pid_matches_profile_in_snapshot(
            &snapshot,
            452,
            &profile_dir
        ));
        assert!(external_chrome_pid_matches_profile_in_snapshot(
            &snapshot,
            453,
            &profile_dir
        ));
        assert_eq!(
            external_chrome_pids_for_profile_in_snapshot(&snapshot, &profile_dir),
            vec![453],
            "cleanup authority must include helper-only residue after the root exits"
        );
    }

    #[test]
    fn external_cleanup_tracking_retains_harness_fallback_for_retry_authority() {
        let pid = 424242;
        let profile_dir = std::env::temp_dir().join(format!(
            "rub-external-chrome-fallback-retry-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&profile_dir).expect("create fallback retry profile");
        {
            let mut entries = registered_external_chromes()
                .lock()
                .expect("external entries lock");
            entries.push((pid, profile_dir.clone()));
        }

        apply_external_cleanup_tracking(pid, CleanupVerification::VerifiedWithHarnessFallback);

        let still_registered = registered_external_chromes()
            .lock()
            .expect("external entries lock")
            .iter()
            .any(|(entry_pid, entry_profile)| *entry_pid == pid && entry_profile == &profile_dir);
        assert!(
            still_registered,
            "harness fallback cleanup must preserve external chrome retry tracking"
        );

        registered_external_chromes()
            .lock()
            .expect("external entries lock")
            .retain(|(entry_pid, _)| *entry_pid != pid);
        let _ = std::fs::remove_dir_all(profile_dir);
    }

    #[test]
    fn failed_spawn_cleanup_tracking_retains_harness_fallback_for_retry_authority() {
        let pid = 434343;
        let profile_dir = std::env::temp_dir().join(format!(
            "rub-external-chrome-failed-spawn-fallback-retry-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&profile_dir).expect("create failed spawn fallback retry profile");
        {
            let mut entries = registered_external_chromes()
                .lock()
                .expect("external entries lock");
            entries.push((pid, profile_dir.clone()));
        }

        finalize_failed_external_cleanup_tracking(
            pid,
            Ok(CleanupVerification::VerifiedWithHarnessFallback),
        )
        .expect("failed spawn cleanup tracking should accept fallback result");

        let still_registered = registered_external_chromes()
            .lock()
            .expect("external entries lock")
            .iter()
            .any(|(entry_pid, entry_profile)| *entry_pid == pid && entry_profile == &profile_dir);
        assert!(
            still_registered,
            "failed spawn cleanup must preserve external chrome retry tracking when only harness fallback verified cleanup"
        );

        registered_external_chromes()
            .lock()
            .expect("external entries lock")
            .retain(|(entry_pid, _)| *entry_pid != pid);
        let _ = std::fs::remove_dir_all(profile_dir);
    }

    #[test]
    fn probe_cdp_http_ready_once_times_out_on_half_open_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n")
                .expect("write partial response");
            std::thread::sleep(Duration::from_millis(300));
        });

        let start = std::time::Instant::now();
        let ready =
            probe_cdp_http_ready_once(&format!("http://{addr}"), Duration::from_millis(100));
        let elapsed = start.elapsed();

        server.join().expect("server thread should join");
        assert!(!ready);
        assert!(elapsed < Duration::from_millis(250));
    }
}
