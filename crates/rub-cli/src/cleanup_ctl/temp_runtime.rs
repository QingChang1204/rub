use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rub_core::error::{ErrorCode, RubError};
use rub_core::managed_profile::{
    extract_managed_profile_path_from_command, is_temp_owned_managed_profile_path,
    managed_profile_paths_equivalent, managed_profile_temp_roots,
};
use rub_core::process::{
    ProcessInfo, extract_flag_value, is_chromium_process_command, process_has_ancestor,
    process_snapshot as collect_process_snapshot, process_tree, tokenize_command,
};
use rub_daemon::rub_paths::RubPaths;

#[derive(Debug, Clone)]
pub(super) struct TempDaemonProcess {
    pub(super) pid: u32,
    pub(super) session_name: String,
    pub(super) session_id: String,
    pub(super) rub_home: PathBuf,
    pub(super) user_data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TempDaemonReleaseOutcome {
    Released,
    StillLive,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct OrphanBrowserCleanupOutcome {
    pub(super) terminated_pids: HashSet<u32>,
    pub(super) surviving_pids: HashSet<u32>,
}

pub(super) fn process_snapshot() -> Result<Vec<ProcessInfo>, RubError> {
    collect_process_snapshot().map_err(|error| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Failed to collect process snapshot: {error}"),
        )
    })
}

pub(super) fn temp_daemon_processes(snapshot: &[ProcessInfo]) -> Vec<TempDaemonProcess> {
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
        let user_data_dir =
            super::upgrade_probe::registry_entry_for_home_session_id(&rub_home, &session_id)
                .and_then(|entry| entry.user_data_dir.map(PathBuf::from));
        daemons.push(TempDaemonProcess {
            pid: process.pid,
            session_name,
            session_id,
            rub_home,
            user_data_dir,
        });
    }
    daemons
}

pub(super) fn daemon_process_matches_authority(
    process: &ProcessInfo,
    daemon: &TempDaemonProcess,
) -> bool {
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

pub(super) fn revalidated_temp_daemon_tree(
    snapshot: &[ProcessInfo],
    daemon: &TempDaemonProcess,
) -> Option<HashSet<u32>> {
    snapshot
        .iter()
        .find(|process| daemon_process_matches_authority(process, daemon))
        .map(|process| process_tree(snapshot, process.pid))
}

pub(super) async fn terminate_revalidated_temp_daemon(
    daemon: &TempDaemonProcess,
) -> Result<TempDaemonReleaseOutcome, RubError> {
    let Ok(snapshot) = process_snapshot() else {
        return Ok(TempDaemonReleaseOutcome::StillLive);
    };
    let Some(tree) = revalidated_temp_daemon_tree(&snapshot, daemon) else {
        return Ok(TempDaemonReleaseOutcome::Released);
    };
    terminate_process_tree(&tree).await;
    let Ok(current_snapshot) = process_snapshot() else {
        return Ok(TempDaemonReleaseOutcome::StillLive);
    };
    let survivors = revalidated_temp_daemon_sigkill_tree(&current_snapshot, daemon);
    if survivors.is_empty() {
        return Ok(TempDaemonReleaseOutcome::Released);
    }
    signal_processes(&survivors, libc::SIGKILL);
    let Ok(final_snapshot) = process_snapshot() else {
        return Ok(TempDaemonReleaseOutcome::StillLive);
    };
    Ok(classify_temp_daemon_release(
        &revalidated_temp_daemon_sigkill_tree(&final_snapshot, daemon),
    ))
}

pub(super) fn orphan_temp_browser_pids_for_roots(
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
        if !orphan_roots_contain_equivalent(orphan_roots, &root)
            || process_has_ancestor(snapshot, process.pid, &daemon_pids)
        {
            continue;
        }
        orphan_pids.insert(process.pid);
    }
    orphan_pids
}

pub(super) async fn terminate_orphan_temp_browser_processes(
    snapshot: &[ProcessInfo],
    orphan_roots: &HashSet<PathBuf>,
) -> Result<OrphanBrowserCleanupOutcome, RubError> {
    let orphan_pids = orphan_temp_browser_pids_for_roots(snapshot, orphan_roots);
    if orphan_pids.is_empty() {
        return Ok(OrphanBrowserCleanupOutcome::default());
    }
    terminate_process_tree(&orphan_pids).await;
    let Ok(current_snapshot) = process_snapshot() else {
        return Ok(OrphanBrowserCleanupOutcome {
            terminated_pids: HashSet::new(),
            surviving_pids: orphan_pids,
        });
    };
    let survivors = orphan_temp_browser_pids_for_roots(&current_snapshot, orphan_roots);
    if survivors.is_empty() {
        return Ok(summarize_orphan_browser_cleanup(&orphan_pids, &survivors));
    }
    signal_processes(&survivors, libc::SIGKILL);
    let Ok(final_snapshot) = process_snapshot() else {
        return Ok(OrphanBrowserCleanupOutcome {
            terminated_pids: HashSet::new(),
            surviving_pids: survivors,
        });
    };
    let surviving_pids = orphan_temp_browser_pids_for_roots(&final_snapshot, orphan_roots);
    Ok(summarize_orphan_browser_cleanup(
        &orphan_pids,
        &surviving_pids,
    ))
}

