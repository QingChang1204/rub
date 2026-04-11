//! Cleanup control — stale current-home sessions plus orphaned temporary rub artifacts.

mod current_home;
mod projection;
mod temp_runtime;
mod temp_sweep;
mod upgrade_probe;

use std::path::Path;

use rub_core::error::RubError;
use serde::Serialize;

use self::current_home::cleanup_current_home_stale;
use self::temp_runtime::process_snapshot;
use self::temp_sweep::{sweep_orphan_temp_browsers, sweep_stale_temp_homes, sweep_temp_daemons};

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

pub fn project_cleanup_result(rub_home: &Path, result: &CleanupResult) -> serde_json::Value {
    projection::project_cleanup_result(rub_home, result)
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
        orphan_temp_browser_roots, revalidated_temp_daemon_tree, temp_daemon_processes,
    };
    use super::upgrade_probe::{cleanup_upgrade_status_error, fetch_upgrade_status_for_session};
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
        let daemon = TempDaemonProcess {
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

        cleanup_temp_daemon_registry_state(&TempDaemonProcess {
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

    #[test]
    fn orphan_temp_browser_roots_include_residual_profile_dirs_without_live_daemon_owner() {
        let root = std::env::temp_dir().join("rub-chrome-424242");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let orphan_roots = orphan_temp_browser_roots(&[]);
        assert!(
            orphan_roots.contains(&root),
            "cleanup sweep should discover residual managed browser profile directories even after all browser processes are gone"
        );

        let _ = std::fs::remove_dir_all(root);
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
        let envelope = cleanup_current_home_stale(&home, &mut result)
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
        cleanup_current_home_stale(&home, &mut result)
            .await
            .unwrap();

        assert_eq!(result.skipped_unreachable_sessions, vec!["default"]);
        let registry = rub_daemon::session::read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);

        let _ = std::fs::remove_dir_all(home);
    }
}
