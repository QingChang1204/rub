//! Cleanup control — stale current-home sessions plus orphaned temporary rub artifacts.

use crate::daemon_ctl::send_existing_request_with_replay_recovery;
use crate::timeout_budget::helpers::mutating_request;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, RubError};
use rub_core::process::{
    ProcessInfo, extract_flag_value, is_chromium_browser_command, is_process_alive,
    process_has_ancestor, process_snapshot as collect_process_snapshot, process_tree,
    tokenize_command,
};
use rub_daemon::rub_paths::{RubPaths, SessionPaths};
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanupResult {
    pub cleaned_stale_sessions: Vec<String>,
    pub kept_active_sessions: Vec<String>,
    pub skipped_unreachable_sessions: Vec<String>,
    pub cleaned_temp_daemons: Vec<String>,
    pub skipped_busy_temp_daemons: Vec<String>,
    pub removed_temp_homes: Vec<String>,
    pub killed_orphan_browser_pids: Vec<u32>,
    pub removed_orphan_browser_profiles: Vec<String>,
}

pub fn project_cleanup_result(rub_home: &Path, result: &CleanupResult) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "runtime_cleanup",
            "rub_home": rub_home.display().to_string(),
        },
        "result": {
            "cleaned_stale_sessions": result.cleaned_stale_sessions,
            "kept_active_sessions": result.kept_active_sessions,
            "skipped_unreachable_sessions": result.skipped_unreachable_sessions,
            "cleaned_temp_daemons": result.cleaned_temp_daemons,
            "skipped_busy_temp_daemons": result.skipped_busy_temp_daemons,
            "removed_temp_homes": result.removed_temp_homes,
            "killed_orphan_browser_pids": result.killed_orphan_browser_pids,
            "removed_orphan_browser_profiles": result.removed_orphan_browser_profiles,
        }
    })
}

#[derive(Debug, Clone)]
struct TempDaemonProcess {
    pid: u32,
    session_name: String,
    session_id: String,
    rub_home: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct UpgradeStatus {
    idle: bool,
}

pub async fn cleanup_runtime(rub_home: &Path, timeout_ms: u64) -> Result<CleanupResult, RubError> {
    let mut result = CleanupResult::default();
    cleanup_current_home_stale(rub_home, &mut result).await?;

    let snapshot = process_snapshot()?;
    let active_temp_homes =
        sweep_temp_daemons(rub_home, &snapshot, timeout_ms, &mut result).await?;

    let post_daemon_snapshot = process_snapshot()?;
    sweep_orphan_temp_browsers(&post_daemon_snapshot, &mut result).await;
    sweep_stale_temp_homes(rub_home, &active_temp_homes, &mut result);

    sort_and_dedup(&mut result.cleaned_stale_sessions);
    sort_and_dedup(&mut result.kept_active_sessions);
    sort_and_dedup(&mut result.skipped_unreachable_sessions);
    sort_and_dedup(&mut result.cleaned_temp_daemons);
    sort_and_dedup(&mut result.skipped_busy_temp_daemons);
    sort_and_dedup(&mut result.removed_temp_homes);
    result.killed_orphan_browser_pids.sort_unstable();
    result.killed_orphan_browser_pids.dedup();
    sort_and_dedup(&mut result.removed_orphan_browser_profiles);

    Ok(result)
}

async fn cleanup_current_home_stale(
    rub_home: &Path,
    result: &mut CleanupResult,
) -> Result<(), RubError> {
    let snapshot = match rub_daemon::session::registry_authority_snapshot(rub_home) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(RubError::domain(
                ErrorCode::DaemonStartFailed,
                format!("Failed to read registry for cleanup: {error}"),
            ));
        }
    };

    for session in snapshot.sessions {
        for entry_snapshot in session.entries {
            let pending_startup = entry_snapshot.is_pending_startup();
            let live_authority = entry_snapshot.is_live_authority();
            let entry = entry_snapshot.entry;
            let session_name = entry.session_name.clone();
            let session_paths =
                RubPaths::new(rub_home).session_runtime(&session_name, &entry.session_id);
            match fetch_upgrade_status_for_session(&session_paths).await {
                Ok(Some(_)) => {
                    result.kept_active_sessions.push(session_name);
                    continue;
                }
                Ok(None) => {}
                Err(_) => {
                    if pending_startup {
                        result.skipped_unreachable_sessions.push(session_name);
                        continue;
                    }
                    if live_authority {
                        result.skipped_unreachable_sessions.push(session_name);
                        continue;
                    }
                }
            }

            if pending_startup {
                result.skipped_unreachable_sessions.push(session_name);
                continue;
            }

            if live_authority {
                result.skipped_unreachable_sessions.push(session_name);
                continue;
            }

            rub_daemon::session::cleanup_projections(rub_home, &entry);
            let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
            result.cleaned_stale_sessions.push(entry.session_name);
        }
    }

    Ok(())
}

