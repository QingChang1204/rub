use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::{
    EventJavascriptDialogClosed, EventJavascriptDialogOpening, HandleJavaScriptDialogParams,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    DialogInterceptPolicy, DialogKind, DialogRuntimeInfo, DialogRuntimeStatus, PendingDialogInfo,
};
use std::collections::HashSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
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
pub(crate) type DialogListenerEndedCallback = Arc<dyn Fn(&'static str) + Send + Sync>;
pub type SharedDialogRuntime = Arc<RwLock<DialogRuntimeInfo>>;

#[derive(Clone, Default)]
pub struct DialogCallbacks {
    pub on_runtime: Option<RuntimeCallback>,
    pub on_opened: Option<OpeningCallback>,
    pub on_closed: Option<ClosedCallback>,
    pub on_listener_ended: Option<DialogListenerEndedCallback>,
}

impl DialogCallbacks {
    pub fn is_empty(&self) -> bool {
        self.on_runtime.is_none()
            && self.on_opened.is_none()
            && self.on_closed.is_none()
            && self.on_listener_ended.is_none()
    }
}

pub fn new_shared_dialog_runtime() -> SharedDialogRuntime {
    Arc::new(RwLock::new(DialogRuntimeInfo::default()))
}

pub async fn pending_dialog(runtime: &SharedDialogRuntime) -> Option<PendingDialogInfo> {
    runtime.read().await.pending_dialog.clone()
}

pub(crate) fn pending_dialog_matches_target(dialog: &PendingDialogInfo, target_id: &str) -> bool {
    dialog.tab_target_id.as_deref() == Some(target_id)
}

pub async fn pending_dialog_for_target(
    runtime: &SharedDialogRuntime,
    target_id: &str,
) -> Option<PendingDialogInfo> {
    let dialog = pending_dialog(runtime).await?;
    pending_dialog_matches_target(&dialog, target_id).then_some(dialog)
}

pub async fn clear_stale_pending_dialog_for_live_targets(
    runtime: &SharedDialogRuntime,
    live_target_ids: &HashSet<String>,
) -> Option<DialogRuntimeInfo> {
    clear_stale_pending_dialog_for_live_targets_if(runtime, live_target_ids, || true).await
}

pub(crate) async fn clear_stale_pending_dialog_for_live_targets_if<F>(
    runtime: &SharedDialogRuntime,
    live_target_ids: &HashSet<String>,
    should_commit: F,
) -> Option<DialogRuntimeInfo>
where
    F: Fn() -> bool,
{
    let mut state = runtime.write().await;
    let target_id = state
        .pending_dialog
        .as_ref()
        .and_then(|dialog| dialog.tab_target_id.clone())?;
    if live_target_ids.contains(&target_id) {
        return None;
    }
    if !should_commit() {
        return None;
    }

    state.pending_dialog = None;
    state.degraded_reason = Some("pending_dialog_target_lost".to_string());
    apply_dialog_runtime_status(&mut state, DialogRuntimeStatus::Degraded);
    Some(state.clone())
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

fn take_matching_intercept_policy(
    intercept: &SharedDialogIntercept,
    tab_target_id: &str,
) -> Option<DialogInterceptPolicy> {
    match intercept.lock() {
        Ok(mut guard) => {
            let should_consume = guard
                .as_ref()
                .is_some_and(|p| intercept_policy_matches(p, tab_target_id));
            if should_consume { guard.take() } else { None }
        }
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            let should_consume = guard
                .as_ref()
                .is_some_and(|p| intercept_policy_matches(p, tab_target_id));
            let taken = if should_consume { guard.take() } else { None };
            drop(guard);
            intercept.clear_poison();
            tracing::warn!(
                "Recovered poisoned dialog intercept state while consuming opening event"
            );
            taken
        }
    }
}

async fn publish_dialog_opening(
    runtime_state: &SharedDialogRuntime,
    opened_callback: Option<&OpeningCallback>,
    opened: &BrowserDialogOpening,
    authority_release_in_progress: &AtomicBool,
) -> bool {
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }

    {
        let mut state = runtime_state.write().await;
        if authority_release_in_progress.load(Ordering::SeqCst) {
            return false;
        }
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

    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }

    if let Some(callback) = opened_callback {
        callback(opened.clone());
    }
    true
}

