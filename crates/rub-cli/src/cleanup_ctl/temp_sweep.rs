use super::CleanupResult;
use super::temp_runtime::{
    cleanup_temp_daemon_registry_state, extract_temp_browser_root, is_rub_daemon_command,
    is_temp_rub_home, orphan_roots_contain_equivalent, orphan_temp_browser_pids_for_roots,
    orphan_temp_browser_roots, process_snapshot, root_has_live_browser_process,
    temp_daemon_processes, temp_roots, terminate_process_tree, terminate_revalidated_temp_daemon,
};
use super::upgrade_probe::{
    fetch_upgrade_status_for_session_with_deadline, wait_for_shutdown_paths_until,
};
use crate::daemon_ctl::send_existing_request_with_replay_recovery;
use crate::timeout_budget::helpers::mutating_request;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rub_core::error::RubError;
use rub_core::managed_profile::{
    is_temp_owned_managed_profile_path, managed_profile_paths_equivalent,
};
use rub_core::process::{ProcessInfo, is_process_alive, process_has_ancestor};
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::IpcClient;

pub(super) async fn sweep_temp_daemons(
    current_rub_home: &Path,
    snapshot: &[ProcessInfo],
    deadline: Instant,
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

        match fetch_upgrade_status_for_session_with_deadline(
            &session_paths,
            deadline,
            timeout_ms,
            "cleanup_temp_daemon_upgrade_check",
        )
        .await
        {
            Ok(Some((status, socket_path))) => {
                if status.idle {
                    let request = mutating_request(
                        "close",
                        serde_json::json!({}),
                        remaining_cleanup_budget_ms(
                            deadline,
                            timeout_ms,
                            "cleanup_temp_daemon_close",
                        )?,
                    );
                    let key = format!("{}@{}", daemon.session_name, daemon.rub_home.display());
                    match crate::timeout_budget::run_with_remaining_budget(
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_connect",
                        async { Ok::<_, RubError>(IpcClient::connect(&socket_path).await.ok()) },
                    )
                    .await?
                    {
                        Some(mut client) => {
                            let _ = crate::timeout_budget::run_with_remaining_budget(
                                deadline,
                                timeout_ms,
                                "cleanup_temp_daemon_close",
                                send_existing_request_with_replay_recovery(
                                    &mut client,
                                    &request,
                                    deadline,
                                    &daemon.rub_home,
                                    &daemon.session_name,
                                    Some(daemon.session_id.as_str()),
                                ),
                            )
                            .await;
                        }
                        None => {
                            crate::timeout_budget::run_with_remaining_budget(
                                deadline,
                                timeout_ms,
                                "cleanup_temp_daemon_force_terminate",
                                async {
                                    terminate_revalidated_temp_daemon(&daemon).await;
                                    Ok::<(), RubError>(())
                                },
                            )
                            .await?;
                        }
                    }
                    wait_for_shutdown_paths_until(
                        &session_paths.actual_socket_paths(),
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_shutdown_wait",
                    )
                    .await?;
                    crate::timeout_budget::run_with_remaining_budget(
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_force_terminate",
                        async {
                            terminate_revalidated_temp_daemon(&daemon).await;
                            Ok::<(), RubError>(())
                        },
                    )
                    .await?;
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
            Err(error) if matches!(&error, RubError::Domain(envelope) if envelope.code == rub_core::error::ErrorCode::IpcTimeout) =>
            {
                return Err(error);
            }
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

        crate::timeout_budget::run_with_remaining_budget(
            deadline,
            timeout_ms,
            "cleanup_temp_daemon_force_terminate",
            async {
                terminate_revalidated_temp_daemon(&daemon).await;
                Ok::<(), RubError>(())
            },
        )
        .await?;
        cleanup_temp_daemon_registry_state(&daemon);
        result.cleaned_temp_daemons.push(format!(
            "{}@{}",
            daemon.session_name,
            daemon.rub_home.display()
        ));
    }

    Ok(active_temp_homes)
}

fn remaining_cleanup_budget_ms(
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<u64, RubError> {
    crate::timeout_budget::remaining_budget_duration(deadline)
        .map(|remaining| remaining.as_millis().clamp(1, u64::MAX as u128) as u64)
        .ok_or_else(|| crate::main_support::command_timeout_error(timeout_ms, phase))
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
    let mut orphan_roots = orphan_temp_browser_roots(snapshot);

    for process in snapshot {
        let Some(root) = extract_temp_browser_root(&process.command) else {
            continue;
        };
        if !is_temp_owned_managed_profile_path(&root) {
            continue;
        }
        if process_has_ancestor(snapshot, process.pid, &daemon_pids) {
            orphan_roots.retain(|candidate| {
                candidate != &root && !managed_profile_paths_equivalent(candidate, &root)
            });
            continue;
        }
        if orphan_roots_contain_equivalent(&orphan_roots, &root) {
            orphan_pids.insert(process.pid);
        }
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
            if temp_home_paths_equivalent(&path, current_rub_home)
                || !is_temp_rub_home(&path)
                || active_temp_homes
                    .iter()
                    .any(|active| temp_home_paths_equivalent(&path, active))
            {
                continue;
            }
            if std::fs::remove_dir_all(&path).is_ok() {
                result.removed_temp_homes.push(path.display().to_string());
            }
        }
    }
}

fn temp_home_paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    canonical_temp_home_path(left)
        .zip(canonical_temp_home_path(right))
        .is_some_and(|(left, right)| left == right)
}

fn canonical_temp_home_path(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path)
        .ok()
        .or_else(|| strip_private_prefix(path))
        .or_else(|| Some(path.to_path_buf()))
}

fn strip_private_prefix(path: &Path) -> Option<PathBuf> {
    let stripped = path.strip_prefix("/private").ok()?;
    Some(PathBuf::from("/").join(stripped))
}
