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
    ProcessInfo, extract_flag_value, is_browser_root_process, is_process_alive,
    process_snapshot as collect_process_snapshot, process_tree,
};
use tokio::time::{sleep, timeout};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedProfileDir {
    pub path: PathBuf,
    pub ephemeral: bool,
}

pub(crate) fn projected_managed_profile_path_for_scope(scope: &str) -> PathBuf {
    shared_projected_managed_profile_path_for_scope(scope)
}

pub(crate) fn resolve_managed_profile_dir(
    explicit: Option<PathBuf>,
    explicit_ephemeral: bool,
) -> ManagedProfileDir {
    match explicit {
        Some(path) => ManagedProfileDir {
            path,
            ephemeral: explicit_ephemeral,
        },
        None => ManagedProfileDir {
            path: projected_managed_profile_path_for_scope(&format!("pid-{}", std::process::id())),
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
    resolve_managed_profile_dir(explicit, false).path
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
            "managed_profile_ephemeral": profile.ephemeral,
            "operation": operation,
        }),
    )
}

pub(crate) async fn shutdown_managed_browser(
    browser: &Browser,
    profile: &ManagedProfileDir,
) -> Result<(), RubError> {
    let root_pid = find_managed_browser_root_pid(&profile.path)?;

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
    ephemeral: bool,
) -> Result<(), RubError> {
    let profile = ManagedProfileDir {
        path: profile_dir.into(),
        ephemeral,
    };
    let root_pid = find_managed_browser_root_pid(&profile.path)?;
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
            let Some(tree) = authoritative_process_tree(&snapshot, root_pid, &profile.path) else {
                wait_for_profile_release(&profile.path, Duration::from_secs(2)).await?;
                if is_profile_in_use(&profile.path)? {
                    return Err(RubError::domain_with_context(
                        ErrorCode::ProfileInUse,
                        format!(
                            "Managed browser root process {root_pid} could not be revalidated before shutdown fencing for profile {}",
                            profile.path.display()
                        ),
                        serde_json::json!({
                            "user_data_dir": profile.path.display().to_string(),
                            "root_pid": root_pid,
                            "reason": "managed_browser_root_authority_lost_before_kill",
                        }),
                    ));
                }
                if profile.ephemeral {
                    let _ = std::fs::remove_dir_all(&profile.path);
                }
                return Ok(());
            };
            terminate_process_tree(&tree).await;
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
                        "root_pid": root_pid,
                    }),
                ));
            }
        }
    }

    wait_for_profile_release(&profile.path, Duration::from_secs(2)).await?;
    if is_profile_in_use(&profile.path)? {
        return Err(RubError::domain_with_context(
            ErrorCode::ProfileInUse,
            format!(
                "Managed browser profile {} is still in use after shutdown fencing",
                profile.path.display()
            ),
            serde_json::json!({
                "user_data_dir": profile.path.display().to_string(),
            }),
        ));
    }

    if profile.ephemeral {
        let _ = std::fs::remove_dir_all(&profile.path);
    }

    Ok(())
}

pub fn is_profile_in_use(profile_dir: &Path) -> Result<bool, RubError> {
    let snapshot = process_snapshot()?;
    Ok(find_root_pid_in_snapshot(&snapshot, profile_dir).is_some())
}

fn find_managed_browser_root_pid(profile_dir: &Path) -> Result<Option<u32>, RubError> {
    let snapshot = process_snapshot()?;
    Ok(find_root_pid_in_snapshot(&snapshot, profile_dir))
}

fn find_root_pid_in_snapshot(snapshot: &[ProcessInfo], profile_dir: &Path) -> Option<u32> {
    snapshot
        .iter()
        .find(|process| {
            is_browser_root_process(&process.command)
                && extract_flag_value(&process.command, "--user-data-dir")
                    .as_deref()
                    .map(Path::new)
                    .is_some_and(|candidate| {
                        candidate == profile_dir
                            || managed_profile_paths_equivalent(candidate, profile_dir)
                    })
        })
        .map(|process| process.pid)
}