async fn publish_dialog_closed(
    runtime_state: &SharedDialogRuntime,
    closed_callback: Option<&ClosedCallback>,
    listener_generation: ListenerGeneration,
    accepted: bool,
    user_input: String,
    authority_release_in_progress: &AtomicBool,
) -> bool {
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }

    {
        let mut state = runtime_state.write().await;
        if authority_release_in_progress.load(Ordering::SeqCst) {
            return false;
        }
        let prompt_input = state
            .last_dialog
            .as_ref()
            .filter(|dialog| matches!(dialog.kind, DialogKind::Prompt))
            .map(|_| user_input.clone());
        apply_dialog_runtime_status(&mut state, DialogRuntimeStatus::Inactive);
        state.pending_dialog = None;
        state.last_result = Some(rub_core::model::DialogResolutionInfo {
            accepted,
            user_input: prompt_input,
            closed_at: rfc3339_now(),
        });
    }

    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }
    if let Some(callback) = closed_callback {
        callback(BrowserDialogClosed {
            generation: listener_generation,
            accepted,
            user_input,
        });
    }
    true
}

async fn publish_dialog_intercept_failure(
    runtime_state: &SharedDialogRuntime,
    runtime_callback: Option<&RuntimeCallback>,
    listener_generation: ListenerGeneration,
    error: &RubError,
    authority_release_in_progress: &AtomicBool,
) -> bool {
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }

    let projection = {
        let mut state = runtime_state.write().await;
        if authority_release_in_progress.load(Ordering::SeqCst) {
            return false;
        }
        state.degraded_reason = Some(format!("dialog_intercept_handle_failed:{error}"));
        apply_dialog_runtime_status(&mut state, DialogRuntimeStatus::Degraded);
        state.clone()
    };

    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }
    if let Some(callback) = runtime_callback {
        callback(DialogRuntimeUpdate {
            generation: listener_generation,
            runtime: projection,
        });
    }
    true
}

