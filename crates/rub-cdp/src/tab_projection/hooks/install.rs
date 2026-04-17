use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::{dom::EventDocumentUpdated, page::EventFrameNavigated};
use rub_core::error::{ErrorCode, RubError};
use std::fmt::{Debug, Display};
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use tracing::warn;

use super::runtime::{
    ProjectionContext, probe_runtime_state_for_active_page,
    projection_authority_commit_in_progress, projection_generation_current,
    refresh_identity_self_probe,
};
use super::{PageHookFlag, PageHookInstallState, PageHookResult, active_page_runtime_commit_ready};
use crate::listener_generation::next_listener_event;

mod identity;

const PAGE_HOOK_TIMEOUT: Duration = Duration::from_secs(3);
const PAGE_HOOK_INSTALL_POLL_FLOOR_MS: u64 = 25;
const PAGE_HOOK_INSTALL_POLL_STEP_MS: u64 = 25;
const PAGE_HOOK_INSTALL_POLL_CEILING_MS: u64 = 100;

#[derive(Debug, Default)]
struct PageHookInstallOutcome {
    critical_failed: bool,
    auxiliary_failed: bool,
}

impl PageHookInstallOutcome {
    fn mark_critical_failure(&mut self) {
        self.critical_failed = true;
    }

    async fn mark_auxiliary_failure(
        &mut self,
        context: &ProjectionContext,
        record_coverage_failure: bool,
    ) {
        self.auxiliary_failed = true;
        if record_coverage_failure {
            context
                .identity_coverage
                .lock()
                .await
                .record_page_hook_failure();
        }
    }

    fn any_failure(&self) -> bool {
        self.critical_failed || self.auxiliary_failed
    }
}

#[derive(Clone, Copy)]
struct AuxiliaryPageHookSpec {
    hook: PageHookFlag,
    label: &'static str,
    warn_message: &'static str,
    record_timeout_coverage: bool,
}

struct PageHookInstallTransaction {
    target_key: String,
    require_runtime_hooks: bool,
    baseline_hook_state: PageHookInstallState,
    hook_state: PageHookInstallState,
    outcome: PageHookInstallOutcome,
}

impl PageHookInstallTransaction {
    async fn begin(
        page: &Arc<Page>,
        context: &ProjectionContext,
        require_runtime_hooks: bool,
    ) -> Result<Option<Self>, RubError> {
        let target_key = page.target_id().as_ref().to_string();
        let Some((baseline_hook_state, hook_state)) =
            acquire_page_hook_install_state(&target_key, context, require_runtime_hooks).await?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            target_key,
            require_runtime_hooks,
            baseline_hook_state,
            hook_state,
            outcome: PageHookInstallOutcome::default(),
        }))
    }

    async fn finish(self, context: &ProjectionContext) -> Result<(), RubError> {
        if !projection_generation_current(context) {
            restore_page_hook_installation_baseline(
                &self.target_key,
                self.baseline_hook_state,
                &context.page_hook_states,
            )
            .await;
            return Ok(());
        }
        finalize_page_hook_installation(
            &self.target_key,
            self.hook_state,
            context,
            self.outcome,
            self.require_runtime_hooks,
        )
        .await
    }
}

pub(super) struct PageHookInstaller<'a> {
    page: Arc<Page>,
    context: &'a ProjectionContext,
    transaction: PageHookInstallTransaction,
}

