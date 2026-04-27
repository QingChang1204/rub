use super::CleanupResult;
use super::temp_runtime::{
    TempDaemonProcess, TempDaemonReleaseOutcome, cleanup_temp_daemon_registry_state,
    extract_temp_browser_root, is_rub_daemon_command, is_temp_rub_home, orphan_temp_browser_roots,
    process_snapshot, root_has_live_browser_process, temp_daemon_processes, temp_roots,
    terminate_orphan_temp_browser_processes, terminate_revalidated_temp_daemon,
};
use super::upgrade_probe::{
    fetch_upgrade_status_for_session_with_deadline, wait_for_shutdown_paths_until,
};
use crate::daemon_ctl::send_existing_request_with_replay_recovery;
use crate::daemon_ctl::{
    CompatibilityDegradedOwnedSession, compatibility_degraded_owned_from_snapshot,
};
use crate::timeout_budget::helpers::mutating_request;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rub_core::error::RubError;
use rub_core::managed_profile::{
    is_temp_owned_managed_profile_path, managed_profile_paths_equivalent,
    projected_managed_profile_path_for_session,
};
use rub_core::process::{ProcessInfo, is_process_alive, process_has_ancestor};
use rub_daemon::rub_paths::{RubPaths, read_temp_home_owner_pid};
use rub_daemon::session::RegistryData;
use rub_ipc::client::IpcClient;

fn temp_daemon_compatibility_degraded_owned(
    daemon: &TempDaemonProcess,
) -> Option<CompatibilityDegradedOwnedSession> {
    let snapshot = rub_daemon::session::registry_authority_snapshot(&daemon.rub_home).ok()?;
    compatibility_degraded_owned_for_temp_daemon_in_snapshot(&snapshot, daemon)
}

fn compatibility_degraded_owned_for_temp_daemon_in_snapshot(
    snapshot: &rub_daemon::session::RegistryAuthoritySnapshot,
    daemon: &TempDaemonProcess,
) -> Option<CompatibilityDegradedOwnedSession> {
    snapshot
        .session(&daemon.session_name)?
        .entries
        .iter()
        .find(|entry| entry.entry.session_id == daemon.session_id)
        .and_then(compatibility_degraded_owned_from_snapshot)
}

fn record_temp_daemon_unreleased(
    result: &mut CleanupResult,
    daemon: &TempDaemonProcess,
    key: &str,
) {
    if let Some(compatibility_degraded_owned) = temp_daemon_compatibility_degraded_owned(daemon) {
        result
            .compatibility_degraded_owned_sessions
            .push(compatibility_degraded_owned);
    } else {
        result.skipped_busy_temp_daemons.push(key.to_string());
    }
}

fn record_temp_daemon_released(
    result: &mut CleanupResult,
    daemon: &TempDaemonProcess,
    key: String,
) {
    cleanup_temp_daemon_registry_state(daemon);
    result.cleaned_temp_daemons.push(key);
}

