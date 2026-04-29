//! Managed browser lifecycle — authoritative launch profile resolution and shutdown fencing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::browser::CloseParams as BrowserCloseParams;
use rub_core::error::{ErrorCode, RubError};
use rub_core::managed_profile::{
    managed_profile_paths_equivalent,
    projected_managed_profile_path_for_scope as shared_projected_managed_profile_path_for_scope,
    sync_temp_owned_managed_profile_marker,
};
use rub_core::process::{
    ProcessInfo, is_browser_root_process, is_chromium_process_command, is_process_alive,
    process_snapshot as collect_process_snapshot, process_tree,
};
use tokio::time::{sleep, timeout};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedProfileDir {
    pub path: PathBuf,
    pub profile_directory: Option<String>,
    pub ephemeral: bool,
}

pub(crate) fn projected_managed_profile_path_for_scope(scope: &str) -> PathBuf {
    shared_projected_managed_profile_path_for_scope(scope)
}

pub(crate) fn resolve_managed_profile_dir(
    explicit: Option<PathBuf>,
    profile_directory: Option<String>,
    explicit_ephemeral: bool,
) -> ManagedProfileDir {
    match explicit {
        Some(path) => ManagedProfileDir {
            path,
            profile_directory,
            ephemeral: explicit_ephemeral,
        },
        None => ManagedProfileDir {
            path: projected_managed_profile_path_for_scope(&format!("pid-{}", std::process::id())),
            profile_directory,
            ephemeral: true,
        },
    }
}

pub(crate) fn prepare_managed_profile_ownership_prelaunch(
    profile: &ManagedProfileDir,
) -> Result<(), RubError> {
    if !profile.ephemeral {
        return Ok(());
    }
    sync_temp_owned_managed_profile_marker(&profile.path, true).map_err(|error| {
        managed_profile_ownership_error(
            profile,
            "prepare managed profile temp-owned marker before launch",
            error,
        )
    })
}

pub(crate) fn rollback_managed_profile_ownership_prelaunch(
    profile: &ManagedProfileDir,
) -> Result<(), RubError> {
    if !profile.ephemeral {
        return Ok(());
    }

    match std::fs::remove_dir_all(&profile.path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            let restore_marker_error =
                sync_temp_owned_managed_profile_marker(&profile.path, true).err();
            let mut ownership_error = managed_profile_ownership_error(
                profile,
                "remove ephemeral managed profile after failed launch",
                error,
            );
            if let RubError::Domain(envelope) = &mut ownership_error
                && let Some(restore_marker_error) = restore_marker_error
            {
                let mut context = envelope
                    .context
                    .take()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                context.insert(
                    "fallback_marker_restore_succeeded".to_string(),
                    serde_json::json!(false),
                );
                context.insert(
                    "fallback_marker_restore_error".to_string(),
                    serde_json::json!(restore_marker_error.to_string()),
                );
                envelope.context = Some(serde_json::Value::Object(context));
            }
            Err(ownership_error)
        }
    }
}

pub(crate) fn commit_managed_profile_ownership(
    profile: &ManagedProfileDir,
) -> Result<(), RubError> {
    sync_temp_owned_managed_profile_marker(&profile.path, profile.ephemeral).map_err(|error| {
        managed_profile_ownership_error(
            profile,
            "commit managed profile ownership after browser authority install",
            error,
        )
    })
}

pub fn projected_managed_profile_path_for_session(session_id: &str) -> PathBuf {
    projected_managed_profile_path_for_scope(&format!("session-{session_id}"))
}

pub fn projected_managed_profile_path(explicit: Option<PathBuf>) -> PathBuf {
    resolve_managed_profile_dir(explicit, None, false).path
}

