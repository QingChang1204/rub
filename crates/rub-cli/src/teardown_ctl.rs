//! Canonical lifecycle exit wrapper for one RUB_HOME.

use crate::{cleanup_ctl, daemon_ctl};
use rub_core::error::{ErrorCode, RubError};
use rub_daemon::rub_paths::is_temp_owned_home;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::Path;

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
    let temp_owned_home = rub_home.exists() && is_temp_owned_home(rub_home);
    let close_all = daemon_ctl::close_all_sessions(rub_home, timeout_ms).await?;
    let cleanup = if rub_home.exists() {
        cleanup_ctl::cleanup_runtime(rub_home, timeout_ms)
            .await
            .map_err(|error| augment_cleanup_error(rub_home, &close_all, error))?
    } else {
        cleanup_ctl::CleanupResult::default()
    };
    let rub_home_removed = if close_all.failed.is_empty() && temp_owned_home {
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
            && (!temp_owned_home || rub_home_removed || !rub_home.exists()),
        rub_home_removed,
        close_all,
        cleanup,
    };

    if result.fully_released {
        Ok(result)
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
    RubError::domain_with_context_and_suggestion(
        ErrorCode::SessionBusy,
        "Teardown could not confirm shutdown for one or more sessions",
        json!({
            "teardown": project_teardown_result(rub_home, result),
            "failed_sessions": result.close_all.failed,
        }),
        "Retry 'rub teardown' after the remaining session finishes shutting down, or inspect the failed session with 'rub sessions' and 'rub doctor'",
    )
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
    use super::{TeardownResult, project_teardown_result, teardown_incomplete_error};
    use crate::{cleanup_ctl::CleanupResult, daemon_ctl::BatchCloseResult};
    use rub_core::error::ErrorCode;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn project_teardown_result_combines_close_and_cleanup_surfaces() {
        let result = TeardownResult {
            fully_released: true,
            rub_home_removed: false,
            close_all: BatchCloseResult {
                closed: vec!["default".to_string()],
                cleaned_stale: vec![],
                failed: vec![],
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
                failed: vec!["default".to_string()],
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
                .and_then(|ctx| ctx.get("teardown"))
                .and_then(|value| value.get("result"))
                .and_then(|value| value.get("fully_released")),
            Some(&json!(false))
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
    async fn teardown_runtime_removes_temp_owned_home_after_release() {
        let home = std::env::temp_dir().join(format!(
            "rub-teardown-temp-owned-{}-{}",
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

        let result = super::teardown_runtime(&home, 1_000)
            .await
            .expect("teardown should release temp-owned home");

        assert!(result.fully_released);
        assert!(result.rub_home_removed);
        assert!(!home.exists(), "temp-owned home should be removed");
    }

    #[tokio::test]
    async fn teardown_runtime_preserves_non_temp_owned_home_after_release() {
        let home = std::env::temp_dir().join(format!(
            "rub-teardown-non-temp-owned-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create non-temp-owned teardown home");

        let result = super::teardown_runtime(&home, 1_000)
            .await
            .expect("teardown should succeed for non-temp-owned home");

        assert!(result.fully_released);
        assert!(!result.rub_home_removed);
        assert!(home.exists(), "non-temp-owned home should be preserved");

        let _ = std::fs::remove_dir_all(&home);
    }
}
