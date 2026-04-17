use super::{CleanupVerification, HomeCleanupObservation, rub_cmd, unregister_home};
use rub_core::managed_profile::{
    extract_managed_profile_path_from_command, is_temp_owned_managed_profile_path,
    managed_profile_paths_equivalent,
};
use rub_core::process::{
    ProcessInfo, extract_flag_value, process_has_ancestor,
    process_snapshot as collect_process_snapshot,
};
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::handshake::{
    SocketSessionIdentityConfirmation as SocketIdentityConfirmation,
    confirm_daemon_session_identity,
};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
struct HomeDaemonAuthority {
    pid: u32,
    session_name: Option<String>,
    session_id: Option<String>,
    socket_path: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
}

pub fn default_session_pid_path(home: &str) -> PathBuf {
    RubPaths::new(home).session("default").pid_path()
}

pub fn session_pid_path(home: &str, session: &str) -> PathBuf {
    RubPaths::new(home).session(session).pid_path()
}

pub fn cleanup(home: &str) {
    match try_cleanup_home(home) {
        Ok(CleanupVerification::Verified | CleanupVerification::SkippedDuringPanic) => {}
        Ok(CleanupVerification::VerifiedWithHarnessFallback) => unreachable!(
            "strict cleanup must surface harness fallback as an error before returning"
        ),
        Err(message) => panic!("{message}"),
    }
}

pub fn prepare_home(home: &str) {
    if let Err(message) = try_prepare_home(home) {
        panic!("{message}");
    }
}

pub(super) fn try_cleanup_home(home: &str) -> Result<CleanupVerification, String> {
    let verification = try_cleanup_home_allow_harness_fallback(home)?;
    require_product_teardown_verification(home, verification)
}

pub(super) fn try_cleanup_home_allow_harness_fallback(
    home: &str,
) -> Result<CleanupVerification, String> {
    let verification = try_prepare_home(home)?;
    if matches!(
        verification,
        CleanupVerification::Verified | CleanupVerification::VerifiedWithHarnessFallback
    ) {
        unregister_home(home);
    }
    Ok(verification)
}

fn require_product_teardown_verification(
    home: &str,
    verification: CleanupVerification,
) -> Result<CleanupVerification, String> {
    match verification {
        CleanupVerification::Verified | CleanupVerification::SkippedDuringPanic => Ok(verification),
        CleanupVerification::VerifiedWithHarnessFallback => Err(format!(
            "browser-backed cleanup for {home} required harness fallback; product teardown must verify without harness-owned cleanup by default"
        )),
    }
}

