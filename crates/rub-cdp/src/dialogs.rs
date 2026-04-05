use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::{
    EventJavascriptDialogClosed, EventJavascriptDialogOpening, HandleJavaScriptDialogParams,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    DialogInterceptPolicy, DialogKind, DialogRuntimeInfo, DialogRuntimeStatus, PendingDialogInfo,
};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::RwLock;

use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, next_listener_event};

/// A shared, atomically-consumed one-shot dialog intercept policy.
///
/// The CDP listener task takes (consumes) this policy when a dialog opens,
/// immediately calling `Page.handleJavaScriptDialog` before Chrome's built-in
/// handler can auto-dismiss.
pub type SharedDialogIntercept = Arc<Mutex<Option<DialogInterceptPolicy>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogRuntimeUpdate {
    pub generation: ListenerGeneration,
    pub runtime: DialogRuntimeInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDialogOpening {
    pub generation: ListenerGeneration,
    pub kind: DialogKind,
    pub message: String,
    pub url: String,
    pub tab_target_id: Option<String>,
    pub frame_id: Option<String>,
    pub default_prompt: Option<String>,
    pub has_browser_handler: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDialogClosed {
    pub generation: ListenerGeneration,
    pub accepted: bool,
    pub user_input: String,
}

type RuntimeCallback = Arc<dyn Fn(DialogRuntimeUpdate) + Send + Sync>;
type OpeningCallback = Arc<dyn Fn(BrowserDialogOpening) + Send + Sync>;
type ClosedCallback = Arc<dyn Fn(BrowserDialogClosed) + Send + Sync>;
pub type SharedDialogRuntime = Arc<RwLock<DialogRuntimeInfo>>;

#[derive(Clone, Default)]
pub struct DialogCallbacks {
    pub on_runtime: Option<RuntimeCallback>,
    pub on_opened: Option<OpeningCallback>,
    pub on_closed: Option<ClosedCallback>,
}

impl DialogCallbacks {
    pub fn is_empty(&self) -> bool {
        self.on_runtime.is_none() && self.on_opened.is_none() && self.on_closed.is_none()
    }
}

pub fn new_shared_dialog_runtime() -> SharedDialogRuntime {
    Arc::new(RwLock::new(DialogRuntimeInfo::default()))
}

pub async fn pending_dialog(runtime: &SharedDialogRuntime) -> Option<PendingDialogInfo> {
    runtime.read().await.pending_dialog.clone()
}

/// Whether a pre-registered intercept policy should be consumed by the
/// listener task running on a page identified by `tab_target_id`.
///
/// Matching rules (both must hold for `true`):
/// * The policy slot is non-empty (checked by the caller before calling this).
/// * `policy.target_tab_id` is `None` (wildcard) **or** equals `tab_target_id`.
///
/// This is extracted from the hot path so it can be unit-tested without a
/// browser. The invariant is: a `Some(t)` target can only be consumed by
/// the tab whose `tab_target_id` string equals `t`.
pub(crate) fn intercept_policy_matches(
    policy: &DialogInterceptPolicy,
    tab_target_id: &str,
) -> bool {
    policy
        .target_tab_id
        .as_deref()
        .is_none_or(|t| t == tab_target_id)
}

pub async fn ensure_page_dialog_runtime(
    page: Arc<Page>,
    callbacks: DialogCallbacks,
    runtime: SharedDialogRuntime,
    intercept: SharedDialogIntercept,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
) -> Result<(), RubError> {
    let mut degraded_reason = None;
    let tab_target_id = page.target_id().as_ref().to_string();

    let opened_callback = callbacks.on_opened.clone();
    let runtime_state = runtime.clone();
    match page.event_listener::<EventJavascriptDialogOpening>().await {
        Ok(mut listener) => {
            let generation_rx = listener_generation_rx.clone();
            let intercept = intercept.clone();
            let page_for_intercept = page.clone();
            tokio::spawn(async move {
                let mut generation_rx = generation_rx;
                while let Some(event) =
                    next_listener_event(&mut listener, listener_generation, &mut generation_rx)
                        .await
                {
                    let opened = BrowserDialogOpening {
                        generation: listener_generation,
                        kind: normalize_dialog_kind(&event),
                        message: event.message.clone(),
                        url: event.url.clone(),
                        tab_target_id: Some(tab_target_id.clone()),
                        frame_id: Some(event.frame_id.as_ref().to_string()),
                        default_prompt: event.default_prompt.clone(),
                        has_browser_handler: event.has_browser_handler,
                    };
                    {
                        let mut state = runtime_state.write().await;
                        apply_dialog_runtime_status(&mut state, DialogRuntimeStatus::Active);
                        state.pending_dialog = Some(PendingDialogInfo {
                            kind: opened.kind,
                            message: opened.message.clone(),
                            url: opened.url.clone(),
                            tab_target_id: opened.tab_target_id.clone(),
                            frame_id: opened.frame_id.clone(),
                            default_prompt: opened.default_prompt.clone(),
                            has_browser_handler: opened.has_browser_handler,
                            opened_at: rfc3339_now(),
                        });
                        state.last_dialog = state.pending_dialog.clone();
                    }

                    // ── Dialog Intercept (one-shot, page-scoped) ─────────────
                    // Consume the pre-registered intercept policy and call
                    // Page.handleJavaScriptDialog *directly* (no extra spawn) so
                    // the CDP command races in the same task slice as the opening
                    // event — before Chrome's built-in handler can auto-dismiss.
                    //
                    // Policy matching rules:
                    //   target_tab_id = None   → wildcard, consumed by any tab
                    //   target_tab_id = Some(t) → consumed only when t == this tab
                    //
                    // The MutexGuard is scoped to the inner block below so it is
                    // guaranteed dropped before the .await (std::sync::MutexGuard
                    // is !Send and must not be held across an await point).
                    let intercept_policy = {
                        if let Ok(mut guard) = intercept.lock() {
                            let should_consume = guard
                                .as_ref()
                                .is_some_and(|p| intercept_policy_matches(p, &tab_target_id));
                            if should_consume { guard.take() } else { None }
                        } else {
                            None
                        }
                    }; // guard dropped here — safe to await below

                    if let Some(policy) = intercept_policy {
                        // Direct await — stays in the same listener task,
                        // eliminating the second-spawn timing gap.
                        let _ =
                            handle_dialog(&page_for_intercept, policy.accept, policy.prompt_text)
                                .await;
                    }

                    if let Some(callback) = &opened_callback {
                        callback(opened);
                    }
                }
            });
        }
        Err(error) => {
            degraded_reason = Some(format!("dialog_open_listener_failed:{error}"));
        }
    }

    let closed_callback = callbacks.on_closed.clone();
    let runtime_state = runtime.clone();
    match page.event_listener::<EventJavascriptDialogClosed>().await {
        Ok(mut listener) => {
            let generation_rx = listener_generation_rx.clone();
            tokio::spawn(async move {
                let mut generation_rx = generation_rx;
                while let Some(event) =
                    next_listener_event(&mut listener, listener_generation, &mut generation_rx)
                        .await
                {
                    {
                        let mut state = runtime_state.write().await;
                        let prompt_input = state
                            .last_dialog
                            .as_ref()
                            .filter(|dialog| matches!(dialog.kind, DialogKind::Prompt))
                            .map(|_| event.user_input.clone());
                        apply_dialog_runtime_status(&mut state, DialogRuntimeStatus::Inactive);
                        state.pending_dialog = None;
                        state.last_result = Some(rub_core::model::DialogResolutionInfo {
                            accepted: event.result,
                            user_input: prompt_input,
                            closed_at: rfc3339_now(),
                        });
                    }
                    if let Some(callback) = &closed_callback {
                        callback(BrowserDialogClosed {
                            generation: listener_generation,
                            accepted: event.result,
                            user_input: event.user_input.clone(),
                        });
                    }
                }
            });
        }
        Err(error) => {
            degraded_reason.get_or_insert_with(|| format!("dialog_closed_listener_failed:{error}"));
        }
    }

    let projection = commit_dialog_hook_install_projection(&runtime, degraded_reason.clone()).await;
    if let Some(callback) = callbacks.on_runtime {
        callback(DialogRuntimeUpdate {
            generation: listener_generation,
            runtime: projection,
        });
    }

    if let Some(reason) = degraded_reason {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            format!("Dialog hook install degraded before commit: {reason}"),
        ));
    }

    Ok(())
}

