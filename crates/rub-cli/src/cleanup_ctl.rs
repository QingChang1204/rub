//! Cleanup control — stale current-home sessions plus orphaned temporary rub artifacts.

mod current_home;
mod projection;
mod temp_runtime;
mod temp_sweep;
mod upgrade_probe;

use std::path::Path;
use std::time::Instant;

use crate::daemon_ctl::CompatibilityDegradedOwnedSession;
use rub_core::error::RubError;
use serde::Serialize;

use self::current_home::cleanup_current_home_stale;
use self::temp_runtime::process_snapshot;
pub(crate) use self::temp_sweep::{TempHomeDeleteDecision, revalidated_temp_home_delete_decision};
use self::temp_sweep::{sweep_orphan_temp_browsers, sweep_stale_temp_homes, sweep_temp_daemons};

#[cfg(test)]
static FORCE_TEMP_DAEMON_SWEEP_TIMEOUT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, Clone, Serialize)]
pub struct CleanupResult {
    pub cleaned_stale_sessions: Vec<String>,
    pub kept_active_sessions: Vec<String>,
    pub compatibility_degraded_owned_sessions: Vec<CompatibilityDegradedOwnedSession>,
    pub skipped_unreachable_sessions: Vec<String>,
    pub cleaned_temp_daemons: Vec<String>,
    pub skipped_busy_temp_daemons: Vec<String>,
    pub skipped_best_effort_phases: Vec<String>,
    pub active_temp_homes_authority_complete: bool,
    pub removed_temp_homes: Vec<String>,
    pub killed_orphan_browser_pids: Vec<u32>,
    pub removed_orphan_browser_profiles: Vec<String>,
}

impl Default for CleanupResult {
    fn default() -> Self {
        Self {
            cleaned_stale_sessions: Vec::new(),
            kept_active_sessions: Vec::new(),
            compatibility_degraded_owned_sessions: Vec::new(),
            skipped_unreachable_sessions: Vec::new(),
            cleaned_temp_daemons: Vec::new(),
            skipped_busy_temp_daemons: Vec::new(),
            skipped_best_effort_phases: Vec::new(),
            active_temp_homes_authority_complete: true,
            removed_temp_homes: Vec::new(),
            killed_orphan_browser_pids: Vec::new(),
            removed_orphan_browser_profiles: Vec::new(),
        }
    }
}

impl CleanupResult {
    pub(crate) fn degraded_under_shared_deadline(&self) -> bool {
        !self.skipped_best_effort_phases.is_empty() || !self.active_temp_homes_authority_complete
    }

    pub(crate) fn has_compatibility_degraded_owned_sessions(&self) -> bool {
        !self.compatibility_degraded_owned_sessions.is_empty()
    }

    pub(crate) fn first_skipped_best_effort_phase(&self) -> Option<&str> {
        self.skipped_best_effort_phases.first().map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy)]
struct UpgradeStatus {
    idle: bool,
}

pub async fn cleanup_runtime(rub_home: &Path, timeout_ms: u64) -> Result<CleanupResult, RubError> {
    let deadline = crate::timeout_budget::deadline_from_start(Instant::now(), timeout_ms);
    cleanup_runtime_until(rub_home, deadline, timeout_ms).await
}