fn managed_profile_ownership_error(
    profile: &ManagedProfileDir,
    operation: &str,
    error: std::io::Error,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::BrowserLaunchFailed,
        format!(
            "Failed to {operation} for {}: {error}",
            profile.path.display()
        ),
        serde_json::json!({
            "user_data_dir": profile.path.display().to_string(),
            "profile_directory": profile.profile_directory.clone(),
            "managed_profile_ephemeral": profile.ephemeral,
            "operation": operation,
        }),
    )
}

pub(crate) async fn shutdown_managed_browser(
    browser: &Browser,
    profile: &ManagedProfileDir,
) -> Result<(), RubError> {
    let root_pid = find_managed_browser_root_pid(profile)?;

    let close_result = timeout(
        Duration::from_secs(2),
        browser.execute(BrowserCloseParams::default()),
    )
    .await;

    match close_result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            warn!(
                user_data_dir = %profile.path.display(),
                error = %error,
                "Managed browser close returned an error; falling back to process fence"
            );
        }
        Err(_) => {
            warn!(
                user_data_dir = %profile.path.display(),
                "Managed browser close timed out; falling back to process fence"
            );
        }
    }

    enforce_managed_browser_process_fence(profile, root_pid).await
}

pub async fn cleanup_managed_profile_authority(
    profile_dir: impl Into<PathBuf>,
    profile_directory: Option<&str>,
    ephemeral: bool,
) -> Result<(), RubError> {
    let profile = ManagedProfileDir {
        path: profile_dir.into(),
        profile_directory: profile_directory.map(str::to_owned),
        ephemeral,
    };
    let root_pid = find_managed_browser_root_pid(&profile)?;
    enforce_managed_browser_process_fence(&profile, root_pid).await
}

async fn enforce_managed_browser_process_fence(
    profile: &ManagedProfileDir,
    root_pid: Option<u32>,
) -> Result<(), RubError> {
    if let Some(root_pid) = root_pid {
        wait_for_process_exit(root_pid, Duration::from_secs(2)).await;
        if is_process_alive(root_pid) {
            let snapshot = process_snapshot()?;
            let Some(tree) = authoritative_process_tree(&snapshot, root_pid, profile) else {
                wait_for_profile_release(profile, Duration::from_secs(2)).await?;
                let current_snapshot = process_snapshot()?;
                let residual_profile_processes =
                    managed_profile_residue_pids(&current_snapshot, profile);
                if !residual_profile_processes.is_empty() {
                    if profile_residue_is_chromium_only(
                        &current_snapshot,
                        &residual_profile_processes,
                    ) {
                        terminate_browser_profile_residue(&residual_profile_processes).await;
                        let final_snapshot = process_snapshot()?;
                        let final_residual = managed_profile_residue_pids(&final_snapshot, profile);
                        if final_residual.is_empty() {
                            remove_ephemeral_profile_dir(profile).await?;
                            return Ok(());
                        }
                    }
                    return Err(RubError::domain_with_context(
                        ErrorCode::ProfileInUse,
                        format!(
                            "Managed browser root process {root_pid} lost authority before shutdown fencing completed for profile {}",
                            profile.path.display()
                        ),
                        serde_json::json!({
                            "user_data_dir": profile.path.display().to_string(),
                            "profile_directory": profile.profile_directory.clone(),
                            "root_pid": root_pid,
                            "reason": "managed_browser_root_authority_lost_with_residual_profile_processes",
                            "residual_profile_process_pids": sorted_pids(&residual_profile_processes),
                        }),
                    ));
                }
                remove_ephemeral_profile_dir(profile).await?;
                return Ok(());
            };
            terminate_process_tree(&tree).await;
            let current_snapshot = process_snapshot()?;
            match authoritative_sigkill_tree(&current_snapshot, root_pid, profile) {
                Some(authoritative_survivors) => {
                    signal_processes(&authoritative_survivors, libc::SIGKILL);
                }
                None if current_snapshot
                    .iter()
                    .any(|process| process.pid == root_pid) =>
                {
                    return Err(sigkill_authority_lost_error(profile, root_pid));
                }
                None => {}
            }
            wait_for_process_exit(root_pid, Duration::from_secs(2)).await;
            if is_process_alive(root_pid) {
                wait_for_process_exit(root_pid, Duration::from_millis(500)).await;
            }
            if is_process_alive(root_pid) {
                wait_for_process_exit(root_pid, Duration::from_millis(250)).await;
            }
            if is_process_alive(root_pid) {
                return Err(RubError::domain_with_context(
                    ErrorCode::ProfileInUse,
                    format!(
                        "Managed browser root process {root_pid} still owns profile {} after shutdown fencing",
                        profile.path.display()
                    ),
                    serde_json::json!({
                        "user_data_dir": profile.path.display().to_string(),
                        "profile_directory": profile.profile_directory.clone(),
                        "root_pid": root_pid,
                    }),
                ));
            }
        }
    }

    wait_for_profile_release(profile, Duration::from_secs(2)).await?;
    let current_snapshot = process_snapshot()?;
    let residual_profile_processes = managed_profile_residue_pids(&current_snapshot, profile);
    if !residual_profile_processes.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::ProfileInUse,
            format!(
                "Managed browser profile {} still has residual process authority after shutdown fencing",
                profile.path.display()
            ),
            serde_json::json!({
                "user_data_dir": profile.path.display().to_string(),
                "profile_directory": profile.profile_directory.clone(),
                "residual_profile_process_pids": sorted_pids(&residual_profile_processes),
            }),
        ));
    }

    remove_ephemeral_profile_dir(profile).await?;

    Ok(())
}