fn apply_dialog_runtime_status(state: &mut DialogRuntimeInfo, desired: DialogRuntimeStatus) {
    if state.degraded_reason.is_some() {
        state.status = DialogRuntimeStatus::Degraded;
    } else {
        state.status = desired;
    }
}

async fn commit_dialog_hook_install_projection(
    runtime: &SharedDialogRuntime,
    degraded_reason: Option<String>,
) -> DialogRuntimeInfo {
    let mut state = runtime.write().await;
    if let Some(reason) = degraded_reason {
        state.status = DialogRuntimeStatus::Degraded;
        state.degraded_reason = Some(reason);
        return state.clone();
    }

    // Dialog runtime is session-scoped, but page hooks install per-tab. A
    // background tab that already owns a pending dialog remains the
    // authoritative page for actuation, so a later hook reinstall on an
    // unrelated page must not republish shared runtime as `Inactive`.
    if state.pending_dialog.is_none() {
        apply_dialog_runtime_status(&mut state, DialogRuntimeStatus::Inactive);
        if !matches!(state.status, DialogRuntimeStatus::Degraded) {
            state.degraded_reason = None;
        }
    }
    state.clone()
}

fn rfc3339_now() -> String {
    // Rfc3339 formatting of OffsetDateTime::now_utc() is infallible in
    // practice (the time crate's well-known format never fails for utc).
    // unwrap_or_else is kept as a belt-and-suspenders guard, but the
    // fallback is a clearly-invalid sentinel rather than the Unix epoch:
    // epoch is a valid timestamp and would silently corrupt time-series
    // data (e.g. dialog opened_at / closed_at fields) if ever triggered.
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "TIMESTAMP_FORMAT_ERROR".to_string())
}

