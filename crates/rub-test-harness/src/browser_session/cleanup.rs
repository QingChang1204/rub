use super::{CleanupVerification, HomeCleanupObservation, rub_cmd, unregister_home};
use rub_daemon::rub_paths::RubPaths;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn default_session_pid_path(home: &str) -> PathBuf {
    RubPaths::new(home).session("default").pid_path()
}

pub fn session_pid_path(home: &str, session: &str) -> PathBuf {
    RubPaths::new(home).session(session).pid_path()
}

pub fn cleanup(home: &str) {
    match try_cleanup_home(home) {
        Ok(CleanupVerification::Verified) => {}
        Ok(CleanupVerification::SkippedDuringPanic) => {}
        Err(message) => panic!("{message}"),
    }
}

pub fn prepare_home(home: &str) {
    if let Err(message) = try_prepare_home(home) {
        panic!("{message}");
    }
}

pub(super) fn try_cleanup_home(home: &str) -> Result<CleanupVerification, String> {
    let verification = try_prepare_home(home)?;
    if matches!(verification, CleanupVerification::Verified) {
        unregister_home(home);
    }
    Ok(verification)
}

fn try_prepare_home(home: &str) -> Result<CleanupVerification, String> {
    let observed = observe_home_cleanup(home);
    cleanup_impl(home, &observed);
    verify_home_cleanup_complete(home, &observed)
}

fn cleanup_impl(home: &str, observed: &HomeCleanupObservation) {
    if !Path::new(home).exists() {
        return;
    }
    let _ = request_graceful_close_all(home, Duration::from_secs(5));
    let _ = request_cleanup_runtime(home, Duration::from_secs(5));
    if wait_for_home_processes_to_exit(home, Duration::from_secs(5)) {
        let _ = request_cleanup_runtime(home, Duration::from_secs(5));
        wait_for_or_reap_managed_browser_authority_residue(observed, Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(home);
        return;
    }

    kill_home_process_tree(home);
    let _ = request_cleanup_runtime(home, Duration::from_secs(5));
    if wait_for_home_processes_to_exit(home, Duration::from_secs(5)) {
        let _ = request_cleanup_runtime(home, Duration::from_secs(5));
        wait_for_or_reap_managed_browser_authority_residue(observed, Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(home);
    }
}

fn request_graceful_close_all(home: &str, timeout: Duration) -> bool {
    let timeout_ms = timeout.as_millis().to_string();
    let output = rub_cmd(home)
        .args(["--timeout", &timeout_ms, "close", "--all"])
        .output();
    matches!(output, Ok(result) if result.status.success())
}

fn request_cleanup_runtime(home: &str, timeout: Duration) -> bool {
    let timeout_ms = timeout.as_millis().to_string();
    let output = rub_cmd(home)
        .args(["--timeout", &timeout_ms, "cleanup"])
        .output();
    matches!(output, Ok(result) if result.status.success())
}

pub fn observe_home_cleanup(home: &str) -> HomeCleanupObservation {
    let mut daemon_root_pids = daemon_root_pids_for_home(home);
    daemon_root_pids.extend(home_artifact_daemon_root_pids(home));
    daemon_root_pids.sort_unstable();
    daemon_root_pids.dedup();
    HomeCleanupObservation { daemon_root_pids }
}

fn wait_for_or_reap_managed_browser_authority_residue(
    observed: &HomeCleanupObservation,
    timeout: Duration,
) {
    if wait_for_managed_browser_authority_release(observed, timeout) {
        return;
    }
    kill_managed_browser_authority_residue(observed);
    let _ = wait_for_managed_browser_authority_release(observed, timeout);
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

pub fn verify_home_cleanup_complete(
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

fn wait_for_managed_browser_authority_release(
    observed: &HomeCleanupObservation,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let residue = observed
            .daemon_root_pids
            .iter()
            .copied()
            .filter(|daemon_pid| {
                !browser_processes_for_daemon_pid(*daemon_pid).is_empty()
                    || managed_browser_profile_dir_for_daemon(*daemon_pid).exists()
            })
            .count();
        if residue == 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn kill_managed_browser_authority_residue(observed: &HomeCleanupObservation) {
    for daemon_pid in &observed.daemon_root_pids {
        let residue = browser_processes_for_daemon_pid(*daemon_pid);
        if !residue.is_empty() {
            kill_process_tree_from_roots(&residue);
        }
        let _ = std::fs::remove_dir_all(managed_browser_profile_dir_for_daemon(*daemon_pid));
    }
}

pub fn managed_browser_profile_dir_for_daemon(daemon_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("rub-chrome-{daemon_pid}"))
}

pub fn wait_for_home_processes_to_exit(home: &str, timeout: Duration) -> bool {
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

pub fn daemon_processes_for_home(home: &str) -> Vec<String> {
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

pub fn daemon_pid_matches_home_in_snapshot(snapshot: &str, pid: u32, home: &str) -> bool {
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

pub(super) fn sweep_stale_test_homes() {
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

pub fn e2e_home_owner_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let suffix = name.strip_prefix("rub-e2e-")?;
    let pid = suffix.split('-').next()?;
    pid.parse::<u32>().ok()
}

pub(super) fn kill_process_tree_from_roots(roots: &[u32]) {
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

pub(super) fn process_command_snapshot() -> String {
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

pub fn browser_processes_for_daemon_pid(daemon_pid: u32) -> Vec<u32> {
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