async fn remove_ephemeral_profile_dir(profile: &ManagedProfileDir) -> Result<(), RubError> {
    if !profile.ephemeral {
        return Ok(());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

    loop {
        match std::fs::remove_dir_all(&profile.path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(managed_profile_ownership_error(
                        profile,
                        "remove ephemeral managed profile after shutdown fencing",
                        error,
                    ));
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

pub fn is_profile_in_use(profile_dir: &Path) -> Result<bool, RubError> {
    let snapshot = process_snapshot()?;
    Ok(!managed_profile_residue_pids(
        &snapshot,
        &ManagedProfileDir {
            path: profile_dir.to_path_buf(),
            profile_directory: None,
            ephemeral: false,
        },
    )
    .is_empty())
}

fn find_managed_browser_root_pid(profile: &ManagedProfileDir) -> Result<Option<u32>, RubError> {
    let snapshot = process_snapshot()?;
    Ok(find_root_pid_in_snapshot(&snapshot, profile))
}

fn find_root_pid_in_snapshot(snapshot: &[ProcessInfo], profile: &ManagedProfileDir) -> Option<u32> {
    snapshot
        .iter()
        .find(|process| {
            is_browser_root_process(&process.command)
                && managed_profile_process_matches_authority(process, profile)
        })
        .map(|process| process.pid)
}

fn authoritative_process_tree(
    snapshot: &[ProcessInfo],
    root_pid: u32,
    profile: &ManagedProfileDir,
) -> Option<HashSet<u32>> {
    snapshot
        .iter()
        .find(|process| {
            process.pid == root_pid
                && is_browser_root_process(&process.command)
                && managed_profile_process_matches_authority(process, profile)
        })
        .map(|process| process_tree(snapshot, process.pid))
}

fn process_snapshot() -> Result<Vec<ProcessInfo>, RubError> {
    collect_process_snapshot().map_err(|error| {
        RubError::domain(
            ErrorCode::BrowserLaunchFailed,
            format!("Failed to collect browser process snapshot: {error}"),
        )
    })
}

async fn terminate_process_tree(processes: &HashSet<u32>) {
    signal_processes(processes, libc::SIGTERM);
    sleep(Duration::from_millis(500)).await;
}

async fn terminate_browser_profile_residue(processes: &HashSet<u32>) {
    signal_processes(processes, libc::SIGTERM);
    sleep(Duration::from_millis(500)).await;
    let survivors = processes
        .iter()
        .copied()
        .filter(|pid| is_process_alive(*pid))
        .collect::<HashSet<_>>();
    signal_processes(&survivors, libc::SIGKILL);
    sleep(Duration::from_millis(250)).await;
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

fn authoritative_sigkill_tree(
    snapshot: &[ProcessInfo],
    root_pid: u32,
    profile: &ManagedProfileDir,
) -> Option<HashSet<u32>> {
    authoritative_process_tree(snapshot, root_pid, profile)
}

fn sigkill_authority_lost_error(profile: &ManagedProfileDir, root_pid: u32) -> RubError {
    RubError::domain_with_context(
        ErrorCode::ProfileInUse,
        format!(
            "Managed browser root process {root_pid} lost authority before SIGKILL revalidation for profile {}",
            profile.path.display()
        ),
        serde_json::json!({
            "reason": "managed_browser_sigkill_authority_lost",
            "user_data_dir": profile.path.display().to_string(),
            "profile_directory": profile.profile_directory.clone(),
            "root_pid": root_pid,
            "cleanup_authority": "managed_browser_process_tree_revalidation",
            "unsafe_to_kill": true,
        }),
    )
}

fn managed_profile_residue_pids(
    snapshot: &[ProcessInfo],
    profile: &ManagedProfileDir,
) -> HashSet<u32> {
    snapshot
        .iter()
        .filter(|process| {
            is_chromium_process_command(&process.command)
                && managed_profile_process_matches_authority(process, profile)
        })
        .map(|process| process.pid)
        .collect()
}

fn profile_residue_is_chromium_only(snapshot: &[ProcessInfo], residue: &HashSet<u32>) -> bool {
    !residue.is_empty()
        && residue.iter().all(|pid| {
            snapshot
                .iter()
                .find(|process| process.pid == *pid)
                .is_some_and(|process| is_chromium_process_command(&process.command))
        })
}

fn managed_profile_process_matches_authority(
    process: &ProcessInfo,
    profile: &ManagedProfileDir,
) -> bool {
    let Some(candidate_user_data_dir) =
        extract_browser_process_flag_value(&process.command, "--user-data-dir")
    else {
        return false;
    };
    let candidate_user_data_dir = Path::new(&candidate_user_data_dir);
    let user_data_dir_matches = candidate_user_data_dir == profile.path.as_path()
        || managed_profile_paths_equivalent(candidate_user_data_dir, &profile.path);
    if !user_data_dir_matches {
        return false;
    }
    match profile.profile_directory.as_deref() {
        Some(expected) => {
            extract_browser_process_flag_value(&process.command, "--profile-directory")
                .as_deref()
                .is_some_and(|candidate| candidate == expected)
        }
        None => true,
    }
}

fn extract_browser_process_flag_value(command: &str, flag: &str) -> Option<String> {
    let parts = rub_core::process::tokenize_command(command);
    let inline_prefix = format!("{flag}=");
    let mut index = 0;
    while index < parts.len() {
        let part = &parts[index];
        if part == flag {
            return collect_split_flag_value(&parts, index + 1);
        }
        if let Some(value) = part.strip_prefix(&inline_prefix) {
            return Some(collect_split_flag_value_with_seed(&parts, index + 1, value));
        }
        index += 1;
    }
    None
}

fn collect_split_flag_value(parts: &[String], start_index: usize) -> Option<String> {
    let seed = parts.get(start_index)?;
    Some(collect_split_flag_value_with_seed(
        parts,
        start_index + 1,
        seed,
    ))
}

fn collect_split_flag_value_with_seed(
    parts: &[String],
    mut next_index: usize,
    seed: &str,
) -> String {
    let mut value = seed.to_string();
    while let Some(part) = parts.get(next_index) {
        if part.starts_with('-') {
            break;
        }
        if !value.is_empty() {
            value.push(' ');
        }
        value.push_str(part);
        next_index += 1;
    }
    value
}

fn sorted_pids(processes: &HashSet<u32>) -> Vec<u32> {
    let mut pids = processes.iter().copied().collect::<Vec<_>>();
    pids.sort_unstable();
    pids
}

async fn wait_for_process_exit(pid: u32, budget: Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if !is_process_alive(pid) {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_profile_release(
    profile: &ManagedProfileDir,
    budget: Duration,
) -> Result<(), RubError> {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if managed_profile_residue_pids(&process_snapshot()?, profile).is_empty() {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ManagedProfileDir, authoritative_process_tree, authoritative_sigkill_tree,
        cleanup_managed_profile_authority, commit_managed_profile_ownership,
        find_root_pid_in_snapshot, managed_profile_residue_pids,
        prepare_managed_profile_ownership_prelaunch, profile_residue_is_chromium_only,
        resolve_managed_profile_dir, rollback_managed_profile_ownership_prelaunch,
        sigkill_authority_lost_error,
    };
    use rub_core::error::ErrorCode;
    use rub_core::managed_profile::{
        has_temp_owned_managed_profile_marker, sync_temp_owned_managed_profile_marker,
    };
    use rub_core::process::{ProcessInfo, extract_flag_value, parse_process_snapshot_line};
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[test]
    fn generated_profile_dir_is_ephemeral() {
        let profile = resolve_managed_profile_dir(None, None, false);
        assert!(profile.ephemeral);
        assert_eq!(profile.profile_directory, None);
        assert!(
            profile
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rub-chrome-hex-"))
        );
    }

    #[test]
    fn explicit_profile_dir_is_not_ephemeral() {
        let profile =
            resolve_managed_profile_dir(Some(PathBuf::from("/tmp/custom-profile")), None, false);
        assert_eq!(
            profile,
            ManagedProfileDir {
                path: PathBuf::from("/tmp/custom-profile"),
                profile_directory: None,
                ephemeral: false,
            }
        );
    }

    #[test]
    fn explicit_profile_dir_can_remain_ephemeral_when_authority_was_derived_upstream() {
        let profile =
            resolve_managed_profile_dir(Some(PathBuf::from("/tmp/derived-profile")), None, true);
        assert_eq!(
            profile,
            ManagedProfileDir {
                path: PathBuf::from("/tmp/derived-profile"),
                profile_directory: None,
                ephemeral: true,
            }
        );
    }

    #[test]
    fn explicit_durable_prelaunch_does_not_clear_existing_temp_owned_marker() {
        let profile = resolve_managed_profile_dir(
            Some(
                std::env::temp_dir().join(format!("rub-chrome-prelaunch-{}", uuid::Uuid::now_v7())),
            ),
            None,
            false,
        );
        let _ = std::fs::remove_dir_all(&profile.path);
        sync_temp_owned_managed_profile_marker(&profile.path, true)
            .expect("seed temp-owned marker");

        prepare_managed_profile_ownership_prelaunch(&profile)
            .expect("explicit durable prelaunch should be a no-op");
        assert!(
            has_temp_owned_managed_profile_marker(&profile.path),
            "prelaunch durable path must not revoke existing temp-owned cleanup authority"
        );

        let _ = std::fs::remove_dir_all(&profile.path);
    }

    #[test]
    fn durable_ownership_commit_clears_temp_owned_marker_only_after_commit() {
        let profile = resolve_managed_profile_dir(
            Some(std::env::temp_dir().join(format!("rub-chrome-commit-{}", uuid::Uuid::now_v7()))),
            None,
            false,
        );
        let _ = std::fs::remove_dir_all(&profile.path);
        sync_temp_owned_managed_profile_marker(&profile.path, true)
            .expect("seed temp-owned marker");

        commit_managed_profile_ownership(&profile)
            .expect("durable ownership commit should clear the marker");
        assert!(
            !has_temp_owned_managed_profile_marker(&profile.path),
            "durable ownership adoption should clear temp-owned proof only at commit time"
        );

        let _ = std::fs::remove_dir_all(&profile.path);
    }

    #[test]
    fn ephemeral_prelaunch_rollback_removes_marker_and_profile_residue() {
        let profile = resolve_managed_profile_dir(
            Some(std::env::temp_dir().join(format!(
                "rub-chrome-prelaunch-rollback-{}",
                uuid::Uuid::now_v7()
            ))),
            None,
            true,
        );
        let _ = std::fs::remove_dir_all(&profile.path);
        prepare_managed_profile_ownership_prelaunch(&profile)
            .expect("ephemeral prelaunch should write temp-owned proof");
        std::fs::write(profile.path.join("residue.txt"), "launch failed")
            .expect("seed prelaunch residue");

        rollback_managed_profile_ownership_prelaunch(&profile)
            .expect("failed ephemeral launch should rollback temp-owned profile authority");

        assert!(
            !profile.path.exists(),
            "ephemeral launch failure must not leave temp profile or marker residue"
        );
    }

    #[test]
    #[cfg(unix)]
    fn failed_ephemeral_prelaunch_rollback_preserves_marker_for_retry_authority() {
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::temp_dir().join(format!(
            "rub-chrome-prelaunch-rollback-parent-{}",
            uuid::Uuid::now_v7()
        ));
        let profile = resolve_managed_profile_dir(Some(parent.join("profile")), None, true);
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).expect("create readonly parent");
        prepare_managed_profile_ownership_prelaunch(&profile)
            .expect("ephemeral prelaunch should write temp-owned proof");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500))
            .expect("make parent non-writable");

        let error = rollback_managed_profile_ownership_prelaunch(&profile)
            .expect_err("non-writable parent should block profile removal");

        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
            .expect("restore parent permissions");
        assert!(
            has_temp_owned_managed_profile_marker(&profile.path),
            "failed rollback must retain temp-owned marker as fallback cleanup authority: {error}"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn root_pid_selection_prefers_browser_process_without_type_flag() {
        let snapshot = vec![
            ProcessInfo {
                pid: 100,
                ppid: 1,
                command:
                    "Google Chrome --user-data-dir=/tmp/rub-chrome-100 --remote-debugging-port=0"
                        .to_string(),
            },
            ProcessInfo {
                pid: 101,
                ppid: 100,
                command: "Google Chrome Helper --type=renderer --user-data-dir=/tmp/rub-chrome-100"
                    .to_string(),
            },
        ];

        let root = find_root_pid_in_snapshot(
            &snapshot,
            &ManagedProfileDir {
                path: PathBuf::from("/tmp/rub-chrome-100"),
                profile_directory: None,
                ephemeral: false,
            },
        );
        assert_eq!(root, Some(100));
    }

    #[test]
    fn root_pid_selection_ignores_non_browser_processes_with_matching_profile_arg() {
        let snapshot = vec![ProcessInfo {
            pid: 200,
            ppid: 1,
            command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-200".to_string(),
        }];

        let root = find_root_pid_in_snapshot(
            &snapshot,
            &ManagedProfileDir {
                path: PathBuf::from("/tmp/rub-chrome-200"),
                profile_directory: None,
                ephemeral: false,
            },
        );
        assert_eq!(root, None);
    }

    #[test]
    fn authoritative_process_tree_requires_same_profile_authority_for_root_pid() {
        let matching = vec![
            ProcessInfo {
                pid: 300,
                ppid: 1,
                command:
                    "Google Chrome --user-data-dir=/tmp/rub-chrome-300 --remote-debugging-port=0"
                        .to_string(),
            },
            ProcessInfo {
                pid: 301,
                ppid: 300,
                command: "Google Chrome Helper --type=renderer --user-data-dir=/tmp/rub-chrome-300"
                    .to_string(),
            },
        ];
        assert_eq!(
            authoritative_process_tree(
                &matching,
                300,
                &ManagedProfileDir {
                    path: PathBuf::from("/tmp/rub-chrome-300"),
                    profile_directory: None,
                    ephemeral: false,
                },
            )
            .map(|tree| tree.len()),
            Some(2)
        );

        let reused = vec![ProcessInfo {
            pid: 300,
            ppid: 1,
            command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-300".to_string(),
        }];
        assert!(
            authoritative_process_tree(
                &reused,
                300,
                &ManagedProfileDir {
                    path: PathBuf::from("/tmp/rub-chrome-300"),
                    profile_directory: None,
                    ephemeral: false,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn authoritative_sigkill_tree_reports_reused_root_pid_as_lost_authority() {
        let reused = vec![ProcessInfo {
            pid: 300,
            ppid: 1,
            command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-300".to_string(),
        }];
        assert!(
            authoritative_sigkill_tree(
                &reused,
                300,
                &ManagedProfileDir {
                    path: PathBuf::from("/tmp/rub-chrome-300"),
                    profile_directory: None,
                    ephemeral: false,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn sigkill_authority_lost_error_projects_unsafe_to_kill_contract() {
        let error = sigkill_authority_lost_error(
            &ManagedProfileDir {
                path: PathBuf::from("/tmp/rub-chrome-300"),
                profile_directory: None,
                ephemeral: false,
            },
            300,
        )
        .into_envelope();

        assert_eq!(error.code, ErrorCode::ProfileInUse);
        let context = error.context.expect("context");
        assert_eq!(context["reason"], "managed_browser_sigkill_authority_lost");
        assert_eq!(context["unsafe_to_kill"], true);
        assert_eq!(
            context["cleanup_authority"],
            "managed_browser_process_tree_revalidation"
        );
    }

    #[test]
    fn managed_profile_residue_pids_include_only_browser_profile_survivors_without_root_authority()
    {
        let snapshot = vec![
            ProcessInfo {
                pid: 401,
                ppid: 1,
                command: "Google Chrome Helper --type=renderer --user-data-dir=/tmp/rub-chrome-401"
                    .to_string(),
            },
            ProcessInfo {
                pid: 402,
                ppid: 1,
                command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-401"
                    .to_string(),
            },
        ];

        let residue = managed_profile_residue_pids(
            &snapshot,
            &ManagedProfileDir {
                path: PathBuf::from("/tmp/rub-chrome-401"),
                profile_directory: None,
                ephemeral: false,
            },
        );
        assert_eq!(residue.len(), 1);
        assert!(residue.contains(&401));
        assert!(
            !residue.contains(&402),
            "non-browser processes may carry a --user-data-dir flag without owning browser profile locks"
        );
    }

    #[test]
    fn browser_profile_residue_cleanup_authority_is_chromium_only() {
        let browser_only = vec![ProcessInfo {
            pid: 410,
            ppid: 1,
            command: "Google Chrome Helper --type=renderer --user-data-dir=/tmp/rub-chrome-410"
                .to_string(),
        }];
        let residue = HashSet::from([410]);
        assert!(profile_residue_is_chromium_only(&browser_only, &residue));

        let mixed = vec![
            ProcessInfo {
                pid: 411,
                ppid: 1,
                command: "Google Chrome Helper --type=renderer --user-data-dir=/tmp/rub-chrome-411"
                    .to_string(),
            },
            ProcessInfo {
                pid: 412,
                ppid: 1,
                command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-411"
                    .to_string(),
            },
        ];
        let residue = HashSet::from([411, 412]);
        assert!(
            !profile_residue_is_chromium_only(&mixed, &residue),
            "non-browser processes with a matching profile flag are not owned by browser cleanup"
        );
    }

    #[test]
    fn profile_scoped_authority_requires_matching_profile_directory() {
        let snapshot = vec![
            ProcessInfo {
                pid: 500,
                ppid: 1,
                command: "Google Chrome --user-data-dir=/Users/test/Chrome --profile-directory=Profile 3 --remote-debugging-port=0".to_string(),
            },
            ProcessInfo {
                pid: 501,
                ppid: 1,
                command: "Google Chrome --user-data-dir=/Users/test/Chrome --profile-directory=Profile 2 --remote-debugging-port=0".to_string(),
            },
        ];

        let authority = ManagedProfileDir {
            path: PathBuf::from("/Users/test/Chrome"),
            profile_directory: Some("Profile 3".to_string()),
            ephemeral: false,
        };
        assert_eq!(find_root_pid_in_snapshot(&snapshot, &authority), Some(500));

        let sibling_authority = ManagedProfileDir {
            path: PathBuf::from("/Users/test/Chrome"),
            profile_directory: Some("Profile 4".to_string()),
            ephemeral: false,
        };
        assert_eq!(
            find_root_pid_in_snapshot(&snapshot, &sibling_authority),
            None
        );
    }

    #[test]
    fn profile_scoped_residue_ignores_sibling_profile_under_same_user_data_dir() {
        let snapshot = vec![
            ProcessInfo {
                pid: 601,
                ppid: 1,
                command: "Google Chrome Helper --type=renderer --user-data-dir=/Users/test/Chrome --profile-directory=Profile 3".to_string(),
            },
            ProcessInfo {
                pid: 602,
                ppid: 1,
                command: "Google Chrome Helper --type=renderer --user-data-dir=/Users/test/Chrome --profile-directory=Profile 2".to_string(),
            },
        ];

        let residue = managed_profile_residue_pids(
            &snapshot,
            &ManagedProfileDir {
                path: PathBuf::from("/Users/test/Chrome"),
                profile_directory: Some("Profile 3".to_string()),
                ephemeral: false,
            },
        );
        assert_eq!(residue, HashSet::from([601]));
    }

    #[test]
    fn extract_flag_value_handles_quoted_paths_with_spaces() {
        let inline =
            r#"Google Chrome --user-data-dir="/tmp/rub chrome 100" --remote-debugging-port=0"#;
        assert_eq!(
            extract_flag_value(inline, "--user-data-dir"),
            Some("/tmp/rub chrome 100".to_string())
        );

        let separated =
            r#"Google Chrome --user-data-dir "/tmp/rub chrome 200" --remote-debugging-port=0"#;
        assert_eq!(
            extract_flag_value(separated, "--user-data-dir"),
            Some("/tmp/rub chrome 200".to_string())
        );
    }

    #[test]
    fn process_snapshot_line_preserves_command_with_embedded_spaces() {
        let parsed = parse_process_snapshot_line(
            r#"  123  1 Google Chrome --user-data-dir="/tmp/rub chrome 300" --remote-debugging-port=0"#,
        )
        .expect("snapshot line should parse");
        assert_eq!(parsed.pid, 123);
        assert_eq!(parsed.ppid, 1);
        assert_eq!(
            extract_flag_value(&parsed.command, "--user-data-dir"),
            Some("/tmp/rub chrome 300".to_string())
        );
    }

    #[tokio::test]
    async fn cleanup_managed_profile_authority_is_noop_when_no_browser_owns_profile() {
        let profile_dir =
            std::env::temp_dir().join(format!("rub-managed-cleanup-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&profile_dir).expect("create profile dir");
        cleanup_managed_profile_authority(&profile_dir, None, true)
            .await
            .expect("cleanup should succeed without owned browser");
        assert!(
            !profile_dir.exists(),
            "ephemeral profile authority should be removed once cleanup completes"
        );
    }
}
