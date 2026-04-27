//! Canonical lifecycle exit wrapper for one RUB_HOME.

use crate::{cleanup_ctl, daemon_ctl};
use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::is_temp_owned_home_cleanup_authoritative;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct TeardownResult {
    pub fully_released: bool,
    pub rub_home_removed: bool,
    pub close_all: daemon_ctl::BatchCloseResult,
    pub cleanup: cleanup_ctl::CleanupResult,
}

pub async fn teardown_runtime(
    rub_home: &Path,
    timeout_ms: u64,
) -> Result<TeardownResult, RubError> {
    let deadline = crate::timeout_budget::deadline_from_start(Instant::now(), timeout_ms);
    teardown_runtime_until(rub_home, deadline, timeout_ms).await
}

async fn teardown_runtime_until(
    rub_home: &Path,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<TeardownResult, RubError> {
    let temp_owned_home = rub_home.exists() && is_temp_owned_home_cleanup_authoritative(rub_home);
    let close_all = daemon_ctl::close_all_sessions_until(rub_home, deadline).await?;
    let cleanup = if rub_home.exists() {
        cleanup_ctl::cleanup_runtime_until(rub_home, deadline, timeout_ms)
            .await
            .map_err(|error| augment_cleanup_error(rub_home, &close_all, error))?
    } else {
        cleanup_ctl::CleanupResult::default()
    };
    let compatibility_degraded_owned = close_all.has_compatibility_degraded_owned_sessions()
        || cleanup.has_compatibility_degraded_owned_sessions();
    let cleanup_degraded = cleanup.degraded_under_shared_deadline();
    let rub_home_removed = if close_all.failed.is_empty()
        && !compatibility_degraded_owned
        && !cleanup_degraded
        && temp_owned_home
    {
        if !rub_home.exists() {
            true
        } else {
            release_temp_owned_home(rub_home, &close_all, &cleanup)?
        }
    } else {
        false
    };
    let result = TeardownResult {
        fully_released: close_all.failed.is_empty()
            && !compatibility_degraded_owned
            && !cleanup_degraded
            && (!temp_owned_home || rub_home_removed || !rub_home.exists()),
        rub_home_removed,
        close_all,
        cleanup,
    };

    if result.fully_released {
        Ok(result)
    } else if compatibility_degraded_owned {
        Err(teardown_compatibility_degraded_owned_error(
            rub_home, &result,
        ))
    } else if cleanup_degraded && result.close_all.failed.is_empty() {
        Err(teardown_cleanup_degraded_error(
            rub_home, &result, timeout_ms,
        ))
    } else {
        Err(teardown_incomplete_error(rub_home, &result))
    }
}

pub fn project_teardown_result(rub_home: &Path, result: &TeardownResult) -> Value {
    let close_projection = daemon_ctl::project_batch_close_result(rub_home, &result.close_all);
    let cleanup_projection = cleanup_ctl::project_cleanup_result(rub_home, &result.cleanup);
    let rub_home_state = cleanup_projection
        .get("subject")
        .and_then(|subject| subject.get("rub_home_state"))
        .cloned()
        .unwrap_or(Value::Null);
    let close_result = close_projection
        .get("result")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let cleanup_result = cleanup_projection
        .get("result")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));

    json!({
        "subject": {
            "kind": "runtime_teardown",
            "rub_home": rub_home.display().to_string(),
            "rub_home_state": rub_home_state,
        },
        "result": {
            "fully_released": result.fully_released,
            "rub_home_removed": result.rub_home_removed,
            "close_all": close_result,
            "cleanup": cleanup_result,
        }
    })
}