impl<'a> PageHookInstaller<'a> {
    pub(super) async fn begin(
        page: Arc<Page>,
        context: &'a ProjectionContext,
        require_runtime_hooks: bool,
    ) -> Result<Option<Self>, RubError> {
        let Some(transaction) =
            PageHookInstallTransaction::begin(&page, context, require_runtime_hooks).await?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            page,
            context,
            transaction,
        }))
    }

    pub(super) async fn run(mut self) -> Result<(), RubError> {
        if !projection_generation_current(self.context) {
            return self.transaction.finish(self.context).await;
        }
        self.install_identity_hooks().await;
        if !projection_generation_current(self.context) {
            return self.transaction.finish(self.context).await;
        }
        self.install_critical_runtime_hooks().await;
        if !projection_generation_current(self.context) {
            return self.transaction.finish(self.context).await;
        }
        self.install_navigation_listener_hooks().await;
        if !projection_generation_current(self.context) {
            return self.transaction.finish(self.context).await;
        }
        self.install_runtime_callback_hooks().await;
        self.transaction.finish(self.context).await
    }

    async fn install_critical_runtime_hooks(&mut self) {
        let page = self.page.clone();
        let context = self.context;
        let PageHookInstallTransaction {
            hook_state,
            outcome,
            ..
        } = &mut self.transaction;

        install_critical_page_hook(
            hook_state,
            PageHookFlag::SelfProbe,
            "identity.self_probe",
            None,
            outcome,
            || async {
                refresh_identity_self_probe(&page, context).await;
                Ok::<(), ()>(())
            },
        )
        .await;

        let page = self.page.clone();
        install_critical_page_hook(
            hook_state,
            PageHookFlag::DomEnable,
            "dom.enable",
            None,
            outcome,
            || async { page.enable_dom().await },
        )
        .await;

        let page = self.page.clone();
        install_critical_page_hook(
            hook_state,
            PageHookFlag::RuntimeProbe,
            "runtime_state.probe",
            None,
            outcome,
            || async {
                probe_runtime_state_for_active_page(page.clone(), context).await;
                Ok::<(), ()>(())
            },
        )
        .await;
    }

    async fn install_navigation_listener_hooks(&mut self) {
        let page = self.page.clone();
        let context = self.context;
        let PageHookInstallTransaction {
            hook_state,
            outcome,
            ..
        } = &mut self.transaction;
        install_frame_listener(&page, context, hook_state, outcome).await;
        install_document_listener(&page, context, hook_state, outcome).await;
    }

    async fn install_runtime_callback_hooks(&mut self) {
        let context = self.context;
        let page = self.page.clone();
        let target_key = self.transaction.target_key.clone();
        let observatory_callbacks = guard_observatory_callbacks_for_commit(
            context.observatory_callbacks.lock().await.clone(),
            context,
        );
        let dialog_callbacks = guard_dialog_callbacks_for_commit(
            context.dialog_callbacks.lock().await.clone(),
            context,
        );
        let PageHookInstallTransaction {
            hook_state,
            outcome,
            ..
        } = &mut self.transaction;

        if !hook_state.contains(PageHookFlag::Observatory) {
            let observatory_pending_registry = {
                let mut registries = context.observatory_pending_registries.lock().await;
                registries
                    .entry(target_key.clone())
                    .or_insert_with(crate::runtime_observatory::new_shared_pending_request_registry)
                    .clone()
            };
            let observatory_page = page.clone();
            install_critical_page_hook(
                hook_state,
                PageHookFlag::Observatory,
                "observatory.install",
                Some("Runtime observatory install failed"),
                outcome,
                || async {
                    crate::runtime_observatory::ensure_page_observatory(
                        observatory_page,
                        observatory_callbacks,
                        context.request_correlation.clone(),
                        observatory_pending_registry,
                        context.listener_generation,
                        context.listener_generation_rx.clone(),
                    )
                    .await
                },
            )
            .await;
        }

        let dialog_page = page.clone();
        install_critical_page_hook(
            hook_state,
            PageHookFlag::Dialogs,
            "dialogs.install",
            Some("Dialog hook installation failed before commit"),
            outcome,
            || async {
                crate::dialogs::ensure_page_dialog_runtime(
                    dialog_page,
                    dialog_callbacks,
                    context.dialog_runtime.clone(),
                    context.dialog_intercept.clone(),
                    context.listener_generation,
                    context.listener_generation_rx.clone(),
                )
                .await
            },
        )
        .await;

        install_critical_page_hook(
            hook_state,
            PageHookFlag::NetworkRules,
            "network_rules.install",
            Some("Network rule interception install failed"),
            outcome,
            || async {
                crate::network_rules::ensure_page_request_interception(
                    page,
                    context.network_rule_runtime.clone(),
                    context.request_correlation.clone(),
                    context.listener_generation,
                    context.listener_generation_rx.clone(),
                )
                .await
            },
        )
        .await;
    }
}

