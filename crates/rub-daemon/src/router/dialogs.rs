use std::sync::Arc;
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::DialogRuntimeInfo;

use crate::runtime_refresh::refresh_live_dialog_runtime;
use crate::session::SessionState;

use super::DaemonRouter;

pub(super) async fn cmd_dialog(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match args
        .get("sub")
        .and_then(|value| value.as_str())
        .unwrap_or("status")
    {
        "status" => {
            refresh_live_dialog_runtime(&router.browser, state).await;
            let runtime = state.dialog_runtime().await;
            Ok(dialog_runtime_payload(
                serde_json::json!({
                    "kind": "dialog_runtime",
                    "action": "status",
                }),
                runtime,
            ))
        }
        "accept" => {
            let prompt_text = args
                .get("prompt_text")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            handle_pending_dialog(router, state, true, prompt_text).await
        }
        "dismiss" => handle_pending_dialog(router, state, false, None).await,
        "intercept" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("accept");
            let accept = !matches!(action, "dismiss");
            let prompt_text = args
                .get("prompt_text")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let tab_list = router.browser.list_tabs().await;
            let target_tab_id = resolve_intercept_target_tab_id(tab_list)?;

            let policy = rub_core::model::DialogInterceptPolicy {
                accept,
                prompt_text,
                target_tab_id: Some(target_tab_id.clone()),
            };
            router.browser.set_dialog_intercept(policy.clone());
            Ok(serde_json::json!({
                "subject": {
                    "kind": "dialog_intercept",
                    "action": "armed",
                },
                "intercept": {
                    "action": if accept { "accept" } else { "dismiss" },
                    "prompt_text": policy.prompt_text,
                    "target_tab_id": target_tab_id,
                }
            }))
        }

        "cancel_intercept" => {
            router.browser.clear_dialog_intercept();
            Ok(serde_json::json!({
                "subject": {
                    "kind": "dialog_intercept",
                    "action": "cancelled",
                }
            }))
        }
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown dialog subcommand: '{other}'"),
        )),
    }
}

async fn handle_pending_dialog(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    accept: bool,
    prompt_text: Option<String>,
) -> Result<serde_json::Value, RubError> {
    refresh_live_dialog_runtime(&router.browser, state).await;
    let current = state.dialog_runtime().await;
    if current.pending_dialog.is_none() {
        return Err(generic_no_pending_dialog_error());
    }

    if let Err(error) = router.browser.handle_dialog(accept, prompt_text).await {
        refresh_live_dialog_runtime(&router.browser, state).await;
        let runtime = state.dialog_runtime().await;
        if runtime.pending_dialog.is_none() {
            return Err(browser_handled_dialog_error(&runtime, accept));
        }
        return Err(error);
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        refresh_live_dialog_runtime(&router.browser, state).await;
        let runtime = state.dialog_runtime().await;
        if runtime.pending_dialog.is_none() {
            return Ok(dialog_action_payload(accept, runtime));
        }
        if Instant::now() >= deadline {
            return Err(RubError::domain_with_context(
                ErrorCode::WaitTimeout,
                "JavaScript dialog action did not observe a closed event before timeout",
                serde_json::json!({
                    "kind": "dialog",
                    "action": if accept { "accept" } else { "dismiss" },
                }),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn dialog_runtime_payload(
    subject: serde_json::Value,
    runtime: DialogRuntimeInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "runtime": runtime,
    })
}

fn dialog_action_payload(accept: bool, runtime: DialogRuntimeInfo) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "dialog_action",
            "action": if accept { "accept" } else { "dismiss" },
        },
        "result": {
            "pending_dialog": runtime.pending_dialog.clone(),
            "last_result": runtime.last_result.clone(),
        },
        "runtime": runtime,
    })
}

/// Resolve the authoritative target tab ID for a dialog intercept arm request.
///
/// # Fail-closed rule
///
/// If `list_tabs_result` is an error, or if the tab list contains no active
/// tab, we return `Err` rather than `None` / falling back to a wildcard `None`
/// policy. A wildcard would allow any tab's dialog to consume the intercept,
/// violating the page-scoped authority guarantee introduced to prevent
/// multi-tab cross-contamination.
///
/// Extracted as a pure function so the fail-closed contract can be verified
/// by unit tests without constructing a `DaemonRouter` or any `BrowserPort`.
pub(crate) fn resolve_intercept_target_tab_id(
    list_tabs_result: Result<Vec<rub_core::model::TabInfo>, RubError>,
) -> Result<String, RubError> {
    list_tabs_result
        .ok()
        .and_then(|tabs| tabs.into_iter().find(|t| t.active).map(|t| t.target_id))
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Cannot arm dialog intercept: no active tab is available. \
                 Navigate to a page before calling 'dialog intercept'.",
            )
        })
}