fn release_temp_owned_home(
    rub_home: &Path,
    close_all: &daemon_ctl::BatchCloseResult,
    cleanup: &cleanup_ctl::CleanupResult,
) -> Result<bool, RubError> {
    let delete_decision = cleanup_ctl::temp_home_delete_decision(rub_home);
    match delete_decision {
        cleanup_ctl::TempHomeDeleteDecision::Remove => {}
        cleanup_ctl::TempHomeDeleteDecision::SkipLiveOwner
        | cleanup_ctl::TempHomeDeleteDecision::SkipLiveDaemon
        | cleanup_ctl::TempHomeDeleteDecision::SkipLiveBrowser
        | cleanup_ctl::TempHomeDeleteDecision::SkipAuthorityIncomplete => {
            let reason = match delete_decision {
                cleanup_ctl::TempHomeDeleteDecision::SkipLiveOwner => {
                    "temp_owned_rub_home_live_owner_revalidated"
                }
                cleanup_ctl::TempHomeDeleteDecision::SkipLiveDaemon => {
                    "temp_owned_rub_home_live_daemon_revalidated"
                }
                cleanup_ctl::TempHomeDeleteDecision::SkipLiveBrowser => {
                    "temp_owned_rub_home_live_browser_revalidated"
                }
                cleanup_ctl::TempHomeDeleteDecision::SkipAuthorityIncomplete => {
                    "temp_owned_rub_home_delete_authority_incomplete"
                }
                cleanup_ctl::TempHomeDeleteDecision::Remove => unreachable!(),
            };
            let message = if reason == "temp_owned_rub_home_delete_authority_incomplete" {
                "Teardown refused to remove the temp-owned RUB_HOME because delete authority could not be revalidated"
            } else {
                "Teardown refused to remove the temp-owned RUB_HOME because live ownership was revalidated at delete time"
            };
            return Err(RubError::domain_with_context_and_suggestion(
                ErrorCode::SessionBusy,
                message,
                json!({
                    "reason": reason,
                    "rub_home": rub_home.display().to_string(),
                    "rub_home_state": daemon_ctl::daemon_ctl_path_state(
                        "cli.teardown.subject.rub_home",
                        "cli_rub_home",
                        "temp_owned_rub_home",
                    ),
                    "teardown": project_teardown_result(
                        rub_home,
                        &TeardownResult {
                            fully_released: false,
                            rub_home_removed: false,
                            close_all: close_all.clone(),
                            cleanup: cleanup.clone(),
                        },
                    ),
                }),
                "Retry 'rub teardown' after the revalidated owner is released, or inspect the remaining runtime with 'rub sessions' and 'rub doctor'.",
            ));
        }
    }
    match std::fs::remove_dir_all(rub_home) {
        Ok(()) => Ok(!rub_home.exists()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::SessionBusy,
            "Teardown released session authority but could not remove the temp-owned RUB_HOME",
            json!({
                "reason": "temp_owned_rub_home_release_failed",
                "rub_home": rub_home.display().to_string(),
                "rub_home_state": daemon_ctl::daemon_ctl_path_state(
                    "cli.teardown.subject.rub_home",
                    "cli_rub_home",
                    "temp_owned_rub_home",
                ),
                "error": error.to_string(),
                "teardown": project_teardown_result(
                    rub_home,
                    &TeardownResult {
                        fully_released: false,
                        rub_home_removed: false,
                        close_all: close_all.clone(),
                        cleanup: cleanup.clone(),
                    },
                ),
            }),
            "Retry 'rub teardown'. If the temp-owned home is still busy, inspect the remaining processes with 'rub sessions' or 'rub doctor'.",
        )),
    }
}

fn teardown_incomplete_error(rub_home: &Path, result: &TeardownResult) -> RubError {
    let cleanup_degraded = result.cleanup.degraded_under_shared_deadline();
    let message = if cleanup_degraded {
        "Teardown could not confirm shared-deadline cleanup after session shutdown"
    } else {
        "Teardown could not confirm shutdown for one or more sessions"
    };
    let suggestion = if cleanup_degraded {
        "Retry 'rub teardown'. The shared timeout budget exhausted during cleanup after close --all, so verify cleanup phases and remaining runtime residue before assuming release completed."
    } else {
        "Retry 'rub teardown' after the remaining session finishes shutting down, or inspect the failed session with 'rub sessions' and 'rub doctor'"
    };
    RubError::domain_with_context_and_suggestion(
        ErrorCode::SessionBusy,
        message,
        json!({
            "teardown": project_teardown_result(rub_home, result),
            "failed_sessions": result.close_all.failed,
            "session_error_details": result.close_all.session_error_details,
            "cleanup_skipped_best_effort_phases": result.cleanup.skipped_best_effort_phases,
            "reason": if cleanup_degraded {
                "teardown_cleanup_degraded_after_close_all"
            } else {
                "teardown_sessions_failed_to_release"
            },
        }),
        suggestion,
    )
}