async fn acquire_page_hook_install_state(
    target_key: &str,
    context: &ProjectionContext,
    require_runtime_hooks: bool,
) -> Result<Option<(PageHookInstallState, PageHookInstallState)>, RubError> {
    loop {
        if !projection_generation_current(context) {
            return Ok(None);
        }
        let maybe_state = {
            let mut hook_states = context.page_hook_states.lock().await;
            let state = hook_states.entry(target_key.to_string()).or_default();
            if state.complete() {
                return Ok(None);
            }
            if state.installing {
                None
            } else {
                let baseline = state.clone();
                state.installing = true;
                Some((baseline, state.clone()))
            }
        };
        if let Some(state) = maybe_state {
            return Ok(Some(state));
        }
        if !require_runtime_hooks {
            return Ok(None);
        }
        wait_for_active_page_hook_installation(target_key, &context.page_hook_states).await?;
    }
}

async fn finalize_page_hook_installation(
    target_key: &str,
    hook_state: PageHookInstallState,
    context: &ProjectionContext,
    outcome: PageHookInstallOutcome,
    require_runtime_hooks: bool,
) -> Result<(), RubError> {
    let install_complete = hook_state.complete();
    let mut hook_states = context.page_hook_states.lock().await;
    let state = hook_states.entry(target_key.to_string()).or_default();
    *state = hook_state;
    state.installing = false;
    if install_complete && !state.installation_recorded {
        context
            .identity_coverage
            .lock()
            .await
            .record_page_hook_installation();
        state.installation_recorded = true;
    } else if outcome.any_failure() {
        context
            .identity_coverage
            .lock()
            .await
            .record_page_hook_failure();
    }

    if require_runtime_hooks && !active_page_runtime_commit_ready(state, outcome.critical_failed) {
        return Err(active_page_runtime_hooks_incomplete_error(
            target_key,
            "critical_page_hooks_incomplete",
            "Active page runtime hooks did not install completely",
        ));
    }

    Ok(())
}

async fn restore_page_hook_installation_baseline(
    target_key: &str,
    baseline_hook_state: PageHookInstallState,
    page_hook_states: &tokio::sync::Mutex<std::collections::HashMap<String, PageHookInstallState>>,
) {
    let mut hook_states = page_hook_states.lock().await;
    let state = hook_states.entry(target_key.to_string()).or_default();
    *state = baseline_hook_state;
    state.installing = false;
}