pub(super) async fn sweep_temp_daemons(
    current_rub_home: &Path,
    snapshot: &[ProcessInfo],
    deadline: Instant,
    timeout_ms: u64,
    result: &mut CleanupResult,
) -> Result<HashSet<PathBuf>, RubError> {
    let mut active_temp_homes = HashSet::new();

    for daemon in temp_daemon_processes(snapshot) {
        let key = format!("{}@{}", daemon.session_name, daemon.rub_home.display());
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
                    let request_timeout_ms = match remaining_cleanup_budget_ms(
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_close",
                    ) {
                        Ok(timeout_ms) => timeout_ms,
                        Err(error) if timeout_error_for_best_effort_temp_sweep(&error) => {
                            active_temp_homes.insert(daemon.rub_home.clone());
                            record_temp_daemon_unreleased(result, &daemon, &key);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let request =
                        mutating_request("close", serde_json::json!({}), request_timeout_ms);
                    match crate::timeout_budget::run_with_remaining_budget(
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_connect",
                        async {
                            Ok::<_, RubError>(
                                IpcClient::connect_bound(&socket_path, daemon.session_id.as_str())
                                    .await
                                    .ok(),
                            )
                        },
                    )
                    .await
                    {
                        Ok(client) => match client {
                            Some(mut client) => {
                                let close_result =
                                    crate::timeout_budget::run_with_remaining_budget(
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
                                if let Err(error) = close_result {
                                    if timeout_error_for_best_effort_temp_sweep(&error) {
                                        active_temp_homes.insert(daemon.rub_home.clone());
                                        record_temp_daemon_unreleased(result, &daemon, &key);
                                        continue;
                                    }
                                    return Err(error);
                                }
                            }
                            None => {
                                let terminated = crate::timeout_budget::run_with_remaining_budget(
                                    deadline,
                                    timeout_ms,
                                    "cleanup_temp_daemon_force_terminate",
                                    async { terminate_revalidated_temp_daemon(&daemon).await },
                                )
                                .await;
                                let terminated = match terminated {
                                    Ok(terminated) => terminated,
                                    Err(error)
                                        if timeout_error_for_best_effort_temp_sweep(&error) =>
                                    {
                                        active_temp_homes.insert(daemon.rub_home.clone());
                                        record_temp_daemon_unreleased(result, &daemon, &key);
                                        continue;
                                    }
                                    Err(error) => return Err(error),
                                };
                                match terminated {
                                    TempDaemonReleaseOutcome::Released => {
                                        record_temp_daemon_released(result, &daemon, key);
                                    }
                                    TempDaemonReleaseOutcome::StillLive => {
                                        active_temp_homes.insert(daemon.rub_home.clone());
                                        record_temp_daemon_unreleased(result, &daemon, &key);
                                    }
                                }
                                continue;
                            }
                        },
                        Err(error) if timeout_error_for_best_effort_temp_sweep(&error) => {
                            active_temp_homes.insert(daemon.rub_home.clone());
                            record_temp_daemon_unreleased(result, &daemon, &key);
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                    let shutdown_wait = wait_for_shutdown_paths_until(
                        &session_paths.actual_socket_paths(),
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_shutdown_wait",
                    )
                    .await;
                    if let Err(error) = shutdown_wait {
                        if timeout_error_for_best_effort_temp_sweep(&error) {
                            active_temp_homes.insert(daemon.rub_home.clone());
                            record_temp_daemon_unreleased(result, &daemon, &key);
                            continue;
                        }
                        return Err(error);
                    }
                    let terminated = crate::timeout_budget::run_with_remaining_budget(
                        deadline,
                        timeout_ms,
                        "cleanup_temp_daemon_force_terminate",
                        async { terminate_revalidated_temp_daemon(&daemon).await },
                    )
                    .await;
                    let terminated = match terminated {
                        Ok(terminated) => terminated,
                        Err(error) if timeout_error_for_best_effort_temp_sweep(&error) => {
                            active_temp_homes.insert(daemon.rub_home.clone());
                            record_temp_daemon_unreleased(result, &daemon, &key);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    match terminated {
                        TempDaemonReleaseOutcome::Released => {
                            record_temp_daemon_released(result, &daemon, key);
                        }
                        TempDaemonReleaseOutcome::StillLive => {
                            active_temp_homes.insert(daemon.rub_home.clone());
                            record_temp_daemon_unreleased(result, &daemon, &key);
                        }
                    }
                    continue;
                } else {
                    active_temp_homes.insert(daemon.rub_home.clone());
                    record_temp_daemon_unreleased(result, &daemon, &key);
                }
                continue;
            }
            Ok(None) => {}
            Err(error) if timeout_error_for_best_effort_temp_sweep(&error) => {
                active_temp_homes.insert(daemon.rub_home.clone());
                record_temp_daemon_unreleased(result, &daemon, &key);
                continue;
            }
            Err(_) if is_process_alive(daemon.pid) => {
                active_temp_homes.insert(daemon.rub_home.clone());
                record_temp_daemon_unreleased(result, &daemon, &key);
                continue;
            }
            Err(_) => {}
        }

        let terminated = crate::timeout_budget::run_with_remaining_budget(
            deadline,
            timeout_ms,
            "cleanup_temp_daemon_force_terminate",
            async { terminate_revalidated_temp_daemon(&daemon).await },
        )
        .await?;
        match terminated {
            TempDaemonReleaseOutcome::Released => {
                record_temp_daemon_released(result, &daemon, key);
            }
            TempDaemonReleaseOutcome::StillLive => {
                active_temp_homes.insert(daemon.rub_home.clone());
                record_temp_daemon_unreleased(result, &daemon, &key);
            }
        }
    }

    Ok(active_temp_homes)
}

fn timeout_error_for_best_effort_temp_sweep(error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(envelope) if envelope.code == rub_core::error::ErrorCode::IpcTimeout
    )
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
    let mut terminated_orphan_pids = HashSet::new();
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
    }

    let initial_orphan_pids = orphan_temp_browser_pids_for_cleanup(snapshot, &orphan_roots);
    if !initial_orphan_pids.is_empty() {
        let cleanup_outcome = terminate_orphan_temp_browser_processes(snapshot, &orphan_roots)
            .await
            .unwrap_or_default();
        terminated_orphan_pids = cleanup_outcome.terminated_pids;
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
        .extend(terminated_orphan_pids.into_iter().collect::<Vec<_>>());
}

fn orphan_temp_browser_pids_for_cleanup(
    snapshot: &[ProcessInfo],
    orphan_roots: &HashSet<PathBuf>,
) -> HashSet<u32> {
    super::temp_runtime::orphan_temp_browser_pids_for_roots(snapshot, orphan_roots)
}

pub(super) fn sweep_stale_temp_homes(
    current_rub_home: &Path,
    active_temp_homes: &HashSet<PathBuf>,
    result: &mut CleanupResult,
) -> bool {
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
            match revalidated_temp_home_delete_decision(&path) {
                TempHomeDeleteDecision::Remove => {
                    if std::fs::remove_dir_all(&path).is_ok() {
                        result.removed_temp_homes.push(path.display().to_string());
                    }
                }
                TempHomeDeleteDecision::SkipLiveOwner
                | TempHomeDeleteDecision::SkipLiveDaemon
                | TempHomeDeleteDecision::SkipLiveBrowser => continue,
                TempHomeDeleteDecision::SkipAuthorityIncomplete => return false,
            }
        }
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TempHomeDeleteDecision {
    Remove,
    SkipLiveOwner,
    SkipLiveDaemon,
    SkipLiveBrowser,
    SkipAuthorityIncomplete,
}

pub(crate) fn revalidated_temp_home_delete_decision(path: &Path) -> TempHomeDeleteDecision {
    let snapshot = match process_snapshot() {
        Ok(snapshot) => snapshot,
        Err(_) => return TempHomeDeleteDecision::SkipAuthorityIncomplete,
    };
    let live_temp_daemons = temp_daemon_processes(&snapshot);
    classify_stale_temp_home_sweep_decision(path, &snapshot, &live_temp_daemons)
}

fn classify_stale_temp_home_sweep_decision(
    path: &Path,
    snapshot: &[ProcessInfo],
    live_temp_daemons: &[TempDaemonProcess],
) -> TempHomeDeleteDecision {
    if read_temp_home_owner_pid(path).is_some_and(is_process_alive) {
        return TempHomeDeleteDecision::SkipLiveOwner;
    }
    if live_temp_daemons
        .iter()
        .any(|daemon| temp_home_paths_equivalent(&daemon.rub_home, path))
    {
        return TempHomeDeleteDecision::SkipLiveDaemon;
    }
    match live_browser_roots_for_temp_home(path) {
        Ok(browser_roots) => {
            if browser_roots
                .iter()
                .any(|root| root_has_live_browser_process(snapshot, root))
            {
                TempHomeDeleteDecision::SkipLiveBrowser
            } else {
                TempHomeDeleteDecision::Remove
            }
        }
        Err(_) => TempHomeDeleteDecision::SkipAuthorityIncomplete,
    }
}

fn live_browser_roots_for_temp_home(path: &Path) -> io::Result<HashSet<PathBuf>> {
    let mut roots = HashSet::new();
    let by_id_dir = RubPaths::new(path).sessions_dir().join("by-id");
    match std::fs::read_dir(&by_id_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let session_id = entry.file_name();
                let Some(session_id) = session_id.to_str() else {
                    continue;
                };
                roots.insert(projected_managed_profile_path_for_session(session_id));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let registry_path = RubPaths::new(path).registry_path();
    match std::fs::read_to_string(&registry_path) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(roots);
            }
            let registry = serde_json::from_str::<RegistryData>(&contents)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            for entry in registry.sessions {
                roots.insert(projected_managed_profile_path_for_session(
                    &entry.session_id,
                ));
                if let Some(user_data_dir) = entry.user_data_dir {
                    roots.insert(PathBuf::from(user_data_dir));
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    Ok(roots)
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

#[cfg(test)]
mod tests {
    use super::{
        TempDaemonProcess, TempHomeDeleteDecision, classify_stale_temp_home_sweep_decision,
        compatibility_degraded_owned_for_temp_daemon_in_snapshot, record_temp_daemon_released,
        record_temp_daemon_unreleased, sweep_stale_temp_homes,
    };
    use crate::cleanup_ctl::CleanupResult;
    use rub_core::managed_profile::projected_managed_profile_path_for_session;
    use rub_core::process::ProcessInfo;
    use rub_daemon::rub_paths::RubPaths;
    use rub_daemon::session::{
        RegistryAuthoritySnapshot, RegistryEntry, RegistryEntryLiveness, RegistryEntrySnapshot,
        RegistrySessionSnapshot,
    };

    fn temp_home(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rub-temp-owned-{name}-{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn temp_daemon_snapshot_mapping_uses_compatibility_degraded_owned_family_when_registry_truth_is_owned_degraded()
     {
        let home = temp_home("rub-cleanup-temp-daemon");
        let daemon = TempDaemonProcess {
            pid: 42,
            session_name: "default".to_string(),
            session_id: "sess-default".to_string(),
            rub_home: home.clone(),
            user_data_dir: None,
        };
        let snapshot = RegistryAuthoritySnapshot {
            sessions: vec![RegistrySessionSnapshot {
                session_name: "default".to_string(),
                entries: vec![RegistryEntrySnapshot {
                    entry: RegistryEntry {
                        session_id: "sess-default".to_string(),
                        session_name: "default".to_string(),
                        pid: 42,
                        socket_path: "/tmp/rub.sock".to_string(),
                        created_at: "2026-04-19T00:00:00Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    liveness: RegistryEntryLiveness::ProtocolIncompatible,
                    pid_live: true,
                }],
            }],
        };
        let compatibility_degraded_owned =
            compatibility_degraded_owned_for_temp_daemon_in_snapshot(&snapshot, &daemon)
                .expect("protocol-incompatible temp-daemon authority must map to shared family");
        assert_eq!(
            serde_json::to_value(&compatibility_degraded_owned).unwrap()["reason"],
            serde_json::json!("protocol_incompatible")
        );
    }

    #[test]
    fn temp_daemon_unreleased_falls_back_to_busy_when_registry_truth_is_unavailable() {
        let home = temp_home("rub-cleanup-temp-daemon");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let daemon = TempDaemonProcess {
            pid: 42,
            session_name: "default".to_string(),
            session_id: "sess-default".to_string(),
            rub_home: home.clone(),
            user_data_dir: None,
        };
        let mut result = CleanupResult::default();
        record_temp_daemon_unreleased(&mut result, &daemon, "default@temp-home");

        assert_eq!(result.skipped_busy_temp_daemons, vec!["default@temp-home"]);
        assert!(result.compatibility_degraded_owned_sessions.is_empty());

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn temp_daemon_release_projection_treats_already_gone_authority_as_cleaned() {
        let home = temp_home("rub-cleanup-temp-daemon");
        let daemon = TempDaemonProcess {
            pid: 42,
            session_name: "default".to_string(),
            session_id: "sess-default".to_string(),
            rub_home: home,
            user_data_dir: None,
        };
        let mut result = CleanupResult::default();

        record_temp_daemon_released(&mut result, &daemon, "default@temp-home".to_string());

        assert_eq!(result.cleaned_temp_daemons, vec!["default@temp-home"]);
        assert!(result.skipped_busy_temp_daemons.is_empty());
        assert!(result.compatibility_degraded_owned_sessions.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn stale_temp_home_sweep_skips_live_owner_revalidated_after_snapshot() {
        let current_home = temp_home("rub-cleanup-current");
        let stale_home = temp_home("rub-cleanup-stale");
        let _ = std::fs::remove_dir_all(&current_home);
        let _ = std::fs::remove_dir_all(&stale_home);
        std::fs::create_dir_all(&current_home).unwrap();
        std::fs::create_dir_all(&stale_home).unwrap();
        std::fs::write(
            RubPaths::new(&current_home).temp_home_owner_marker_path(),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(
            RubPaths::new(&stale_home).temp_home_owner_marker_path(),
            std::process::id().to_string(),
        )
        .unwrap();

        let mut result = CleanupResult::default();
        let authority_complete = sweep_stale_temp_homes(
            &current_home,
            &std::collections::HashSet::new(),
            &mut result,
        );

        assert!(authority_complete);
        assert!(stale_home.exists());
        let current_home_path = current_home.display().to_string();
        let stale_home_path = stale_home.display().to_string();
        assert!(
            !result
                .removed_temp_homes
                .iter()
                .any(|removed| removed == &current_home_path || removed == &stale_home_path),
            "live-owner temp homes must not be removed during stale-home sweep"
        );
        assert!(result.skipped_best_effort_phases.is_empty());

        let _ = std::fs::remove_dir_all(current_home);
        let _ = std::fs::remove_dir_all(stale_home);
    }

    #[test]
    fn stale_temp_home_authority_revalidation_detects_live_daemon_for_same_home() {
        let home = temp_home("rub-cleanup-daemon-authority");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(RubPaths::new(&home).temp_home_owner_marker_path(), "").unwrap();
        let snapshot = vec![ProcessInfo {
            pid: 42,
            ppid: 1,
            command: format!(
                "rub __daemon --session default --session-id sess-live --rub-home {}",
                home.display()
            ),
        }];
        let live_temp_daemons = super::temp_daemon_processes(&snapshot);

        assert_eq!(
            classify_stale_temp_home_sweep_decision(&home, &snapshot, &live_temp_daemons),
            TempHomeDeleteDecision::SkipLiveDaemon
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn stale_temp_home_authority_revalidation_detects_live_browser_root() {
        let home = temp_home("rub-cleanup-browser-authority");
        let _ = std::fs::remove_dir_all(&home);
        let session_dir = RubPaths::new(&home)
            .sessions_dir()
            .join("by-id")
            .join("sess-browser");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(RubPaths::new(&home).temp_home_owner_marker_path(), "").unwrap();
        let profile_root = projected_managed_profile_path_for_session("sess-browser");
        let snapshot = vec![ProcessInfo {
            pid: 43,
            ppid: 1,
            command: format!(
                r#"Google Chrome Helper --type=renderer --user-data-dir="{}""#,
                profile_root.display()
            ),
        }];

        assert_eq!(
            classify_stale_temp_home_sweep_decision(&home, &snapshot, &[]),
            TempHomeDeleteDecision::SkipLiveBrowser
        );

        let _ = std::fs::remove_dir_all(home);
    }
}
