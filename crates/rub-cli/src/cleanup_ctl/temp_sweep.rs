use super::CleanupResult;
use super::temp_runtime::{
    cleanup_temp_daemon_registry_state, extract_temp_browser_root, is_rub_daemon_command,
    is_temp_rub_home, orphan_temp_browser_pids_for_roots, process_snapshot,
    root_has_live_browser_process, temp_daemon_processes, temp_roots, terminate_process_tree,
    terminate_revalidated_temp_daemon,
};
use super::upgrade_probe::{fetch_upgrade_status_for_session, wait_for_shutdown_paths};
use crate::daemon_ctl::send_existing_request_with_replay_recovery;
use crate::timeout_budget::helpers::mutating_request;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rub_core::error::RubError;
use rub_core::process::{ProcessInfo, is_process_alive, process_has_ancestor};
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;

pub(super) async fn sweep_temp_daemons(
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

pub(super) async fn sweep_orphan_temp_browsers(
    snapshot: &[ProcessInfo],
    result: &mut CleanupResult,
) {
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

pub(super) fn sweep_stale_temp_homes(
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