fn teardown_compatibility_degraded_owned_error(
    rub_home: &Path,
    result: &TeardownResult,
) -> RubError {
    RubError::domain_with_context_and_suggestion(
        ErrorCode::SessionBusy,
        "Teardown preserved owned compatibility-degraded session authority instead of treating it as released",
        json!({
            "reason": "teardown_compatibility_degraded_owned_sessions",
            "teardown": project_teardown_result(rub_home, result),
            "session_error_details": result.close_all.session_error_details,
            "compatibility_degraded_owned_sessions": result.close_all.compatibility_degraded_owned_sessions,
            "cleanup_compatibility_degraded_owned_sessions": result.cleanup.compatibility_degraded_owned_sessions,
        }),
        "Retry 'rub teardown' after the incompatible or release-pending daemon is explicitly resolved; teardown will not remove the owned runtime while compatibility-degraded authority still remains.",
    )
}

fn teardown_cleanup_degraded_error(
    rub_home: &Path,
    result: &TeardownResult,
    timeout_ms: u64,
) -> RubError {
    let phase = match result.cleanup.first_skipped_best_effort_phase() {
        Some("temp_daemon_sweep_timeout") => "cleanup_temp_daemon_sweep",
        Some("orphan_browser_sweep_timeout") => "cleanup_orphan_browser_sweep",
        Some("temp_home_sweep_timeout") => "cleanup_temp_home_sweep",
        Some("temp_home_sweep_authority_incomplete") => "cleanup_temp_home_authority",
        Some(other) => other,
        None => "cleanup_best_effort_phase",
    };
    let mut envelope = crate::main_support::command_timeout_envelope_for_phase(timeout_ms, phase);
    let context = envelope
        .context
        .take()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut object = context.as_object().cloned().unwrap_or_default();
    object.insert(
        "reason".to_string(),
        Value::String("teardown_cleanup_degraded_under_shared_deadline".to_string()),
    );
    object.insert(
        "cleanup_skipped_best_effort_phases".to_string(),
        serde_json::to_value(&result.cleanup.skipped_best_effort_phases)
            .unwrap_or_else(|_| Value::Array(Vec::new())),
    );
    object.insert(
        "teardown".to_string(),
        project_teardown_result(rub_home, result),
    );
    envelope.context = Some(Value::Object(object));
    RubError::Domain(envelope)
}

fn augment_cleanup_error(
    rub_home: &Path,
    close_all: &daemon_ctl::BatchCloseResult,
    error: RubError,
) -> RubError {
    let mut envelope = error.into_envelope();
    let close_projection = daemon_ctl::project_batch_close_result(rub_home, close_all);
    let context = envelope
        .context
        .take()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut object = context.as_object().cloned().unwrap_or_default();
    object.insert(
        "teardown_phase".to_string(),
        Value::String("cleanup".to_string()),
    );
    object.insert("teardown_close_all".to_string(), close_projection);
    envelope.context = Some(Value::Object(object));
    RubError::Domain(envelope)
}