pub async fn handle_dialog(
    page: &Arc<Page>,
    accept: bool,
    prompt_text: Option<String>,
) -> Result<(), RubError> {
    let mut builder = HandleJavaScriptDialogParams::builder().accept(accept);
    if let Some(text) = prompt_text {
        builder = builder.prompt_text(text);
    }
    let params = builder.build().map_err(RubError::Internal)?;
    page.execute(params).await.map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to handle JavaScript dialog: {error}"),
        )
    })?;
    Ok(())
}

fn normalize_dialog_kind(event: &EventJavascriptDialogOpening) -> DialogKind {
    match event.r#type.as_ref() {
        "alert" => DialogKind::Alert,
        "confirm" => DialogKind::Confirm,
        "prompt" => DialogKind::Prompt,
        "beforeunload" => DialogKind::Beforeunload,
        _ => DialogKind::Alert,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DialogCallbacks, apply_dialog_runtime_status, commit_dialog_hook_install_projection,
        new_shared_dialog_runtime,
    };
    use rub_core::model::{DialogKind, DialogRuntimeInfo, DialogRuntimeStatus};

    #[test]
    fn empty_callbacks_report_empty() {
        assert!(DialogCallbacks::default().is_empty());
    }

    #[test]
    fn dialog_kind_serializes_as_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&DialogKind::Beforeunload).expect("serialize dialog kind"),
            "\"beforeunload\""
        );
    }

    #[tokio::test]
    async fn shared_runtime_starts_inactive() {
        let runtime = new_shared_dialog_runtime();
        let runtime = runtime.read().await;
        assert_eq!(runtime.status, DialogRuntimeStatus::Inactive);
        assert!(runtime.pending_dialog.is_none());
    }

    #[test]
    fn degraded_reason_keeps_runtime_degraded_across_state_changes() {
        let mut runtime = DialogRuntimeInfo {
            status: DialogRuntimeStatus::Degraded,
            degraded_reason: Some("listener_failed".to_string()),
            ..DialogRuntimeInfo::default()
        };

        apply_dialog_runtime_status(&mut runtime, DialogRuntimeStatus::Active);
        assert_eq!(runtime.status, DialogRuntimeStatus::Degraded);

        runtime.degraded_reason = None;
        apply_dialog_runtime_status(&mut runtime, DialogRuntimeStatus::Inactive);
        assert_eq!(runtime.status, DialogRuntimeStatus::Inactive);
    }

    #[tokio::test]
    async fn hook_install_does_not_clear_foreign_pending_dialog() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(rub_core::model::PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "Background dialog".to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-01-01T00:00:00Z".to_string(),
            });
            state.last_dialog = state.pending_dialog.clone();
        }

        let projection = commit_dialog_hook_install_projection(&runtime, None).await;

        assert_eq!(projection.status, DialogRuntimeStatus::Active);
        assert_eq!(
            projection
                .pending_dialog
                .as_ref()
                .and_then(|dialog| dialog.tab_target_id.as_deref()),
            Some("target-1")
        );
    }
}