async fn sweep_temp_daemons(
    current_rub_home: &Path,
    snapshot: &[ProcessInfo],
    timeout_ms: u64,
    result: &mut CleanupResult,
) -> Result<HashSet<PathBuf>, RubError> {
    let mut active_temp_homes = HashSet::new();

    for daemon in temp_daemon_processes(snapshot) {
        let session_paths = RubPaths::new(&daemon.rub_home)
            .session_runtime(&daemon.session_name, &daemon.session_id);
        if daemon.rub_home == current_rub_home {
            if is_temp_rub_home(&daemon.rub_home) {
                active_temp_homes.insert(daemon.rub_home.clone());
            }
            continue;
        }

        match fetch_upgrade_status_for_session(&session_paths).await {
            Ok(Some((status, socket_path))) => {
                if status.idle {
                    let request =
                        mutating_request("close", serde_json::json!({}), timeout_ms.max(1_000));
                    let key = format!("{}@{}", daemon.session_name, daemon.rub_home.display());
                    match IpcClient::connect(&socket_path).await {
                        Ok(mut client) => {
                            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                            let _ = send_existing_request_with_replay_recovery(
                                &mut client,
                                &request,
                                deadline,
                                &daemon.rub_home,
                                &daemon.session_name,
                                Some(daemon.session_id.as_str()),
                            )
                            .await;
                        }
                        Err(_) => {
                            terminate_revalidated_temp_daemon(&daemon).await;
                        }
                    }
                    wait_for_shutdown_paths(&session_paths.actual_socket_paths()).await;
                    terminate_revalidated_temp_daemon(&daemon).await;
                    cleanup_temp_daemon_registry_state(&daemon);
                    result.cleaned_temp_daemons.push(key);
                } else {
                    active_temp_homes.insert(daemon.rub_home.clone());
                    result.skipped_busy_temp_daemons.push(format!(
                        "{}@{}",
                        daemon.session_name,
                        daemon.rub_home.display()
                    ));
                }
                continue;
            }
            Ok(None) => {}
            Err(_) if is_process_alive(daemon.pid) => {
                active_temp_homes.insert(daemon.rub_home.clone());
                result.skipped_busy_temp_daemons.push(format!(
                    "{}@{}",
                    daemon.session_name,
                    daemon.rub_home.display()
                ));
                continue;
            }
            Err(_) => {}
        }

        terminate_revalidated_temp_daemon(&daemon).await;
        cleanup_temp_daemon_registry_state(&daemon);
        result.cleaned_temp_daemons.push(format!(
            "{}@{}",
            daemon.session_name,
            daemon.rub_home.display()
        ));
    }

    Ok(active_temp_homes)
}