pub(crate) async fn cleanup_runtime_until(
    rub_home: &Path,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<CleanupResult, RubError> {
    let mut result = CleanupResult::default();
    cleanup_current_home_stale(rub_home, deadline, timeout_ms, &mut result).await?;

    let snapshot = process_snapshot()?;
    let temp_daemon_sweep_result = {
        #[cfg(test)]
        {
            match maybe_force_temp_daemon_sweep_timeout_for_test(timeout_ms) {
                Ok(()) => {
                    sweep_temp_daemons(rub_home, &snapshot, deadline, timeout_ms, &mut result).await
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(not(test))]
        {
            sweep_temp_daemons(rub_home, &snapshot, deadline, timeout_ms, &mut result).await
        }
    };
    let (active_temp_homes, active_temp_homes_authority_complete) = match temp_daemon_sweep_result {
        Ok(active_temp_homes) => (active_temp_homes, true),
        Err(error) if best_effort_cleanup_timeout(&error) => {
            result.active_temp_homes_authority_complete = false;
            result
                .skipped_best_effort_phases
                .push("temp_daemon_sweep_timeout".to_string());
            (std::collections::HashSet::new(), false)
        }
        Err(error) => return Err(error),
    };

    let post_daemon_snapshot = process_snapshot()?;
    match crate::timeout_budget::run_with_remaining_budget(
        deadline,
        timeout_ms,
        "cleanup_orphan_browser_sweep",
        async {
            sweep_orphan_temp_browsers(&post_daemon_snapshot, &mut result).await;
            Ok::<(), RubError>(())
        },
    )
    .await
    {
        Ok(()) => {}
        Err(error) if best_effort_cleanup_timeout(&error) => {
            result
                .skipped_best_effort_phases
                .push("orphan_browser_sweep_timeout".to_string());
        }
        Err(error) => return Err(error),
    }
    if active_temp_homes_authority_complete {
        match crate::timeout_budget::ensure_remaining_budget(
            deadline,
            timeout_ms,
            "cleanup_temp_home_sweep",
        ) {
            Ok(()) => {
                if !sweep_stale_temp_homes(rub_home, &active_temp_homes, &mut result) {
                    result
                        .skipped_best_effort_phases
                        .push("temp_home_sweep_authority_incomplete".to_string());
                }
            }
            Err(error) if best_effort_cleanup_timeout(&error) => {
                result
                    .skipped_best_effort_phases
                    .push("temp_home_sweep_timeout".to_string());
            }
            Err(error) => return Err(error),
        }
    } else {
        result
            .skipped_best_effort_phases
            .push("temp_home_sweep_authority_incomplete".to_string());
    }

    sort_and_dedup(&mut result.cleaned_stale_sessions);
    sort_and_dedup(&mut result.kept_active_sessions);
    result.compatibility_degraded_owned_sessions.sort();
    result.compatibility_degraded_owned_sessions.dedup();
    sort_and_dedup(&mut result.skipped_unreachable_sessions);
    sort_and_dedup(&mut result.cleaned_temp_daemons);
    sort_and_dedup(&mut result.skipped_busy_temp_daemons);
    sort_and_dedup(&mut result.skipped_best_effort_phases);
    sort_and_dedup(&mut result.removed_temp_homes);
    result.killed_orphan_browser_pids.sort_unstable();
    result.killed_orphan_browser_pids.dedup();
    sort_and_dedup(&mut result.removed_orphan_browser_profiles);

    Ok(result)
}

#[cfg(test)]
fn maybe_force_temp_daemon_sweep_timeout_for_test(timeout_ms: u64) -> Result<(), RubError> {
    if FORCE_TEMP_DAEMON_SWEEP_TIMEOUT.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(crate::main_support::command_timeout_error(
            timeout_ms,
            "cleanup_temp_daemon_sweep",
        ));
    }
    Ok(())
}

fn best_effort_cleanup_timeout(error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(envelope) if envelope.code == rub_core::error::ErrorCode::IpcTimeout
    )
}

pub fn project_cleanup_result(rub_home: &Path, result: &CleanupResult) -> serde_json::Value {
    projection::project_cleanup_result(rub_home, result)
}

pub(crate) fn temp_home_delete_decision(path: &Path) -> TempHomeDeleteDecision {
    revalidated_temp_home_delete_decision(path)
}

fn sort_and_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

#[cfg(test)]
mod tests {
    use super::CleanupResult;
    use super::current_home::cleanup_current_home_stale;
    use super::projection::{CleanupPathContext, cleanup_path_error, project_cleanup_result};
    use super::temp_runtime::{
        TempDaemonProcess, cleanup_temp_daemon_registry_state, daemon_process_matches_authority,
        extract_temp_browser_root, is_rub_daemon_command, is_temp_rub_home,
        orphan_temp_browser_pids_for_roots, orphan_temp_browser_roots,
        revalidated_temp_daemon_sigkill_tree, revalidated_temp_daemon_tree,
        root_has_live_browser_process, temp_daemon_processes,
    };
    use super::upgrade_probe::{cleanup_upgrade_status_error, fetch_upgrade_status_for_session};
    use crate::cleanup_ctl::{FORCE_TEMP_DAEMON_SWEEP_TIMEOUT, cleanup_runtime_until};
    use rub_core::error::ErrorCode;
    use rub_core::process::ProcessInfo;
    use rub_core::process::{extract_flag_value, process_has_ancestor, process_tree};
    use rub_daemon::rub_paths::RubPaths;
    use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn unique_temp_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }

    fn unique_temp_owned_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rub-temp-owned-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn cleanup_runtime_skips_temp_home_removal_when_temp_home_authority_is_incomplete() {
        let current_home = unique_temp_owned_home("rub-cleanup-current");
        let stale_temp_home = unique_temp_owned_home("rub-cleanup-stale");
        let _ = std::fs::remove_dir_all(&current_home);
        let _ = std::fs::remove_dir_all(&stale_temp_home);
        std::fs::create_dir_all(&current_home).unwrap();
        std::fs::create_dir_all(&stale_temp_home).unwrap();
        std::fs::write(
            RubPaths::new(&current_home).temp_home_owner_marker_path(),
            "",
        )
        .unwrap();
        std::fs::write(
            RubPaths::new(&stale_temp_home).temp_home_owner_marker_path(),
            "",
        )
        .unwrap();
        FORCE_TEMP_DAEMON_SWEEP_TIMEOUT.store(true, std::sync::atomic::Ordering::SeqCst);

        let result = cleanup_runtime_until(
            &current_home,
            std::time::Instant::now() + std::time::Duration::from_millis(1_000),
            1_000,
        )
        .await
        .expect("cleanup should degrade instead of deleting temp homes on unknown authority");

        assert!(
            !result.active_temp_homes_authority_complete,
            "cleanup must surface incomplete temp-home authority when temp-daemon sweep times out"
        );
        assert!(
            result
                .skipped_best_effort_phases
                .contains(&"temp_daemon_sweep_timeout".to_string())
        );
        assert!(
            result
                .skipped_best_effort_phases
                .contains(&"temp_home_sweep_authority_incomplete".to_string())
        );
        assert!(result.removed_temp_homes.is_empty());
        assert!(stale_temp_home.exists());

        let _ = std::fs::remove_dir_all(&current_home);
        let _ = std::fs::remove_dir_all(&stale_temp_home);
    }

    #[test]
    #[serial_test::serial]
    fn temp_rub_home_requires_temp_root_and_owner_marker() {
        let temp_home = unique_temp_owned_home("rub-e2e");
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
    fn project_cleanup_result_marks_local_cleanup_path_references() {
        let projected = project_cleanup_result(
            PathBuf::from("/tmp/rub-home").as_path(),
            &CleanupResult {
                removed_temp_homes: vec!["/tmp/rub-temp-a".to_string()],
                removed_orphan_browser_profiles: vec!["/tmp/rub-chrome-a".to_string()],
                ..CleanupResult::default()
            },
        );

        assert_eq!(
            projected["subject"]["rub_home_state"]["path_authority"],
            "cli.cleanup.subject.rub_home"
        );
        assert_eq!(
            projected["result"]["removed_temp_home_refs"][0]["path_state"]["path_kind"],
            "temp_owned_rub_home"
        );
        assert_eq!(
            projected["result"]["removed_orphan_browser_profile_refs"][0]["path_state"]["path_authority"],
            "cli.cleanup.result.removed_orphan_browser_profiles"
        );
    }

    #[test]
    fn cleanup_path_error_preserves_rub_home_state() {
        let error = cleanup_path_error(
            ErrorCode::DaemonStartFailed,
            "boom".to_string(),
            CleanupPathContext {
                path_key: "rub_home",
                path: Path::new("/tmp/rub-home"),
                path_authority: "cli.cleanup.subject.rub_home",
                upstream_truth: "cli_rub_home",
                path_kind: "cleanup_home_directory",
                reason: "cleanup_registry_read_failed",
            },
        )
        .into_envelope();
        let context = error.context.expect("cleanup path error context");
        assert_eq!(context["reason"], "cleanup_registry_read_failed");
        assert_eq!(
            context["rub_home_state"]["path_authority"],
            "cli.cleanup.subject.rub_home"
        );
    }

    #[test]
    #[serial_test::serial]
    fn temp_rub_home_rejects_generic_mktemp_shape_even_when_owned() {
        let temp_home = std::env::temp_dir().join(format!(
            "tmp.{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).unwrap();
        assert!(!is_temp_rub_home(&temp_home));
        std::fs::write(RubPaths::new(&temp_home).temp_home_owner_marker_path(), "").unwrap();
        assert!(!is_temp_rub_home(&temp_home));
        let _ = std::fs::remove_dir_all(temp_home);
    }

    #[test]
    #[serial_test::serial]
    fn daemon_process_parser_extracts_session_and_home() {
        let temp_home = unique_temp_owned_home("rub-e2e");
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
        let daemon = TempDaemonProcess {
            pid: 42,
            session_name: "default".to_string(),
            session_id: "sess-old".to_string(),
            rub_home: PathBuf::from("/tmp/rub-home"),
            user_data_dir: None,
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
    fn revalidated_temp_daemon_sigkill_tree_drops_reused_pid() {
        let daemon = TempDaemonProcess {
            pid: 42,
            session_name: "default".to_string(),
            session_id: "sess-old".to_string(),
            rub_home: PathBuf::from("/tmp/rub-home"),
            user_data_dir: None,
        };
        let reused = ProcessInfo {
            pid: 42,
            ppid: 1,
            command:
                "rub __daemon --session default --session-id sess-new --rub-home /tmp/rub-home"
                    .to_string(),
        };
        assert!(revalidated_temp_daemon_sigkill_tree(&[reused], &daemon).is_empty());
    }

    #[test]
    #[serial_test::serial]
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

        cleanup_temp_daemon_registry_state(&TempDaemonProcess {
            pid: 111,
            session_name: "default".to_string(),
            session_id: "sess-old".to_string(),
            rub_home: temp_home.clone(),
            user_data_dir: None,
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

    #[test]
    fn orphan_temp_browser_roots_include_residual_profile_dirs_without_live_daemon_owner() {
        let pid = 400_000u32 + (uuid::Uuid::now_v7().as_u128() % 100_000) as u32;
        let root = std::env::temp_dir().join(format!("rub-chrome-{pid}"));
        let _ = std::fs::remove_dir_all(&root);
        rub_core::managed_profile::sync_temp_owned_managed_profile_marker(&root, true).unwrap();

        let orphan_roots = orphan_temp_browser_roots(&[]);
        assert!(
            orphan_roots.contains(&root),
            "cleanup sweep should discover residual managed browser profile directories even after all browser processes are gone"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_temp_browser_roots_include_session_scoped_profile_dirs_without_live_owner() {
        let root =
            rub_core::managed_profile::projected_managed_profile_path_for_session("sess-orphan");
        let _ = std::fs::remove_dir_all(&root);
        rub_core::managed_profile::sync_temp_owned_managed_profile_marker(&root, true).unwrap();

        let orphan_roots = orphan_temp_browser_roots(&[]);
        assert!(
            orphan_roots.contains(&root),
            "cleanup sweep should discover session-scoped managed browser profile residue without relying on pid-shaped directory names"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_temp_browser_roots_ignore_explicit_durable_tmp_profile_shape_without_marker() {
        let root = std::env::temp_dir().join("rub-chrome-my-workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let orphan_roots = orphan_temp_browser_roots(&[]);
        assert!(
            !orphan_roots.contains(&root),
            "cleanup sweep must not authorize deletion from path shape alone"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn root_has_live_browser_process_treats_tmp_aliases_as_same_profile_authority() {
        let snapshot = vec![ProcessInfo {
            pid: 42,
            ppid: 1,
            command:
                r#"chrome --type=browser --user-data-dir="/private/tmp/rub-chrome-session-alias""#
                    .to_string(),
        }];
        let alias_root = PathBuf::from("/tmp/rub-chrome-session-alias");
        assert!(
            root_has_live_browser_process(&snapshot, &alias_root),
            "liveness fence must compare managed profile aliases by normalized identity, not raw path strings"
        );
    }

    #[test]
    fn temp_browser_cleanup_treats_helper_processes_as_live_profile_authority() {
        let root = std::env::temp_dir().join("rub-chrome-helper-authority");
        let snapshot = vec![ProcessInfo {
            pid: 43,
            ppid: 1,
            command: format!(
                r#"Google Chrome Helper --type=renderer --user-data-dir="{}""#,
                root.display()
            ),
        }];

        assert!(
            root_has_live_browser_process(&snapshot, &root),
            "helper residue must block profile directory removal while the profile authority is still live"
        );
        assert_eq!(
            orphan_temp_browser_pids_for_roots(&snapshot, &HashSet::from([root.clone()])),
            HashSet::from([43]),
            "orphan cleanup must be able to terminate same-profile helper residue"
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

    #[test]
    fn cleanup_upgrade_status_error_preserves_socket_path_state() {
        let error = cleanup_upgrade_status_error(
            ErrorCode::IpcProtocolError,
            "boom".to_string(),
            Path::new("/tmp/rub.sock"),
            Some(serde_json::json!({ "upstream": "context" })),
            "cleanup_upgrade_check_protocol_failed",
        )
        .into_envelope();
        let context = error.context.expect("upgrade status error context");
        assert_eq!(context["upstream"], "context");
        assert_eq!(context["reason"], "cleanup_upgrade_check_protocol_failed");
        assert_eq!(
            context["socket_path_state"]["path_authority"],
            "cli.cleanup.upgrade_check.socket_path"
        );
    }

    #[tokio::test]
    async fn cleanup_current_home_stale_read_failure_preserves_rub_home_state() {
        let home = std::env::temp_dir().join(format!(
            "rub-cleanup-registry-failure-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::write(&home, b"not-a-directory").expect("seed invalid rub_home");

        let mut result = CleanupResult::default();
        let envelope = cleanup_current_home_stale(
            &home,
            std::time::Instant::now() + std::time::Duration::from_millis(1_000),
            1_000,
            &mut result,
        )
        .await
        .expect_err("registry read failure should propagate")
        .into_envelope();
        let context = envelope.context.expect("cleanup registry error context");
        assert_eq!(context["reason"], "cleanup_registry_read_failed");
        assert_eq!(
            context["rub_home_state"]["path_authority"],
            "cli.cleanup.subject.rub_home"
        );

        let _ = std::fs::remove_file(&home);
    }

    #[tokio::test]
    async fn cleanup_skips_pending_startup_entries() {
        let home = unique_temp_home("rub-cleanup-pending");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-pending");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        std::fs::write(session_paths.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::create_dir_all(session_paths.socket_path().parent().unwrap()).unwrap();
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
        cleanup_current_home_stale(
            &home,
            std::time::Instant::now() + std::time::Duration::from_millis(1_000),
            1_000,
            &mut result,
        )
        .await
        .unwrap();

        assert_eq!(result.skipped_unreachable_sessions, vec!["default"]);
        let registry = rub_daemon::session::read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn cleanup_skips_protocol_incompatible_owned_entries() {
        use std::io::{BufRead as _, BufReader as StdBufReader, Write as _};
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener as StdUnixListener;

        let home = unique_temp_home("rub-cleanup-incompatible-owned");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let session_name = "default";
        let session_id = "sess-incompatible";
        let session_paths = RubPaths::new(&home).session_runtime(session_name, session_id);
        let projection = RubPaths::new(&home).session(session_name);
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(session_paths.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(
            projection.canonical_pid_path(),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(projection.startup_committed_path(), session_id).unwrap();
        symlink(
            session_paths.socket_path(),
            projection.canonical_socket_path(),
        )
        .unwrap();

        let socket_path = session_paths.socket_path();
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let session_id_for_server = session_id.to_string();
        let listener = StdUnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            StdBufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let _: rub_ipc::protocol::IpcRequest =
                serde_json::from_str(request.trim_end()).unwrap();
            let mut response = rub_ipc::protocol::IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": session_id_for_server,
                }),
            );
            response.ipc_protocol_version = "1.0".to_string();
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: session_id.to_string(),
                    session_name: session_name.to_string(),
                    pid: std::process::id(),
                    socket_path: socket_path.display().to_string(),
                    created_at: "2026-04-02T00:00:00Z".to_string(),
                    ipc_protocol_version: "1.0".to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let mut result = CleanupResult::default();
        cleanup_current_home_stale(
            &home,
            std::time::Instant::now() + std::time::Duration::from_millis(1_000),
            1_000,
            &mut result,
        )
        .await
        .unwrap();

        assert!(result.skipped_unreachable_sessions.is_empty());
        assert_eq!(result.compatibility_degraded_owned_sessions.len(), 1);
        assert_eq!(
            result.compatibility_degraded_owned_sessions[0].session,
            "default"
        );
        assert_eq!(
            serde_json::to_value(&result.compatibility_degraded_owned_sessions[0]).unwrap()["reason"],
            serde_json::json!("protocol_incompatible")
        );
        assert!(result.cleaned_stale_sessions.is_empty());
        let registry = rub_daemon::session::read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }
}