#[cfg(test)]
mod intercept_policy_tests {
    use super::intercept_policy_matches;
    use rub_core::model::DialogInterceptPolicy;

    fn policy(accept: bool, target_tab_id: Option<&str>) -> DialogInterceptPolicy {
        DialogInterceptPolicy {
            accept,
            prompt_text: None,
            target_tab_id: target_tab_id.map(str::to_string),
        }
    }

    /// INV: a page-scoped policy (Some target) is consumed ONLY by the matching tab.
    #[test]
    fn page_scoped_policy_matches_only_its_own_tab() {
        let p = policy(true, Some("tab-A"));
        assert!(
            intercept_policy_matches(&p, "tab-A"),
            "policy must match its own target tab"
        );
        assert!(
            !intercept_policy_matches(&p, "tab-B"),
            "policy must NOT match a different tab — would violate page-scoped authority"
        );
    }

    /// INV: a wildcard policy (None target) is consumed by any tab.
    /// This mode is only valid in single-tab sessions.
    #[test]
    fn wildcard_policy_matches_any_tab() {
        let p = policy(true, None);
        assert!(
            intercept_policy_matches(&p, "tab-A"),
            "wildcard policy must match tab-A"
        );
        assert!(
            intercept_policy_matches(&p, "tab-B"),
            "wildcard policy must match tab-B"
        );
    }

    /// INV: SharedDialogIntercept is one-shot — after the first take() the slot
    /// is empty and a second dialog on the same tab must NOT re-consume.
    #[test]
    fn one_shot_semantics_slot_is_empty_after_take() {
        use super::SharedDialogIntercept;
        use std::sync::{Arc, Mutex};

        let intercept: SharedDialogIntercept =
            Arc::new(Mutex::new(Some(policy(true, Some("tab-A")))));

        // First take — should succeed
        let taken = {
            let mut guard = intercept.lock().unwrap();
            if guard
                .as_ref()
                .is_some_and(|p| intercept_policy_matches(p, "tab-A"))
            {
                guard.take()
            } else {
                None
            }
        };
        assert!(taken.is_some(), "first take must succeed");

        // Second take on the same tab — slot is empty, must return None
        let taken_again = {
            let mut guard = intercept.lock().unwrap();
            if guard
                .as_ref()
                .is_some_and(|p| intercept_policy_matches(p, "tab-A"))
            {
                guard.take()
            } else {
                None
            }
        };
        assert!(
            taken_again.is_none(),
            "one-shot: second take must be None — policy must not repeat"
        );
    }
}