async fn sweep_orphan_temp_browsers(snapshot: &[ProcessInfo], result: &mut CleanupResult) {
    let daemon_pids: HashSet<u32> = snapshot
        .iter()
        .filter(|process| is_rub_daemon_command(&process.command))
        .map(|process| process.pid)
        .collect();
    let mut orphan_pids = HashSet::new();
    let mut orphan_roots = HashSet::new();

    for process in snapshot {
        let Some(root) = extract_temp_browser_root(&process.command) else {
            continue;
        };
        if process_has_ancestor(snapshot, process.pid, &daemon_pids) {
            continue;
        }
        orphan_pids.insert(process.pid);
        orphan_roots.insert(root);
    }

    if !orphan_pids.is_empty() {
        let current_snapshot = process_snapshot().unwrap_or_else(|_| snapshot.to_vec());
        let current_orphan_pids =
            orphan_temp_browser_pids_for_roots(&current_snapshot, &orphan_roots);
        if !current_orphan_pids.is_empty() {
            terminate_process_tree(&current_orphan_pids).await;
        }
        orphan_pids = current_orphan_pids;
    }

    let current_snapshot = process_snapshot().unwrap_or_else(|_| snapshot.to_vec());
    for root in orphan_roots {
        if root_has_live_browser_process(&current_snapshot, &root) {
            continue;
        }
        if std::fs::remove_dir_all(&root).is_ok() {
            result
                .removed_orphan_browser_profiles
                .push(root.display().to_string());
        }
    }

    result
        .killed_orphan_browser_pids
        .extend(orphan_pids.into_iter().collect::<Vec<_>>());
}

fn sweep_stale_temp_homes(
    current_rub_home: &Path,
    active_temp_homes: &HashSet<PathBuf>,
    result: &mut CleanupResult,
) {
    for temp_root in temp_roots() {
        let entries = match std::fs::read_dir(&temp_root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path == current_rub_home
                || !is_temp_rub_home(&path)
                || active_temp_homes.contains(&path)
            {
                continue;
            }
            if std::fs::remove_dir_all(&path).is_ok() {
                result.removed_temp_homes.push(path.display().to_string());
            }
        }
    }
}

fn process_snapshot() -> Result<Vec<ProcessInfo>, RubError> {
    collect_process_snapshot().map_err(|error| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Failed to collect process snapshot: {error}"),
        )
    })
}

fn temp_daemon_processes(snapshot: &[ProcessInfo]) -> Vec<TempDaemonProcess> {
    let mut daemons = Vec::new();
    for process in snapshot {
        if !is_rub_daemon_command(&process.command) {
            continue;
        }
        let Some(home) = extract_flag_value(&process.command, "--rub-home") else {
            continue;
        };
        let rub_home = PathBuf::from(home);
        if !is_temp_rub_home(&rub_home) {
            continue;
        }
        let Some(session_name) = extract_flag_value(&process.command, "--session") else {
            continue;
        };
        let Some(session_id) = extract_flag_value(&process.command, "--session-id") else {
            continue;
        };
        daemons.push(TempDaemonProcess {
            pid: process.pid,
            session_name,
            session_id,
            rub_home,
        });
    }
    daemons
}

fn daemon_process_matches_authority(process: &ProcessInfo, daemon: &TempDaemonProcess) -> bool {
    process.pid == daemon.pid
        && is_rub_daemon_command(&process.command)
        && extract_flag_value(&process.command, "--session").as_deref()
            == Some(daemon.session_name.as_str())
        && extract_flag_value(&process.command, "--session-id").as_deref()
            == Some(daemon.session_id.as_str())
        && extract_flag_value(&process.command, "--rub-home")
            .as_deref()
            .map(PathBuf::from)
            .as_ref()
            == Some(&daemon.rub_home)
}

fn revalidated_temp_daemon_tree(
    snapshot: &[ProcessInfo],
    daemon: &TempDaemonProcess,
) -> Option<HashSet<u32>> {
    snapshot
        .iter()
        .find(|process| daemon_process_matches_authority(process, daemon))
        .map(|process| process_tree(snapshot, process.pid))
}