#[cfg(test)]
mod tests {
    use super::{
        TeardownResult, project_teardown_result, teardown_cleanup_degraded_error,
        teardown_compatibility_degraded_owned_error, teardown_incomplete_error,
    };
    use crate::{
        cleanup_ctl::CleanupResult,
        daemon_ctl::{
            BatchCloseResult, CompatibilityDegradedOwnedReason, CompatibilityDegradedOwnedSession,
        },
    };
    use rub_core::error::ErrorCode;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn project_teardown_result_combines_close_and_cleanup_surfaces() {
        let result = TeardownResult {
            fully_released: true,
            rub_home_removed: false,
            close_all: BatchCloseResult {
                closed: vec!["default".to_string()],
                cleaned_stale: vec![],
                compatibility_degraded_owned_sessions: vec![],
                failed: vec![],
                session_error_details: vec![],
            },
            cleanup: CleanupResult {
                removed_orphan_browser_profiles: vec!["/tmp/rub-chrome-a".to_string()],
                ..CleanupResult::default()
            },
        };

        let projected = project_teardown_result(PathBuf::from("/tmp/rub-home").as_path(), &result);
        assert_eq!(projected["subject"]["kind"], "runtime_teardown");
        assert_eq!(projected["result"]["fully_released"], true);
        assert_eq!(projected["result"]["rub_home_removed"], false);
        assert_eq!(
            projected["result"]["close_all"]["closed"],
            json!(["default"])
        );
        assert_eq!(
            projected["result"]["cleanup"]["removed_orphan_browser_profiles"],
            json!(["/tmp/rub-chrome-a"])
        );
    }