fn generic_no_pending_dialog_error() -> RubError {
    RubError::domain(
        ErrorCode::InvalidInput,
        "No pending JavaScript dialog is available for this session",
    )
}

fn browser_handled_dialog_error(runtime: &DialogRuntimeInfo, accept: bool) -> RubError {
    if let (Some(last_dialog), Some(last_result)) = (&runtime.last_dialog, &runtime.last_result)
        && last_dialog.has_browser_handler
    {
        let action = if accept { "accept" } else { "dismiss" };
        return RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "JavaScript dialog was already closed before dialog {action} reached the browser authority"
            ),
            serde_json::json!({
                "dialog_runtime": runtime,
                "requested_action": action,
                "last_dialog_kind": last_dialog.kind,
                "last_result": last_result,
            }),
            "Trigger the dialog and handle it immediately. When 'has_browser_handler' is true, inspect 'last_result' to see how the browser resolved it before your command arrived.",
        );
    }

    generic_no_pending_dialog_error()
}

#[cfg(test)]
mod tests {
    use super::browser_handled_dialog_error;
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        DialogKind, DialogResolutionInfo, DialogRuntimeInfo, DialogRuntimeStatus, PendingDialogInfo,
    };

    #[test]
    fn browser_handled_dialog_error_is_explainable() {
        let runtime = DialogRuntimeInfo {
            status: DialogRuntimeStatus::Active,
            pending_dialog: None,
            last_dialog: Some(PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "Hello".to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-01-01T00:00:00Z".to_string(),
            }),
            last_result: Some(DialogResolutionInfo {
                accepted: true,
                user_input: None,
                closed_at: "2026-01-01T00:00:01Z".to_string(),
            }),
            degraded_reason: None,
        };

        let envelope = browser_handled_dialog_error(&runtime, true).into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("already closed"));
        assert_eq!(
            envelope.context.unwrap()["requested_action"],
            serde_json::json!("accept")
        );
    }

    // ── Dialog intercept: fail-closed contract tests ──────────────────────────

    use super::resolve_intercept_target_tab_id;
    use rub_core::model::TabInfo;

    fn active_tab(target_id: &str) -> TabInfo {
        TabInfo {
            index: 0,
            target_id: target_id.to_string(),
            url: "https://example.com".to_string(),
            title: "Test".to_string(),
            active: true,
        }
    }

    fn inactive_tab(target_id: &str) -> TabInfo {
        TabInfo {
            index: 1,
            target_id: target_id.to_string(),
            url: "https://other.com".to_string(),
            title: "Other".to_string(),
            active: false,
        }
    }

    /// INV (fail-closed): list_tabs() error → resolving MUST fail.
    /// A BrowserPort error must never silently degrade to a wildcard intercept.
    #[test]
    fn intercept_arm_fails_when_list_tabs_returns_error() {
        let err = rub_core::error::RubError::domain(ErrorCode::InvalidInput, "browser not running");
        let result = resolve_intercept_target_tab_id(Err(err));
        assert!(
            result.is_err(),
            "must be Err when list_tabs fails — fail-closed"
        );
        assert!(
            result
                .unwrap_err()
                .into_envelope()
                .message
                .contains("no active tab"),
            "error message must explain what to do"
        );
    }

    /// INV (fail-closed): tabs exist but none is active → resolving MUST fail.
    /// Background tabs must not receive an intercept intended for the active tab.
    #[test]
    fn intercept_arm_fails_when_no_active_tab() {
        let tabs = vec![inactive_tab("tab-background")];
        let result = resolve_intercept_target_tab_id(Ok(tabs));
        assert!(
            result.is_err(),
            "must be Err when no tab is active — fail-closed"
        );
    }

    /// Happy path: exactly one active tab → returns its target_id.
    #[test]
    fn intercept_arm_succeeds_with_active_tab() {
        let tabs = vec![inactive_tab("tab-bg"), active_tab("tab-fg")];
        let result = resolve_intercept_target_tab_id(Ok(tabs));
        assert_eq!(result.unwrap(), "tab-fg");
    }
}