pub async fn ensure_page_dialog_runtime(
    page: Arc<Page>,
    callbacks: DialogCallbacks,
    runtime: SharedDialogRuntime,
    intercept: SharedDialogIntercept,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
    authority_release_in_progress: Arc<AtomicBool>,
) -> Result<(), RubError> {
    let mut degraded_reason = None;
    let tab_target_id = page.target_id().as_ref().to_string();

    let opening_listener = match page.event_listener::<EventJavascriptDialogOpening>().await {
        Ok(listener) => Some(listener),
        Err(error) => {
            degraded_reason = Some(format!("dialog_open_listener_failed:{error}"));
            None
        }
    };

    let closed_listener = match page.event_listener::<EventJavascriptDialogClosed>().await {
        Ok(listener) => Some(listener),
        Err(error) => {
            degraded_reason.get_or_insert_with(|| format!("dialog_closed_listener_failed:{error}"));
            None
        }
    };

    let projection = commit_dialog_hook_install_projection(&runtime, degraded_reason.clone()).await;
    if let Some(callback) = callbacks.on_runtime.as_ref() {
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

    // All fallible/awaiting install work is complete before listener tasks are
    // spawned. This keeps a cancelled page-hook install from leaking live
    // listeners for a hook state that was never committed by the caller.
    if let Some(mut listener) = opening_listener {
        let opened_callback = callbacks.on_opened.clone();
        let runtime_callback = callbacks.on_runtime.clone();
        let runtime_state = runtime.clone();
        let generation_rx = listener_generation_rx.clone();
        let intercept = intercept.clone();
        let page_for_intercept = page.clone();
        let opening_authority_release = authority_release_in_progress.clone();
        let listener_ended = callbacks.on_listener_ended.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                if opening_authority_release.load(Ordering::SeqCst) {
                    continue;
                }
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

                if !publish_dialog_opening(
                    &runtime_state,
                    opened_callback.as_ref(),
                    &opened,
                    &opening_authority_release,
                )
                .await
                {
                    continue;
                }

                let intercept_policy = take_matching_intercept_policy(&intercept, &tab_target_id);
                if let Some(policy) = intercept_policy
                    && let Err(error) =
                        handle_dialog(&page_for_intercept, policy.accept, policy.prompt_text).await
                {
                    tracing::warn!(
                        generation = listener_generation,
                        tab_target_id = %tab_target_id,
                        error = %error,
                        "Dialog intercept actuation failed after policy consumption"
                    );
                    publish_dialog_intercept_failure(
                        &runtime_state,
                        runtime_callback.as_ref(),
                        listener_generation,
                        &error,
                        &opening_authority_release,
                    )
                    .await;
                }
            }
            if let Some(callback) = listener_ended {
                callback("dialog.opening");
            }
        });
    }

    if let Some(mut listener) = closed_listener {
        let closed_callback = callbacks.on_closed.clone();
        let runtime_state = runtime.clone();
        let generation_rx = listener_generation_rx.clone();
        let closed_authority_release = authority_release_in_progress.clone();
        let listener_ended = callbacks.on_listener_ended.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                publish_dialog_closed(
                    &runtime_state,
                    closed_callback.as_ref(),
                    listener_generation,
                    event.result,
                    event.user_input.clone(),
                    &closed_authority_release,
                )
                .await;
            }
            if let Some(callback) = listener_ended {
                callback("dialog.closed");
            }
        });
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
        BrowserDialogOpening, ClosedCallback, DialogCallbacks, DialogRuntimeInfo,
        DialogRuntimeStatus, OpeningCallback, RuntimeCallback, apply_dialog_runtime_status,
        clear_stale_pending_dialog_for_live_targets,
        clear_stale_pending_dialog_for_live_targets_if, commit_dialog_hook_install_projection,
        new_shared_dialog_runtime, pending_dialog_for_target, pending_dialog_matches_target,
        publish_dialog_closed, publish_dialog_intercept_failure, publish_dialog_opening,
    };
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::{DialogKind, PendingDialogInfo};
    use std::collections::HashSet;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

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

    #[tokio::test]
    async fn target_scoped_pending_dialog_requires_matching_tab_authority() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
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
        }

        let matching = pending_dialog_for_target(&runtime, "target-1")
            .await
            .expect("matching target should retain dialog authority");
        assert!(pending_dialog_matches_target(&matching, "target-1"));
        assert!(
            pending_dialog_for_target(&runtime, "target-2")
                .await
                .is_none(),
            "foreign target must not consume session-scoped pending dialog authority"
        );
    }

    #[tokio::test]
    async fn stale_pending_dialog_target_is_cleared_and_degraded_when_tab_authority_is_gone() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(rub_core::model::PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "Detached dialog".to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-01-01T00:00:00Z".to_string(),
            });
            state.last_dialog = state.pending_dialog.clone();
        }

        let live_target_ids = HashSet::from(["target-2".to_string()]);
        let projection = clear_stale_pending_dialog_for_live_targets(&runtime, &live_target_ids)
            .await
            .expect("lost tab authority must degrade stale pending dialog truth");

        assert_eq!(projection.status, DialogRuntimeStatus::Degraded);
        assert!(projection.pending_dialog.is_none());
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("pending_dialog_target_lost")
        );
        assert_eq!(
            projection
                .last_dialog
                .as_ref()
                .and_then(|dialog| dialog.tab_target_id.as_deref()),
            Some("target-1")
        );
    }

    #[tokio::test]
    async fn stale_pending_dialog_cleanup_respects_commit_fence_before_mutation() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(rub_core::model::PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "Detached dialog".to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-01-01T00:00:00Z".to_string(),
            });
            state.last_dialog = state.pending_dialog.clone();
        }

        let live_target_ids = HashSet::from(["target-2".to_string()]);
        assert!(
            clear_stale_pending_dialog_for_live_targets_if(&runtime, &live_target_ids, || false)
                .await
                .is_none(),
            "stale projection must not mutate dialog authority after its commit fence closes"
        );
        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Active);
        assert_eq!(
            state
                .pending_dialog
                .as_ref()
                .and_then(|dialog| dialog.tab_target_id.as_deref()),
            Some("target-1")
        );
    }

    #[tokio::test]
    async fn live_target_keeps_pending_dialog_authority() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(rub_core::model::PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "Still live".to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-01-01T00:00:00Z".to_string(),
            });
        }

        let live_target_ids = HashSet::from(["target-1".to_string()]);
        assert!(
            clear_stale_pending_dialog_for_live_targets(&runtime, &live_target_ids)
                .await
                .is_none(),
            "matching live target must preserve pending dialog authority"
        );
        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Active);
        assert_eq!(
            state
                .pending_dialog
                .as_ref()
                .and_then(|dialog| dialog.tab_target_id.as_deref()),
            Some("target-1")
        );
    }

    #[tokio::test]
    async fn publish_dialog_opening_commits_runtime_before_callback() {
        let runtime = new_shared_dialog_runtime();
        let observed_message = Arc::new(Mutex::new(None::<String>));
        let runtime_for_callback = runtime.clone();
        let observed_message_for_callback = observed_message.clone();
        let callback: OpeningCallback = Arc::new(move |opened| {
            let state = runtime_for_callback
                .try_read()
                .expect("opening callback should observe committed runtime state");
            let pending = state
                .pending_dialog
                .as_ref()
                .map(|dialog| dialog.message.clone());
            drop(state);
            assert_eq!(pending.as_deref(), Some(opened.message.as_str()));
            *observed_message_for_callback
                .lock()
                .expect("callback mutex") = Some(opened.message.clone());
        });
        let opened = BrowserDialogOpening {
            generation: 1,
            kind: DialogKind::Alert,
            message: "Hello from callback".to_string(),
            url: "https://example.test/dialog".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            default_prompt: None,
            has_browser_handler: true,
        };
        let release_in_progress = AtomicBool::new(false);

        assert!(
            publish_dialog_opening(&runtime, Some(&callback), &opened, &release_in_progress).await
        );

        assert_eq!(
            observed_message.lock().expect("callback mutex").as_deref(),
            Some("Hello from callback")
        );
        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Active);
        assert_eq!(
            state
                .pending_dialog
                .as_ref()
                .map(|dialog| dialog.message.as_str()),
            Some("Hello from callback")
        );
    }

    #[tokio::test]
    async fn publish_dialog_opening_rechecks_release_fence_after_waiting_for_write_authority() {
        let runtime = new_shared_dialog_runtime();
        let write_guard = runtime.write().await;
        let release_in_progress = Arc::new(AtomicBool::new(false));
        let callback_called = Arc::new(AtomicBool::new(false));
        let callback_called_for_callback = callback_called.clone();
        let callback: OpeningCallback = Arc::new(move |_| {
            callback_called_for_callback.store(true, Ordering::SeqCst);
        });
        let opened = BrowserDialogOpening {
            generation: 7,
            kind: DialogKind::Alert,
            message: "old authority dialog".to_string(),
            url: "https://example.test/dialog".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            default_prompt: None,
            has_browser_handler: true,
        };
        let runtime_for_task = runtime.clone();
        let release_for_task = release_in_progress.clone();
        let callback_for_task = callback.clone();
        let publish_task = tokio::spawn(async move {
            publish_dialog_opening(
                &runtime_for_task,
                Some(&callback_for_task),
                &opened,
                &release_for_task,
            )
            .await
        });

        tokio::task::yield_now().await;
        release_in_progress.store(true, Ordering::SeqCst);
        drop(write_guard);

        assert!(
            !publish_task.await.expect("publish task should finish"),
            "release fence must reject opening after waiting for write authority"
        );
        assert!(
            !callback_called.load(Ordering::SeqCst),
            "release fence must suppress opening callback"
        );
        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Inactive);
        assert!(state.pending_dialog.is_none());
        assert!(state.last_dialog.is_none());
    }

    #[tokio::test]
    async fn publish_dialog_closed_is_suppressed_while_authority_release_is_in_progress() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "old authority dialog".to_string(),
                url: "https://example.test/dialog".to_string(),
                tab_target_id: Some("tab-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-04-24T00:00:00Z".to_string(),
            });
            state.last_dialog = state.pending_dialog.clone();
        }
        let callback_called = Arc::new(AtomicBool::new(false));
        let callback_called_for_callback = callback_called.clone();
        let callback: ClosedCallback = Arc::new(move |_| {
            callback_called_for_callback.store(true, Ordering::SeqCst);
        });
        let release_in_progress = AtomicBool::new(true);

        let published = publish_dialog_closed(
            &runtime,
            Some(&callback),
            7,
            true,
            "ignored".to_string(),
            &release_in_progress,
        )
        .await;

        assert!(!published, "release fence must reject old-authority close");
        assert!(
            !callback_called.load(Ordering::SeqCst),
            "release fence must suppress close callback"
        );
        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Active);
        assert!(
            state.pending_dialog.is_some(),
            "old-authority close must not rewrite dialog projection"
        );
        assert!(state.last_result.is_none());
    }

    #[tokio::test]
    async fn publish_dialog_closed_rechecks_release_fence_after_waiting_for_write_authority() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "old authority dialog".to_string(),
                url: "https://example.test/dialog".to_string(),
                tab_target_id: Some("tab-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-04-24T00:00:00Z".to_string(),
            });
            state.last_dialog = state.pending_dialog.clone();
        }
        let write_guard = runtime.write().await;
        let release_in_progress = Arc::new(AtomicBool::new(false));
        let callback_called = Arc::new(AtomicBool::new(false));
        let callback_called_for_callback = callback_called.clone();
        let callback: ClosedCallback = Arc::new(move |_| {
            callback_called_for_callback.store(true, Ordering::SeqCst);
        });
        let runtime_for_task = runtime.clone();
        let release_for_task = release_in_progress.clone();
        let callback_for_task = callback.clone();
        let publish_task = tokio::spawn(async move {
            publish_dialog_closed(
                &runtime_for_task,
                Some(&callback_for_task),
                7,
                true,
                "ignored".to_string(),
                &release_for_task,
            )
            .await
        });

        tokio::task::yield_now().await;
        release_in_progress.store(true, Ordering::SeqCst);
        drop(write_guard);

        assert!(
            !publish_task.await.expect("publish task should finish"),
            "release fence must reject close after waiting for write authority"
        );
        assert!(
            !callback_called.load(Ordering::SeqCst),
            "release fence must suppress close callback"
        );
        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Active);
        assert!(state.pending_dialog.is_some());
        assert!(state.last_result.is_none());
    }

    #[tokio::test]
    async fn intercept_handle_failure_degrades_runtime_and_notifies_callback() {
        let runtime = new_shared_dialog_runtime();
        {
            let mut state = runtime.write().await;
            state.status = DialogRuntimeStatus::Active;
            state.pending_dialog = Some(PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "blocked".to_string(),
                url: "https://example.test/dialog".to_string(),
                tab_target_id: Some("tab-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-04-24T00:00:00Z".to_string(),
            });
        }
        let delivered = Arc::new(Mutex::new(Vec::<DialogRuntimeInfo>::new()));
        let delivered_for_callback = delivered.clone();
        let callback: RuntimeCallback = Arc::new(move |update| {
            delivered_for_callback
                .lock()
                .expect("runtime callback mutex")
                .push(update.runtime);
        });
        let release_in_progress = AtomicBool::new(false);
        let error = RubError::domain(ErrorCode::InvalidInput, "cdp dialog handle failed");

        assert!(
            publish_dialog_intercept_failure(
                &runtime,
                Some(&callback),
                11,
                &error,
                &release_in_progress,
            )
            .await
        );

        let state = runtime.read().await;
        assert_eq!(state.status, DialogRuntimeStatus::Degraded);
        assert!(state.pending_dialog.is_some());
        assert!(
            state
                .degraded_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("dialog_intercept_handle_failed:"))
        );
        let delivered = delivered.lock().expect("runtime callback mutex");
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].status, DialogRuntimeStatus::Degraded);
    }
}

#[cfg(test)]
mod intercept_policy_tests {
    use super::{intercept_policy_matches, take_matching_intercept_policy};
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

    #[test]
    fn poisoned_intercept_lock_is_recovered_during_take() {
        use super::SharedDialogIntercept;
        use std::sync::{Arc, Mutex};

        let intercept: SharedDialogIntercept =
            Arc::new(Mutex::new(Some(policy(true, Some("tab-A")))));
        let poisoned = intercept.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().expect("dialog intercept lock");
            panic!("poison dialog intercept lock");
        });

        let taken = take_matching_intercept_policy(&intercept, "tab-A");
        assert!(taken.is_some(), "poisoned intercept should still recover");
        assert!(
            intercept.lock().is_ok(),
            "take should clear the poison flag after recovery"
        );
    }
}
