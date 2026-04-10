use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rub_core::error::{ErrorCode, RubError};
use rub_core::process::{
    ProcessInfo, extract_flag_value, is_chromium_browser_command, is_process_alive,
    process_has_ancestor, process_snapshot as collect_process_snapshot, process_tree,
    tokenize_command,
};
use rub_daemon::rub_paths::RubPaths;

#[derive(Debug, Clone)]
pub(super) struct TempDaemonProcess {
    pub(super) pid: u32,
    pub(super) session_name: String,
    pub(super) session_id: String,
    pub(super) rub_home: PathBuf,
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
        daemons.push(TempDaemonProcess {
            pid: process.pid,
            session_name,
            session_id,
            rub_home,
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

pub(super) async fn terminate_revalidated_temp_daemon(daemon: &TempDaemonProcess) {
    let Ok(snapshot) = process_snapshot() else {
        return;
    };
    let Some(tree) = revalidated_temp_daemon_tree(&snapshot, daemon) else {
        return;
    };
    terminate_process_tree(&tree).await;
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
        if !orphan_roots.contains(&root)
            || process_has_ancestor(snapshot, process.pid, &daemon_pids)
        {
            continue;
        }
        orphan_pids.insert(process.pid);
    }
    orphan_pids
}

pub(super) fn root_has_live_browser_process(snapshot: &[ProcessInfo], root: &Path) -> bool {
    snapshot.iter().any(|process| {
        extract_temp_browser_root(&process.command)
            .as_deref()
            .is_some_and(|candidate| candidate == root)
    })
}

pub(super) async fn terminate_process_tree(processes: &HashSet<u32>) {
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
    rub_daemon::rub_paths::is_temp_owned_home(path)
}

pub(super) fn extract_temp_browser_root(command: &str) -> Option<PathBuf> {
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
        user_data_dir: None,
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