    #[test]
    fn teardown_incomplete_error_preserves_projected_result() {
        let result = TeardownResult {
            fully_released: false,
            rub_home_removed: false,
            close_all: BatchCloseResult {
                closed: vec![],
                cleaned_stale: vec![],
                compatibility_degraded_owned_sessions: vec![],
                failed: vec!["default".to_string()],
                session_error_details: vec![crate::daemon_ctl::BatchCloseSessionError {
                    session: "default".to_string(),
                    error: rub_core::error::ErrorEnvelope::new(
                        ErrorCode::IpcProtocolError,
                        "replay recovery failed",
                    )
                    .with_context(json!({
                        "reason": "ipc_replay_retry_failed",
                        "recovery_contract": {
                            "kind": "session_post_commit_journal",
                        },
                    })),
                }],
            },
            cleanup: CleanupResult::default(),
        };

        let envelope = teardown_incomplete_error(PathBuf::from("/tmp/rub-home").as_path(), &result)
            .into_envelope();
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("failed_sessions")),
            Some(&json!(["default"]))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("teardown_sessions_failed_to_release")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("teardown"))
                .and_then(|value| value.get("result"))
                .and_then(|value| value.get("fully_released")),
            Some(&json!(false))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("session_error_details"))
                .and_then(|value| value.get(0))
                .and_then(|value| value.get("error"))
                .and_then(|value| value.get("context"))
                .and_then(|value| value.get("recovery_contract"))
                .and_then(|value| value.get("kind")),
            Some(&json!("session_post_commit_journal"))
        );
    }

    #[test]
    fn teardown_cleanup_degraded_error_reports_timeout_phase_and_teardown_projection() {
        let result = TeardownResult {
            fully_released: false,
            rub_home_removed: false,
            close_all: BatchCloseResult {
                closed: vec!["default".to_string()],
                cleaned_stale: vec![],
                compatibility_degraded_owned_sessions: vec![],
                failed: vec![],
                session_error_details: vec![],
            },
            cleanup: CleanupResult {
                skipped_best_effort_phases: vec!["orphan_browser_sweep_timeout".to_string()],
                ..CleanupResult::default()
            },
        };

        let envelope = teardown_cleanup_degraded_error(
            PathBuf::from("/tmp/rub-home").as_path(),
            &result,
            1_500,
        )
        .into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("phase"))
                .and_then(|value| value.as_str()),
            Some("cleanup_orphan_browser_sweep")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("teardown_cleanup_degraded_under_shared_deadline")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("teardown"))
                .and_then(|value| value.get("result"))
                .and_then(|value| value.get("fully_released")),
            Some(&json!(false))
        );
    }

    #[test]
    fn teardown_compatibility_degraded_owned_error_preserves_shared_family_projection() {
        let result = TeardownResult {
            fully_released: false,
            rub_home_removed: false,
            close_all: BatchCloseResult {
                closed: vec![],
                cleaned_stale: vec![],
                compatibility_degraded_owned_sessions: vec![CompatibilityDegradedOwnedSession {
                    session: "default".to_string(),
                    daemon_session_id: "sess-default".to_string(),
                    reason: CompatibilityDegradedOwnedReason::ProtocolIncompatible,
                }],
                failed: vec![],
                session_error_details: vec![],
            },
            cleanup: CleanupResult::default(),
        };

        let envelope = teardown_compatibility_degraded_owned_error(
            PathBuf::from("/tmp/rub-home").as_path(),
            &result,
        )
        .into_envelope();
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("teardown_compatibility_degraded_owned_sessions")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("compatibility_degraded_owned_sessions"))
                .and_then(|value| value.get(0))
                .and_then(|value| value.get("reason")),
            Some(&json!("protocol_incompatible"))
        );
    }

    #[tokio::test]
    async fn teardown_runtime_does_not_create_missing_rub_home() {
        let home = std::env::temp_dir().join(format!(
            "rub-teardown-no-create-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);

        let result = super::teardown_runtime(&home, 1_000)
            .await
            .expect("teardown should succeed for a missing home");

        assert!(result.fully_released);
        assert!(!home.exists(), "teardown must not create RUB_HOME");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn teardown_runtime_removes_temp_owned_home_after_release() {
        let home = std::env::temp_dir().join(format!(
            "rub-temp-owned-teardown-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create temp-owned teardown home");
        std::fs::write(
            rub_daemon::rub_paths::RubPaths::new(&home).temp_home_owner_marker_path(),
            "",
        )
        .expect("write temp-home owner marker");

        let result = super::teardown_runtime(&home, 1_000)
            .await
            .expect("teardown should release temp-owned home");

        assert!(result.fully_released);
        assert!(result.rub_home_removed);
        assert!(!home.exists(), "temp-owned home should be removed");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn teardown_runtime_refuses_live_owner_temp_owned_home_after_revalidation() {
        let home = std::env::temp_dir().join(format!(
            "rub-temp-owned-live-owner-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create temp-owned teardown home");
        std::fs::write(
            rub_daemon::rub_paths::RubPaths::new(&home).temp_home_owner_marker_path(),
            std::process::id().to_string(),
        )
        .expect("write temp-home owner marker");

        let error = super::teardown_runtime(&home, 1_000)
            .await
            .expect_err("teardown must not remove a live-owner temp-owned home");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("temp_owned_rub_home_live_owner_revalidated")
        );
        assert!(
            home.exists(),
            "live-owner temp-owned home must be preserved"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn teardown_runtime_preserves_non_temp_owned_home_after_release() {
        let unrelated_live_temp_home = std::env::temp_dir().join(format!(
            "rub-temp-owned-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let home = std::env::temp_dir().join(format!(
            "rub-teardown-non-temp-owned-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&unrelated_live_temp_home);
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&unrelated_live_temp_home)
            .expect("create unrelated live temp-owned home");
        std::fs::write(
            rub_daemon::rub_paths::RubPaths::new(&unrelated_live_temp_home)
                .temp_home_owner_marker_path(),
            std::process::id().to_string(),
        )
        .expect("write unrelated temp-home owner marker");
        std::fs::create_dir_all(&home).expect("create non-temp-owned teardown home");

        let result = super::teardown_runtime(&home, 1_000)
            .await
            .expect("teardown should succeed for non-temp-owned home");

        assert!(result.fully_released);
        assert!(!result.rub_home_removed);
        assert!(home.exists(), "non-temp-owned home should be preserved");

        let _ = std::fs::remove_dir_all(&unrelated_live_temp_home);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn teardown_runtime_uses_one_shared_deadline_across_close_all_and_cleanup() {
        let home = std::env::temp_dir().join(format!(
            "rub-teardown-shared-deadline-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create teardown home");

        let error =
            super::teardown_runtime_until(&home, Instant::now() - Duration::from_millis(1), 1_500)
                .await
                .expect_err(
                    "expired teardown budget must remain expired across close_all and cleanup",
                );

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(|value| value.as_str()),
            Some("cleanup_orphan_browser_sweep")
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