async fn terminate_revalidated_temp_daemon(daemon: &TempDaemonProcess) {
    let Ok(snapshot) = process_snapshot() else {
        return;
    };
    let Some(tree) = revalidated_temp_daemon_tree(&snapshot, daemon) else {
        return;
    };
    terminate_process_tree(&tree).await;
}

fn orphan_temp_browser_pids_for_roots(
    snapshot: &[ProcessInfo],
    orphan_roots: &HashSet<PathBuf>,
) -> HashSet<u32> {
    let daemon_pids: HashSet<u32> = snapshot
        .iter()
        .filter(|process| is_rub_daemon_command(&process.command))
        .map(|process| process.pid)
        .collect();
    let mut orphan_pids = HashSet::new();
    for process in snapshot {
        let Some(root) = extract_temp_browser_root(&process.command) else {
            continue;
        };
        if !orphan_roots.contains(&root)
            || process_has_ancestor(snapshot, process.pid, &daemon_pids)
        {
            continue;
        }
        orphan_pids.insert(process.pid);
    }
    orphan_pids
}

fn root_has_live_browser_process(snapshot: &[ProcessInfo], root: &Path) -> bool {
    snapshot.iter().any(|process| {
        extract_temp_browser_root(&process.command)
            .as_deref()
            .is_some_and(|candidate| candidate == root)
    })
}