fn try_prepare_home(home: &str) -> Result<CleanupVerification, String> {
    let observed = observe_home_cleanup(home);
    let cleanup_path = cleanup_impl(home, &observed);
    verify_home_cleanup_complete(home, &observed)?;
    Ok(cleanup_verification_for_path(cleanup_path))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupPath {
    ProductTeardownVerified,
    HarnessFallbackVerified,
}

fn cleanup_verification_for_path(path: CleanupPath) -> CleanupVerification {
    match path {
        CleanupPath::ProductTeardownVerified => CleanupVerification::Verified,
        CleanupPath::HarnessFallbackVerified => CleanupVerification::VerifiedWithHarnessFallback,
    }
}

fn remove_dir_all_best_effort(path: &str) {
    let _ = std::fs::remove_dir_all(path);
}

struct CleanupOps {
    request_product_teardown: fn(&str, Duration) -> bool,
    request_cleanup_runtime: fn(&str, Duration) -> bool,
    wait_for_exit: fn(&str, Duration) -> bool,
    wait_for_managed_browser_authority_release: fn(&HomeCleanupObservation, Duration) -> bool,
    kill_home_process_tree: fn(&str),
    reap_managed_browser_authority_residue: fn(&HomeCleanupObservation, Duration),
    remove_dir_all: fn(&str),
}

fn cleanup_impl(home: &str, observed: &HomeCleanupObservation) -> CleanupPath {
    cleanup_impl_with(
        home,
        observed,
        CleanupOps {
            request_product_teardown,
            request_cleanup_runtime,
            wait_for_exit: wait_for_home_processes_to_exit,
            wait_for_managed_browser_authority_release,
            kill_home_process_tree,
            reap_managed_browser_authority_residue,
            remove_dir_all: remove_dir_all_best_effort,
        },
    )
}

fn cleanup_impl_with(
    home: &str,
    observed: &HomeCleanupObservation,
    ops: CleanupOps,
) -> CleanupPath {
    if !Path::new(home).exists() {
        return CleanupPath::ProductTeardownVerified;
    }
    if (ops.request_product_teardown)(home, Duration::from_secs(5)) {
        let managed_browser_released =
            (ops.wait_for_managed_browser_authority_release)(observed, Duration::from_secs(5));
        let home_removed = !Path::new(home).exists();
        if managed_browser_released && home_removed {
            return CleanupPath::ProductTeardownVerified;
        }
    }

    (ops.kill_home_process_tree)(home);
    let _ = (ops.request_cleanup_runtime)(home, Duration::from_secs(5));
    if (ops.wait_for_exit)(home, Duration::from_secs(5)) {
        let _ = (ops.request_cleanup_runtime)(home, Duration::from_secs(5));
        (ops.reap_managed_browser_authority_residue)(observed, Duration::from_secs(5));
        (ops.remove_dir_all)(home);
    }
    CleanupPath::HarnessFallbackVerified
}

fn request_product_teardown(home: &str, timeout: Duration) -> bool {
    let timeout_ms = timeout.as_millis().to_string();
    let output = rub_cmd(home)
        .args(["--timeout", &timeout_ms, "teardown"])
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
    let mut daemon_root_pids = proven_home_daemon_root_pids(home);
    if daemon_root_pids.is_empty() {
        daemon_root_pids.extend(
            home_artifact_daemon_authorities(home)
                .into_iter()
                .map(|authority| authority.pid),
        );
    }
    daemon_root_pids.sort_unstable();
    daemon_root_pids.dedup();
    let mut managed_profile_dirs = observed_managed_profile_dirs(home, &daemon_root_pids);
    managed_profile_dirs.sort();
    managed_profile_dirs.dedup();
    HomeCleanupObservation {
        daemon_root_pids,
        managed_profile_dirs,
    }
}

fn observed_managed_profile_dirs(home: &str, daemon_root_pids: &[u32]) -> Vec<PathBuf> {
    let authorities = home_artifact_daemon_authorities(home);
    let daemon_root_pid_set: HashSet<u32> = daemon_root_pids.iter().copied().collect();
    let snapshot = collect_process_snapshot().unwrap_or_default();
    let mut managed_profile_dirs = BTreeSet::new();

    for authority in authorities {
        if !daemon_root_pid_set.contains(&authority.pid) {
            continue;
        }
        if let Some(user_data_dir) = authority
            .user_data_dir
            .filter(|path| is_temp_owned_managed_profile_path(path))
        {
            managed_profile_dirs.insert(user_data_dir);
        }
    }

    for daemon_pid in daemon_root_pids {
        managed_profile_dirs.extend(managed_profile_dirs_for_daemon_pid_in_snapshot(
            &snapshot,
            *daemon_pid,
        ));
        let legacy_path = legacy_managed_browser_profile_dir_for_daemon(*daemon_pid);
        if is_temp_owned_managed_profile_path(&legacy_path) {
            managed_profile_dirs.insert(legacy_path);
        }
    }

    managed_profile_dirs.into_iter().collect()
}

fn reap_managed_browser_authority_residue(observed: &HomeCleanupObservation, timeout: Duration) {
    if wait_for_managed_browser_authority_release(observed, timeout) {
        return;
    }
    kill_managed_browser_authority_residue(observed);
    let _ = wait_for_managed_browser_authority_release(observed, timeout);
}

fn home_artifact_daemon_authorities(home: &str) -> Vec<HomeDaemonAuthority> {
    let mut authorities = std::collections::BTreeMap::<u32, HomeDaemonAuthority>::new();
    if let Ok(registry) = rub_daemon::session::read_registry(Path::new(home)) {
        for entry in registry.sessions {
            merge_home_daemon_authority(
                &mut authorities,
                HomeDaemonAuthority {
                    pid: entry.pid,
                    session_name: Some(entry.session_name),
                    session_id: Some(entry.session_id),
                    socket_path: Some(PathBuf::from(entry.socket_path)),
                    user_data_dir: entry.user_data_dir.map(PathBuf::from),
                },
            );
        }
    }

    let pid_file = default_session_pid_path(home);
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = pid_str.trim().parse::<u32>()
    {
        merge_home_daemon_authority(
            &mut authorities,
            HomeDaemonAuthority {
                pid,
                session_name: Some("default".to_string()),
                session_id: None,
                socket_path: None,
                user_data_dir: None,
            },
        );
    }

    collect_pid_file_authorities(Path::new(home), home, &mut authorities);
    authorities.into_values().collect()
}

fn merge_home_daemon_authority(
    authorities: &mut std::collections::BTreeMap<u32, HomeDaemonAuthority>,
    authority: HomeDaemonAuthority,
) {
    let entry = authorities
        .entry(authority.pid)
        .or_insert_with(|| HomeDaemonAuthority {
            pid: authority.pid,
            ..HomeDaemonAuthority::default()
        });
    if entry.session_name.is_none() {
        entry.session_name = authority.session_name;
    }
    if entry.session_id.is_none() {
        entry.session_id = authority.session_id;
    }
    if entry.socket_path.is_none() {
        entry.socket_path = authority.socket_path;
    }
    if entry.user_data_dir.is_none() {
        entry.user_data_dir = authority.user_data_dir;
    }
}

fn collect_pid_file_authorities(
    root: &Path,
    home: &str,
    authorities: &mut std::collections::BTreeMap<u32, HomeDaemonAuthority>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pid_file_authorities(&path, home, authorities);
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("pid") {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(&path)
            && let Ok(pid) = contents.trim().parse::<u32>()
        {
            let session_id = session_id_from_pid_path(&path);
            let session_name = session_name_from_pid_path(&path);
            let socket_path = session_id
                .as_deref()
                .map(|session_id| runtime_socket_path_for_session_id(home, session_id));
            merge_home_daemon_authority(
                authorities,
                HomeDaemonAuthority {
                    pid,
                    session_name,
                    session_id,
                    socket_path,
                    user_data_dir: None,
                },
            );
        }
    }
}

fn runtime_socket_path_for_session_id(home: &str, session_id: &str) -> PathBuf {
    RubPaths::new(home)
        .session_runtime("default", session_id)
        .socket_path()
}

fn session_id_from_pid_path(path: &Path) -> Option<String> {
    let mut components = path.components().rev();
    let file_name = components.next()?.as_os_str().to_str()?;
    let session_id = components.next()?.as_os_str().to_str()?;
    let by_id = components.next()?.as_os_str().to_str()?;
    let sessions = components.next()?.as_os_str().to_str()?;
    (file_name == "daemon.pid" && by_id == "by-id" && sessions == "sessions")
        .then(|| session_id.to_string())
}

fn session_name_from_pid_path(path: &Path) -> Option<String> {
    let mut components = path.components().rev();
    let file_name = components.next()?.as_os_str().to_str()?;
    let session_name = components.next()?.as_os_str().to_str()?;
    let parent = components.next()?.as_os_str().to_str()?;
    (file_name == "daemon.pid" && parent == "sessions" && session_name != "by-id")
        .then(|| session_name.to_string())
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

    let managed_browser_authority_dirs = observed_managed_browser_authority_dirs(home, observed);
    let browser_residue = managed_browser_authority_dirs
        .iter()
        .filter_map(|profile_dir| {
            let residue = browser_processes_for_profile_dir(profile_dir);
            (!residue.is_empty()).then_some((profile_dir.display().to_string(), residue))
        })
        .collect::<Vec<_>>();
    if !browser_residue.is_empty() {
        return Err(format!(
            "cleanup must not leave managed browser residue for home {home}: {browser_residue:#?}"
        ));
    }

    let managed_profile_residue = managed_browser_authority_dirs
        .iter()
        .filter(|path| path.exists())
        .cloned()
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

fn observed_managed_browser_authority_dirs(
    home: &str,
    observed: &HomeCleanupObservation,
) -> Vec<PathBuf> {
    let daemon_snapshot = process_command_snapshot();
    let mut managed_profile_dirs = BTreeSet::new();
    for profile_dir in &observed.managed_profile_dirs {
        managed_profile_dirs.insert(profile_dir.clone());
    }

    for daemon_pid in &observed.daemon_root_pids {
        if daemon_pid_matches_home_in_snapshot(&daemon_snapshot, *daemon_pid, home)
            || !browser_processes_for_daemon_pid(*daemon_pid).is_empty()
        {
            let dynamic_dirs = managed_profile_dirs_for_daemon_pid(*daemon_pid);
            if dynamic_dirs.is_empty() {
                let legacy_path = legacy_managed_browser_profile_dir_for_daemon(*daemon_pid);
                if legacy_path.exists() {
                    managed_profile_dirs.insert(legacy_path);
                }
            } else {
                managed_profile_dirs.extend(dynamic_dirs);
            }
        }
    }

    managed_profile_dirs.into_iter().collect()
}

fn wait_for_managed_browser_authority_release(
    observed: &HomeCleanupObservation,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let residue = observed
            .managed_profile_dirs
            .iter()
            .filter(|profile_dir| {
                !browser_processes_for_profile_dir(profile_dir).is_empty() || profile_dir.exists()
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
    for profile_dir in &observed.managed_profile_dirs {
        let residue = browser_processes_for_profile_dir(profile_dir);
        if !residue.is_empty() {
            kill_process_tree_from_roots(&residue);
        }
        let _ = std::fs::remove_dir_all(profile_dir);
    }
}

fn legacy_managed_browser_profile_dir_for_daemon(daemon_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("rub-chrome-{daemon_pid}"))
}

pub fn wait_for_home_processes_to_exit(home: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
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
    let command_snapshot = process_command_snapshot();
    let mut roots = proven_home_daemon_root_pids_with_snapshot(home, &command_snapshot);
    if roots.is_empty() {
        return;
    }
    roots.sort_unstable();
    roots.dedup();
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
            if daemon_command_matches_home(trimmed, home) {
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
            .is_some_and(|line_pid| line_pid == pid && daemon_command_matches_home(trimmed, home))
    })
}

fn proven_home_daemon_root_pids(home: &str) -> Vec<u32> {
    let snapshot = process_command_snapshot();
    proven_home_daemon_root_pids_with_snapshot(home, &snapshot)
}

fn proven_home_daemon_root_pids_with_snapshot(home: &str, snapshot: &str) -> Vec<u32> {
    let mut roots = Vec::new();
    let authorities = home_artifact_daemon_authorities(home);
    for authority in &authorities {
        if home_daemon_authority_matches_snapshot(snapshot, home, authority) {
            roots.push(authority.pid);
        }
    }
    if roots.is_empty() {
        roots.extend(daemon_root_pids_for_home(home));
    }
    roots
}

fn home_daemon_authority_matches_snapshot(
    snapshot: &str,
    home: &str,
    authority: &HomeDaemonAuthority,
) -> bool {
    let command_matches = snapshot.lines().any(|line| {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        parts
            .next()
            .and_then(|raw_pid| raw_pid.parse::<u32>().ok())
            .is_some_and(|line_pid| {
                line_pid == authority.pid
                    && daemon_command_matches_home_authority(trimmed, home, authority)
            })
    });
    if !command_matches {
        return false;
    }
    !matches!(
        authority_socket_identity_confirmation(authority),
        SocketIdentityConfirmation::ConfirmedMismatch
    )
}

fn daemon_command_matches_home_authority(
    command: &str,
    home: &str,
    authority: &HomeDaemonAuthority,
) -> bool {
    if !daemon_command_matches_home(command, home) {
        return false;
    }
    if let Some(session_name) = authority.session_name.as_deref()
        && extract_flag_value(command, "--session").as_deref() != Some(session_name)
    {
        return false;
    }
    if let Some(session_id) = authority.session_id.as_deref()
        && extract_flag_value(command, "--session-id").as_deref() != Some(session_id)
    {
        return false;
    }
    true
}

fn daemon_command_matches_home(command: &str, home: &str) -> bool {
    command.contains("__daemon")
        && extract_flag_value(command, "--rub-home").as_deref() == Some(home)
}

fn authority_socket_identity_confirmation(
    authority: &HomeDaemonAuthority,
) -> SocketIdentityConfirmation {
    let (Some(socket_path), Some(session_id)) = (
        authority.socket_path.as_deref(),
        authority.session_id.as_deref(),
    ) else {
        return SocketIdentityConfirmation::Inconclusive;
    };
    socket_identity_confirmation(socket_path, session_id)
}

fn socket_identity_confirmation(
    socket_path: &Path,
    expected_session_id: &str,
) -> SocketIdentityConfirmation {
    confirm_daemon_session_identity(socket_path, expected_session_id)
        .unwrap_or(SocketIdentityConfirmation::Inconclusive)
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

fn managed_profile_dirs_for_daemon_pid_in_snapshot(
    snapshot: &[ProcessInfo],
    daemon_pid: u32,
) -> BTreeSet<PathBuf> {
    let daemon_roots = HashSet::from([daemon_pid]);
    snapshot
        .iter()
        .filter(|process| process_has_ancestor(snapshot, process.pid, &daemon_roots))
        .filter_map(|process| extract_managed_profile_path_from_command(&process.command))
        .filter(|profile_dir| is_temp_owned_managed_profile_path(profile_dir))
        .collect()
}

fn managed_profile_dirs_for_daemon_pid(daemon_pid: u32) -> Vec<PathBuf> {
    let snapshot = collect_process_snapshot().unwrap_or_default();
    managed_profile_dirs_for_daemon_pid_in_snapshot(&snapshot, daemon_pid)
        .into_iter()
        .collect()
}

fn browser_processes_for_profile_dir(profile_dir: &Path) -> Vec<u32> {
    collect_process_snapshot()
        .unwrap_or_default()
        .into_iter()
        .filter(|process| {
            extract_managed_profile_path_from_command(&process.command)
                .as_deref()
                .is_some_and(|candidate| {
                    candidate == profile_dir
                        || managed_profile_paths_equivalent(candidate, profile_dir)
                })
        })
        .map(|process| process.pid)
        .collect()
}

pub fn browser_processes_for_daemon_pid(daemon_pid: u32) -> Vec<u32> {
    let mut processes = BTreeSet::new();
    for profile_dir in managed_profile_dirs_for_daemon_pid(daemon_pid) {
        processes.extend(browser_processes_for_profile_dir(&profile_dir));
    }
    if processes.is_empty() {
        let legacy_path = legacy_managed_browser_profile_dir_for_daemon(daemon_pid);
        if is_temp_owned_managed_profile_path(&legacy_path) {
            processes.extend(browser_processes_for_profile_dir(&legacy_path));
        }
    }
    processes.into_iter().collect()
}

#[cfg(test)]
mod tests;