async fn install_auxiliary_page_hook<F, Fut, T, E>(
    state: &mut PageHookInstallState,
    spec: AuxiliaryPageHookSpec,
    context: &ProjectionContext,
    outcome: &mut PageHookInstallOutcome,
    op: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: Display,
{
    if state.contains(spec.hook) {
        return;
    }
    match page_hook_with_timeout(spec.label, op).await {
        PageHookResult::Completed(Ok(_)) => state.mark(spec.hook),
        PageHookResult::Completed(Err(error)) => {
            warn!(error = %error, "{}", spec.warn_message);
            outcome.mark_auxiliary_failure(context, true).await;
        }
        PageHookResult::TimedOut => {
            outcome
                .mark_auxiliary_failure(context, spec.record_timeout_coverage)
                .await;
        }
    }
}

async fn install_critical_page_hook<F, Fut, T, E>(
    state: &mut PageHookInstallState,
    hook: PageHookFlag,
    label: &'static str,
    warn_message: Option<&'static str>,
    outcome: &mut PageHookInstallOutcome,
    op: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: Debug,
{
    if state.contains(hook) {
        return;
    }
    match page_hook_with_timeout(label, op).await {
        PageHookResult::Completed(Ok(_)) => state.mark(hook),
        PageHookResult::Completed(Err(error)) => {
            if let Some(message) = warn_message {
                warn!(?error, "{message}");
            }
            outcome.mark_critical_failure();
        }
        PageHookResult::TimedOut => outcome.mark_critical_failure(),
    }
}

async fn install_frame_listener(
    page: &Arc<Page>,
    context: &ProjectionContext,
    state: &mut PageHookInstallState,
    outcome: &mut PageHookInstallOutcome,
) {
    if state.contains(PageHookFlag::FrameListener) {
        return;
    }
    if let Ok(Ok(mut listener)) = tokio::time::timeout(
        PAGE_HOOK_TIMEOUT,
        page.event_listener::<EventFrameNavigated>(),
    )
    .await
    {
        let callback_store = context.epoch_callback.clone();
        let page = page.clone();
        let projection_context = context.clone();
        tokio::spawn(async move {
            let mut generation_rx = projection_context.listener_generation_rx.clone();
            while let Some(event) = next_listener_event(
                &mut listener,
                projection_context.listener_generation,
                &mut generation_rx,
            )
            .await
            {
                if !projection_generation_current(&projection_context) {
                    break;
                }
                if event.frame.parent_id.is_some() {
                    continue;
                }
                if !projection_authority_commit_in_progress(&projection_context)
                    && let Some(callback) = callback_store.lock().await.as_ref().cloned()
                {
                    callback(Some(page.target_id().as_ref()));
                }
                refresh_identity_self_probe(&page, &projection_context).await;
                probe_runtime_state_for_active_page(page.clone(), &projection_context).await;
            }
        });
        state.mark(PageHookFlag::FrameListener);
    } else {
        outcome.mark_critical_failure();
    }
}

async fn install_document_listener(
    page: &Arc<Page>,
    context: &ProjectionContext,
    state: &mut PageHookInstallState,
    outcome: &mut PageHookInstallOutcome,
) {
    if state.contains(PageHookFlag::DocumentListener) {
        return;
    }
    if let Ok(Ok(mut listener)) = tokio::time::timeout(
        PAGE_HOOK_TIMEOUT,
        page.event_listener::<EventDocumentUpdated>(),
    )
    .await
    {
        let callback_store = context.epoch_callback.clone();
        let page = page.clone();
        let listener_generation = context.listener_generation;
        let listener_generation_rx = context.listener_generation_rx.clone();
        let projection_context = context.clone();
        tokio::spawn(async move {
            let mut generation_rx = listener_generation_rx;
            while let Some(_event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                if !projection_generation_current(&projection_context) {
                    break;
                }
                if !projection_authority_commit_in_progress(&projection_context)
                    && let Some(callback) = callback_store.lock().await.as_ref().cloned()
                {
                    callback(Some(page.target_id().as_ref()));
                }
                probe_runtime_state_for_active_page(page.clone(), &projection_context).await;
            }
        });
        state.mark(PageHookFlag::DocumentListener);
    } else {
        outcome.mark_critical_failure();
    }
}

fn guard_observatory_callbacks_for_commit(
    callbacks: crate::runtime_observatory::ObservatoryCallbacks,
    context: &ProjectionContext,
) -> crate::runtime_observatory::ObservatoryCallbacks {
    let authority_commit_in_progress = context.authority_commit_in_progress.clone();
    crate::runtime_observatory::ObservatoryCallbacks {
        on_console_error: callbacks
            .on_console_error
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_page_error: callbacks
            .on_page_error
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_network_failure: callbacks
            .on_network_failure
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_request_summary: callbacks
            .on_request_summary
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_request_record: callbacks
            .on_request_record
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_runtime_degraded: callbacks
            .on_runtime_degraded
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
    }
}

fn guard_dialog_callbacks_for_commit(
    callbacks: crate::dialogs::DialogCallbacks,
    context: &ProjectionContext,
) -> crate::dialogs::DialogCallbacks {
    let authority_commit_in_progress = context.authority_commit_in_progress.clone();
    crate::dialogs::DialogCallbacks {
        on_runtime: callbacks
            .on_runtime
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_opened: callbacks
            .on_opened
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
        on_closed: callbacks
            .on_closed
            .map(|callback| guard_callback(callback, authority_commit_in_progress.clone())),
    }
}

fn guard_callback<T>(
    callback: Arc<dyn Fn(T) + Send + Sync>,
    authority_commit_in_progress: Arc<std::sync::atomic::AtomicBool>,
) -> Arc<dyn Fn(T) + Send + Sync>
where
    T: 'static,
{
    Arc::new(move |value: T| {
        if authority_commit_in_progress.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        callback(value);
    })
}

fn active_page_runtime_hooks_incomplete_error(
    target_key: &str,
    reason: &'static str,
    message: &'static str,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::BrowserLaunchFailed,
        message,
        serde_json::json!({
            "reason": reason,
            "target_id": target_key,
        }),
    )
}

async fn wait_for_active_page_hook_installation(
    target_key: &str,
    page_hook_states: &tokio::sync::Mutex<std::collections::HashMap<String, PageHookInstallState>>,
) -> Result<(), RubError> {
    let deadline = tokio::time::Instant::now() + PAGE_HOOK_TIMEOUT;
    let mut poll_count = 0u32;
    loop {
        let (still_installing, install_failed) = {
            let hook_states = page_hook_states.lock().await;
            match hook_states.get(target_key) {
                Some(state) => (state.installing, !state.critical_runtime_hooks_complete()),
                None => (false, false),
            }
        };
        if !still_installing {
            return if install_failed {
                Err(active_page_runtime_hooks_incomplete_error(
                    target_key,
                    "critical_page_hooks_incomplete",
                    "Active page runtime hooks did not install completely",
                ))
            } else {
                Ok(())
            };
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            warn!(
                target_id = target_key,
                timeout_ms = PAGE_HOOK_TIMEOUT.as_millis(),
                "Active page runtime hooks exceeded the bounded install fence; failing closed instead of waiting indefinitely"
            );
            return Err(RubError::domain_with_context(
                ErrorCode::BrowserLaunchFailed,
                "Active page runtime hooks did not commit before the install timeout",
                serde_json::json!({
                    "reason": "active_page_hook_install_timeout",
                    "target_id": target_key,
                    "timeout_ms": PAGE_HOOK_TIMEOUT.as_millis(),
                }),
            ));
        }
        let remaining = deadline.saturating_duration_since(now);
        sleep(page_hook_install_poll_delay(poll_count).min(remaining)).await;
        poll_count = poll_count.saturating_add(1);
    }
}

fn page_hook_install_poll_delay(poll_count: u32) -> Duration {
    let delay_ms = PAGE_HOOK_INSTALL_POLL_FLOOR_MS
        .saturating_add(PAGE_HOOK_INSTALL_POLL_STEP_MS.saturating_mul(u64::from(poll_count)));
    Duration::from_millis(delay_ms.min(PAGE_HOOK_INSTALL_POLL_CEILING_MS))
}

async fn page_hook_with_timeout<F, Fut, T, E>(label: &'static str, op: F) -> PageHookResult<T, E>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    match tokio::time::timeout(PAGE_HOOK_TIMEOUT, op()).await {
        Ok(result) => PageHookResult::Completed(result),
        Err(_) => {
            warn!(
                hook = label,
                timeout_ms = PAGE_HOOK_TIMEOUT.as_millis(),
                "Page hook timed out; continuing with degraded auxiliary coverage"
            );
            PageHookResult::TimedOut
        }
    }
}

#[cfg(test)]
mod tests;