async fn terminate_process_tree(processes: &HashSet<u32>) {
    if processes.is_empty() {
        return;
    }

    for pid in processes {
        unsafe {
            libc::kill(*pid as i32, libc::SIGTERM);
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut survivors = Vec::new();
    for pid in processes {
        if is_process_alive(*pid) {
            survivors.push(*pid);
        }
    }
    for pid in survivors {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

fn is_rub_daemon_command(command: &str) -> bool {
    let mut parts = tokenize_command(command).into_iter();
    let Some(program) = parts.next() else {
        return false;
    };
    let Some(subcommand) = parts.next() else {
        return false;
    };
    if subcommand != "__daemon" {
        return false;
    }
    let basename = Path::new(program.trim_matches('"'))
        .file_name()
        .and_then(|name| name.to_str());
    matches!(basename, Some("rub") | Some("rub.exe"))
}

fn is_temp_rub_home(path: &Path) -> bool {
    rub_daemon::rub_paths::is_temp_owned_home(path)
}

fn extract_temp_browser_root(command: &str) -> Option<PathBuf> {
    if !is_chromium_browser_command(command) {
        return None;
    }
    let user_data_dir = PathBuf::from(extract_flag_value(command, "--user-data-dir")?);
    let file_name = user_data_dir.file_name().and_then(|name| name.to_str())?;
    if file_name.starts_with("rub-chrome-")
        && temp_roots()
            .into_iter()
            .any(|root| user_data_dir.starts_with(root))
    {
        return Some(user_data_dir);
    }
    None
}

fn cleanup_temp_daemon_registry_state(daemon: &TempDaemonProcess) {
    let runtime =
        RubPaths::new(&daemon.rub_home).session_runtime(&daemon.session_name, &daemon.session_id);
    let entry = registry_entry_for_home_session_id(&daemon.rub_home, &daemon.session_id).unwrap_or(
        rub_daemon::session::RegistryEntry {
            session_id: daemon.session_id.clone(),
            session_name: daemon.session_name.clone(),
            pid: daemon.pid,
            socket_path: runtime.socket_path().display().to_string(),
            created_at: String::new(),
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        },
    );
    let _ = rub_daemon::session::deregister_session(&daemon.rub_home, &entry.session_id);
    rub_daemon::session::cleanup_projections(&daemon.rub_home, &entry);
}

fn registry_entry_for_home_session_id(
    rub_home: &Path,
    session_id: &str,
) -> Option<rub_daemon::session::RegistryEntry> {
    rub_daemon::session::read_registry(rub_home)
        .ok()?
        .sessions
        .into_iter()
        .find(|entry| entry.session_id == session_id)
}

async fn fetch_upgrade_status_for_session(
    session_paths: &SessionPaths,
) -> Result<Option<(UpgradeStatus, PathBuf)>, RubError> {
    // Teardown and upgrade checks must target the concrete runtime socket for
    // this session authority. The canonical session-name socket is a shared
    // discovery projection and may already point at a replacement daemon.
    // For session-name-only paths, actual_socket_paths() collapses to the same
    // canonical path, so this stays correct for temp-home discovery.
    for socket_path in session_paths.actual_socket_paths() {
        let mut client = match IpcClient::connect(&socket_path).await {
            Ok(client) => client,
            Err(_) => continue,
        };
        let request = IpcRequest::new("_upgrade_check", serde_json::json!({}), 3_000);
        let response = client
            .send(&request)
            .await
            .map_err(|error| RubError::domain(ErrorCode::IpcProtocolError, error.to_string()))?;
        if response.status == ResponseStatus::Error {
            continue;
        }
        let data = response.data.unwrap_or_default();
        return Ok(Some((
            UpgradeStatus {
                idle: data["idle"].as_bool().unwrap_or(false),
            },
            socket_path,
        )));
    }
    Ok(None)
}

async fn wait_for_shutdown_paths(socket_paths: &[PathBuf]) {
    for _ in 0..20 {
        if socket_paths.iter().all(|socket_path| !socket_path.exists()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn sort_and_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![std::env::temp_dir()];
    let explicit_tmp = PathBuf::from("/tmp");
    if !roots.iter().any(|root| root == &explicit_tmp) {
        roots.push(explicit_tmp);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::{
        CleanupResult, ProcessInfo, cleanup_current_home_stale, cleanup_temp_daemon_registry_state,
        daemon_process_matches_authority, extract_flag_value, extract_temp_browser_root,
        fetch_upgrade_status_for_session, is_rub_daemon_command, is_temp_rub_home,
        process_has_ancestor, process_tree, revalidated_temp_daemon_tree, temp_daemon_processes,
    };
    use rub_daemon::rub_paths::RubPaths;
    use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn unique_temp_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }

    #[test]
    fn temp_rub_home_requires_temp_root_and_owner_marker() {
        let temp_home = unique_temp_home("rub-e2e");
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).unwrap();
        assert!(!is_temp_rub_home(&temp_home));
        std::fs::write(RubPaths::new(&temp_home).temp_home_owner_marker_path(), "").unwrap();
        assert!(is_temp_rub_home(&temp_home));
        assert!(!is_temp_rub_home(&PathBuf::from("/tmp/not-rub")));
        let outside_temp = std::env::current_dir()
            .unwrap()
            .join(format!("rub-non-temp-home-{}", uuid::Uuid::now_v7()));
        let _ = std::fs::remove_dir_all(&outside_temp);
        std::fs::create_dir_all(&outside_temp).unwrap();
        std::fs::write(
            RubPaths::new(&outside_temp).temp_home_owner_marker_path(),
            "",
        )
        .unwrap();
        assert!(!is_temp_rub_home(&outside_temp));
        let _ = std::fs::remove_dir_all(&outside_temp);
        let _ = std::fs::remove_dir_all(temp_home);
    }

    #[test]
    fn temp_rub_home_accepts_generic_mktemp_shape_when_owned() {
        let temp_home = std::env::temp_dir().join(format!(
            "tmp.{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).unwrap();
        assert!(!is_temp_rub_home(&temp_home));
        std::fs::write(RubPaths::new(&temp_home).temp_home_owner_marker_path(), "").unwrap();
        assert!(is_temp_rub_home(&temp_home));
        let _ = std::fs::remove_dir_all(temp_home);
    }

    #[test]
    fn daemon_process_parser_extracts_session_and_home() {
        let temp_home = unique_temp_home("rub-e2e");
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).unwrap();
        std::fs::write(RubPaths::new(&temp_home).temp_home_owner_marker_path(), "").unwrap();
        let snapshot = vec![ProcessInfo {
            pid: 42,
            ppid: 1,
            command: format!(
                "/workspace/target/debug/rub __daemon --session default --session-id sess-old --rub-home {}",
                temp_home.display()
            ),
        }];
        let daemons = temp_daemon_processes(&snapshot);
        assert_eq!(daemons.len(), 1);
        assert_eq!(daemons[0].pid, 42);
        assert_eq!(daemons[0].session_name, "default");
        assert_eq!(daemons[0].session_id, "sess-old");
        assert_eq!(daemons[0].rub_home, temp_home);
        let _ = std::fs::remove_dir_all(temp_home);
    }

    #[test]
    fn revalidated_temp_daemon_tree_requires_full_daemon_authority_match() {
        let daemon = super::TempDaemonProcess {
            pid: 42,
            session_name: "default".to_string(),
            session_id: "sess-old".to_string(),
            rub_home: PathBuf::from("/tmp/rub-home"),
        };
        let matching = ProcessInfo {
            pid: 42,
            ppid: 1,
            command:
                "rub __daemon --session default --session-id sess-old --rub-home /tmp/rub-home"
                    .to_string(),
        };
        let mismatched = ProcessInfo {
            pid: 42,
            ppid: 1,
            command:
                "rub __daemon --session default --session-id sess-new --rub-home /tmp/rub-home"
                    .to_string(),
        };

        assert!(daemon_process_matches_authority(&matching, &daemon));
        assert!(!daemon_process_matches_authority(&mismatched, &daemon));
        assert!(revalidated_temp_daemon_tree(&[matching], &daemon).is_some());
        assert!(revalidated_temp_daemon_tree(&[mismatched], &daemon).is_none());
    }

    #[test]
    fn cleanup_temp_daemon_registry_state_only_removes_matching_session_id() {
        let temp_home = unique_temp_home("rub-cleanup");
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).unwrap();
        std::fs::write(RubPaths::new(&temp_home).temp_home_owner_marker_path(), "").unwrap();

        let old_runtime = RubPaths::new(&temp_home).session_runtime("default", "sess-old");
        let new_runtime = RubPaths::new(&temp_home).session_runtime("default", "sess-new");
        std::fs::create_dir_all(old_runtime.session_dir()).unwrap();
        std::fs::create_dir_all(new_runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            old_runtime
                .startup_committed_path()
                .parent()
                .expect("old startup marker parent"),
        )
        .unwrap();
        std::fs::create_dir_all(
            new_runtime
                .startup_committed_path()
                .parent()
                .expect("new startup marker parent"),
        )
        .unwrap();
        std::fs::write(old_runtime.startup_committed_path(), "sess-old").unwrap();
        std::fs::write(new_runtime.startup_committed_path(), "sess-new").unwrap();

        write_registry(
            &temp_home,
            &RegistryData {
                sessions: vec![
                    RegistryEntry {
                        session_id: "sess-old".to_string(),
                        session_name: "default".to_string(),
                        pid: 111,
                        socket_path: old_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-03T00:00:00Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "sess-new".to_string(),
                        session_name: "default".to_string(),
                        pid: 222,
                        socket_path: new_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-03T00:00:01Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                ],
            },
        )
        .unwrap();

        cleanup_temp_daemon_registry_state(&super::TempDaemonProcess {
            pid: 111,
            session_name: "default".to_string(),
            session_id: "sess-old".to_string(),
            rub_home: temp_home.clone(),
        });

        let registry = rub_daemon::session::read_registry(&temp_home).unwrap();
        assert_eq!(registry.sessions.len(), 1);
        assert_eq!(registry.sessions[0].session_id, "sess-new");
        assert!(
            new_runtime.startup_committed_path().exists(),
            "replacement authority commit marker must remain"
        );

        let _ = std::fs::remove_dir_all(temp_home);
    }

    #[test]
    fn process_tree_collects_nested_descendants() {
        let snapshot = vec![
            ProcessInfo {
                pid: 10,
                ppid: 1,
                command: "rub".to_string(),
            },
            ProcessInfo {
                pid: 11,
                ppid: 10,
                command: "chrome".to_string(),
            },
            ProcessInfo {
                pid: 12,
                ppid: 11,
                command: "renderer".to_string(),
            },
        ];
        let tree = process_tree(&snapshot, 10);
        assert_eq!(tree.len(), 3);
        assert!(tree.contains(&10));
        assert!(tree.contains(&11));
        assert!(tree.contains(&12));
    }

    #[test]
    fn ancestor_detection_finds_rub_daemon_chain() {
        let snapshot = vec![
            ProcessInfo {
                pid: 100,
                ppid: 1,
                command: "rub".to_string(),
            },
            ProcessInfo {
                pid: 101,
                ppid: 100,
                command: "chrome".to_string(),
            },
        ];
        let ancestors = HashSet::from([100]);
        assert!(process_has_ancestor(&snapshot, 101, &ancestors));
        assert!(process_has_ancestor(&snapshot, 100, &ancestors));
        assert!(!process_has_ancestor(&snapshot, 999, &ancestors));
    }

    #[test]
    fn extract_flag_value_reads_cli_flags() {
        let command = "rub __daemon --session work --rub-home /tmp/rub-e2e-1";
        assert_eq!(
            extract_flag_value(command, "--session").as_deref(),
            Some("work")
        );
        assert_eq!(
            extract_flag_value(command, "--rub-home").as_deref(),
            Some("/tmp/rub-e2e-1")
        );
    }

    #[test]
    fn extract_flag_value_supports_inline_assignment() {
        let command = r#"chrome --user-data-dir="/tmp/rub-chrome-123" --flag=value"#;
        assert_eq!(
            extract_flag_value(command, "--user-data-dir").as_deref(),
            Some("/tmp/rub-chrome-123")
        );
        assert_eq!(
            extract_flag_value(command, "--flag").as_deref(),
            Some("value")
        );
    }

    #[test]
    fn daemon_command_detection_accepts_installed_binary_paths() {
        assert!(is_rub_daemon_command("rub __daemon --session default"));
        assert!(is_rub_daemon_command(
            "/usr/local/bin/rub __daemon --session default --rub-home /tmp/rub-e2e-1"
        ));
        assert!(!is_rub_daemon_command("/usr/local/bin/rub doctor"));
        assert!(!is_rub_daemon_command("/usr/local/bin/not-rub __daemon"));
    }

    #[test]
    fn extract_temp_browser_root_uses_user_data_dir_flag() {
        let command = format!(
            r#"chrome --type=renderer --user-data-dir="{0}" --other-flag"#,
            std::env::temp_dir().join("rub-chrome-321").display()
        );
        assert_eq!(
            extract_temp_browser_root(&command),
            Some(std::env::temp_dir().join("rub-chrome-321"))
        );
    }

    #[tokio::test]
    async fn fetch_upgrade_status_for_session_returns_none_for_missing_canonical_socket() {
        let home = std::env::temp_dir().join(format!("rub-cleanup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let session_paths = RubPaths::new(&home).session("default");

        let status = fetch_upgrade_status_for_session(&session_paths)
            .await
            .expect("socket probing should tolerate missing canonical socket");
        assert!(status.is_none());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn cleanup_skips_pending_startup_entries() {
        let home = unique_temp_home("rub-cleanup-pending");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-pending");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        std::fs::write(session_paths.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(session_paths.socket_path(), b"socket").unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-pending".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: session_paths.socket_path().display().to_string(),
                    created_at: "2026-04-02T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let mut result = CleanupResult::default();
        cleanup_current_home_stale(&home, &mut result)
            .await
            .unwrap();

        assert_eq!(result.skipped_unreachable_sessions, vec!["default"]);
        let registry = rub_daemon::session::read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);

        let _ = std::fs::remove_dir_all(home);
    }
}