fn classify_temp_daemon_release(surviving_pids: &HashSet<u32>) -> TempDaemonReleaseOutcome {
    if surviving_pids.is_empty() {
        TempDaemonReleaseOutcome::Released
    } else {
        TempDaemonReleaseOutcome::StillLive
    }
}

fn summarize_orphan_browser_cleanup(
    initial_pids: &HashSet<u32>,
    surviving_pids: &HashSet<u32>,
) -> OrphanBrowserCleanupOutcome {
    OrphanBrowserCleanupOutcome {
        terminated_pids: initial_pids.difference(surviving_pids).copied().collect(),
        surviving_pids: surviving_pids.clone(),
    }
}

pub(super) fn orphan_temp_browser_roots(snapshot: &[ProcessInfo]) -> HashSet<PathBuf> {
    let mut roots = HashSet::new();
    for process in snapshot {
        if let Some(root) = extract_temp_browser_root(&process.command) {
            if !is_temp_owned_managed_profile_path(&root) {
                continue;
            }
            roots.insert(root);
        }
    }

    for temp_root in managed_profile_temp_roots() {
        let Ok(entries) = std::fs::read_dir(&temp_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !is_temp_owned_managed_profile_path(&path) {
                continue;
            }
            if root_has_live_browser_process(snapshot, &path) {
                continue;
            }
            roots.insert(path);
        }
    }

    roots
}

pub(super) fn root_has_live_browser_process(snapshot: &[ProcessInfo], root: &Path) -> bool {
    snapshot.iter().any(|process| {
        extract_temp_browser_root(&process.command)
            .as_deref()
            .is_some_and(|candidate| {
                candidate == root || managed_profile_paths_equivalent(candidate, root)
            })
    })
}

pub(super) async fn terminate_process_tree(processes: &HashSet<u32>) {
    signal_processes(processes, libc::SIGTERM);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

fn signal_processes(processes: &HashSet<u32>, signal: i32) {
    if processes.is_empty() {
        return;
    }

    for pid in processes {
        unsafe {
            libc::kill(*pid as i32, signal);
        }
    }
}

pub(super) fn revalidated_temp_daemon_sigkill_tree(
    snapshot: &[ProcessInfo],
    daemon: &TempDaemonProcess,
) -> HashSet<u32> {
    revalidated_temp_daemon_tree(snapshot, daemon).unwrap_or_default()
}

pub(super) fn is_rub_daemon_command(command: &str) -> bool {
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

pub(super) fn is_temp_rub_home(path: &Path) -> bool {
    rub_daemon::rub_paths::is_temp_owned_home_cleanup_authoritative(path)
}

pub(super) fn extract_temp_browser_root(command: &str) -> Option<PathBuf> {
    if !is_chromium_process_command(command) {
        return None;
    }
    extract_managed_profile_path_from_command(command)
}

pub(super) fn orphan_roots_contain_equivalent(
    orphan_roots: &HashSet<PathBuf>,
    candidate_root: &Path,
) -> bool {
    orphan_roots.iter().any(|root| {
        root == candidate_root || managed_profile_paths_equivalent(root, candidate_root)
    })
}

pub(super) fn cleanup_temp_daemon_registry_state(daemon: &TempDaemonProcess) {
    let runtime =
        RubPaths::new(&daemon.rub_home).session_runtime(&daemon.session_name, &daemon.session_id);
    let entry = super::upgrade_probe::registry_entry_for_home_session_id(
        &daemon.rub_home,
        &daemon.session_id,
    )
    .unwrap_or(rub_daemon::session::RegistryEntry {
        session_id: daemon.session_id.clone(),
        session_name: daemon.session_name.clone(),
        pid: daemon.pid,
        socket_path: runtime.socket_path().display().to_string(),
        created_at: String::new(),
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: daemon
            .user_data_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        attachment_identity: None,
        connection_target: None,
    });
    let _ = rub_daemon::session::deregister_session(&daemon.rub_home, &entry.session_id);
    rub_daemon::session::cleanup_projections(&daemon.rub_home, &entry);
}

pub(super) fn temp_roots() -> Vec<PathBuf> {
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
        OrphanBrowserCleanupOutcome, TempDaemonReleaseOutcome, classify_temp_daemon_release,
        summarize_orphan_browser_cleanup,
    };
    use std::collections::HashSet;

    #[test]
    fn temp_daemon_release_is_honest_when_authority_is_already_gone() {
        assert_eq!(
            classify_temp_daemon_release(&HashSet::new()),
            TempDaemonReleaseOutcome::Released
        );
    }

    #[test]
    fn temp_daemon_release_is_busy_only_when_processes_still_survive() {
        assert_eq!(
            classify_temp_daemon_release(&HashSet::from([42_u32])),
            TempDaemonReleaseOutcome::StillLive
        );
    }

    #[test]
    fn orphan_browser_cleanup_reports_proven_terminated_pids_not_just_sigkill_survivors() {
        let outcome = summarize_orphan_browser_cleanup(
            &HashSet::from([1_u32, 2_u32, 3_u32]),
            &HashSet::from([3_u32]),
        );

        assert_eq!(
            outcome,
            OrphanBrowserCleanupOutcome {
                terminated_pids: HashSet::from([1_u32, 2_u32]),
                surviving_pids: HashSet::from([3_u32]),
            }
        );
    }
}