fn authoritative_process_tree(
    snapshot: &[ProcessInfo],
    root_pid: u32,
    profile_dir: &Path,
) -> Option<HashSet<u32>> {
    snapshot
        .iter()
        .find(|process| {
            process.pid == root_pid
                && is_browser_root_process(&process.command)
                && extract_flag_value(&process.command, "--user-data-dir")
                    .as_deref()
                    .map(Path::new)
                    .is_some_and(|candidate| {
                        candidate == profile_dir
                            || managed_profile_paths_equivalent(candidate, profile_dir)
                    })
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
    if processes.is_empty() {
        return;
    }

    for pid in processes {
        unsafe {
            libc::kill(*pid as i32, libc::SIGTERM);
        }
    }

    sleep(Duration::from_millis(500)).await;

    for pid in processes {
        if is_process_alive(*pid) {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }
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

async fn wait_for_profile_release(profile_dir: &Path, budget: Duration) -> Result<(), RubError> {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if !is_profile_in_use(profile_dir)? {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ManagedProfileDir, authoritative_process_tree, cleanup_managed_profile_authority,
        commit_managed_profile_ownership, find_root_pid_in_snapshot,
        prepare_managed_profile_ownership_prelaunch, resolve_managed_profile_dir,
    };
    use rub_core::managed_profile::{
        has_temp_owned_managed_profile_marker, sync_temp_owned_managed_profile_marker,
    };
    use rub_core::process::{ProcessInfo, extract_flag_value, parse_process_snapshot_line};
    use std::path::{Path, PathBuf};

    #[test]
    fn generated_profile_dir_is_ephemeral() {
        let profile = resolve_managed_profile_dir(None, false);
        assert!(profile.ephemeral);
        assert!(
            profile
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rub-chrome-pid-"))
        );
    }

    #[test]
    fn explicit_profile_dir_is_not_ephemeral() {
        let profile =
            resolve_managed_profile_dir(Some(PathBuf::from("/tmp/custom-profile")), false);
        assert_eq!(
            profile,
            ManagedProfileDir {
                path: PathBuf::from("/tmp/custom-profile"),
                ephemeral: false,
            }
        );
    }

    #[test]
    fn explicit_profile_dir_can_remain_ephemeral_when_authority_was_derived_upstream() {
        let profile =
            resolve_managed_profile_dir(Some(PathBuf::from("/tmp/derived-profile")), true);
        assert_eq!(
            profile,
            ManagedProfileDir {
                path: PathBuf::from("/tmp/derived-profile"),
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

        let root = find_root_pid_in_snapshot(&snapshot, Path::new("/tmp/rub-chrome-100"));
        assert_eq!(root, Some(100));
    }

    #[test]
    fn root_pid_selection_ignores_non_browser_processes_with_matching_profile_arg() {
        let snapshot = vec![ProcessInfo {
            pid: 200,
            ppid: 1,
            command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-200".to_string(),
        }];

        let root = find_root_pid_in_snapshot(&snapshot, Path::new("/tmp/rub-chrome-200"));
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
            authoritative_process_tree(&matching, 300, Path::new("/tmp/rub-chrome-300"))
                .map(|tree| tree.len()),
            Some(2)
        );

        let reused = vec![ProcessInfo {
            pid: 300,
            ppid: 1,
            command: "/usr/bin/python helper.py --user-data-dir=/tmp/rub-chrome-300".to_string(),
        }];
        assert!(
            authoritative_process_tree(&reused, 300, Path::new("/tmp/rub-chrome-300")).is_none()
        );
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
        cleanup_managed_profile_authority(&profile_dir, true)
            .await
            .expect("cleanup should succeed without owned browser");
        assert!(
            !profile_dir.exists(),
            "ephemeral profile authority should be removed once cleanup completes"
        );
    }
}
