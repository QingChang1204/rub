//! Tab projection, page hooks, and launch-policy projection helpers.

use chromiumoxide::Page;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::{
    dom::EventDocumentUpdated,
    emulation::{SetDeviceMetricsOverrideParams, SetTouchEmulationEnabledParams},
    page::EventFrameNavigated,
    target::TargetId,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{ConnectionTarget, TabInfo};
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};
use tracing::warn;

use crate::browser::BrowserLaunchOptions;
use crate::dialogs::DialogCallbacks;
use crate::identity_coverage::IdentityCoverageRegistry;
use crate::identity_policy::IdentityPolicy;
use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, next_listener_event};
use crate::network_rules::NetworkRuleRuntime;
use crate::request_correlation::RequestCorrelationRegistry;
use crate::runtime_observatory::ObservatoryCallbacks;
use crate::runtime_state::RuntimeStateCallbacks;

/// Callback type for CDP event-driven epoch increments (INV-001 Source B).
pub(crate) type EpochCallback = Arc<dyn Fn() + Send + Sync>;
const PAGE_HOOK_TIMEOUT: Duration = Duration::from_secs(3);
const PAGE_HOOK_INSTALL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const TAB_INFO_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

enum PageHookResult<T, E> {
    Completed(Result<T, E>),
    TimedOut,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PageHookInstallState {
    installing: bool,
    installation_recorded: bool,
    environment_metrics: bool,
    touch_emulation: bool,
    stealth_new_document: bool,
    stealth_live: bool,
    user_agent: bool,
    self_probe: bool,
    dom_enable: bool,
    runtime_probe: bool,
    frame_listener: bool,
    document_listener: bool,
    observatory: bool,
    dialogs: bool,
    network_rules: bool,
}

impl PageHookInstallState {
    fn complete(&self) -> bool {
        self.environment_metrics
            && self.touch_emulation
            && self.stealth_new_document
            && self.stealth_live
            && self.user_agent
            && self.self_probe
            && self.dom_enable
            && self.runtime_probe
            && self.frame_listener
            && self.document_listener
            && self.observatory
            && self.dialogs
            && self.network_rules
    }

    fn critical_runtime_hooks_complete(&self) -> bool {
        self.self_probe
            && self.dom_enable
            && self.runtime_probe
            && self.frame_listener
            && self.document_listener
            && self.observatory
            && self.dialogs
            && self.network_rules
    }

    pub(crate) fn invalidate_runtime_callback_hooks(&mut self) {
        self.installing = false;
        self.runtime_probe = false;
        self.frame_listener = false;
        self.document_listener = false;
        self.observatory = false;
        self.dialogs = false;
        self.network_rules = false;
    }

    #[cfg(test)]
    pub(crate) fn completed_runtime_callback_hooks_for_test() -> Self {
        Self {
            runtime_probe: true,
            frame_listener: true,
            document_listener: true,
            observatory: true,
            dialogs: true,
            network_rules: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn runtime_callback_hooks_cleared_for_test(&self) -> bool {
        !self.runtime_probe
            && !self.frame_listener
            && !self.document_listener
            && !self.observatory
            && !self.dialogs
            && !self.network_rules
    }
}

fn active_page_runtime_commit_ready(
    state: &PageHookInstallState,
    critical_install_failed: bool,
) -> bool {
    state.critical_runtime_hooks_complete() && !critical_install_failed
}

fn user_agent_protocol_override_succeeded<T, E>(result: &PageHookResult<T, E>) -> bool {
    matches!(result, PageHookResult::Completed(Ok(_)))
}

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

struct PageHookInstallTransaction {
    target_key: String,
    require_runtime_hooks: bool,
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
        let Some(hook_state) =
            acquire_page_hook_install_state(&target_key, context, require_runtime_hooks).await?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            target_key,
            require_runtime_hooks,
            hook_state,
            outcome: PageHookInstallOutcome::default(),
        }))
    }

    async fn finish(self, context: &ProjectionContext) -> Result<(), RubError> {
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

struct PageHookInstaller<'a> {
    page: Arc<Page>,
    context: &'a ProjectionContext,
    transaction: PageHookInstallTransaction,
}

impl<'a> PageHookInstaller<'a> {
    async fn begin(
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

    async fn run(mut self) -> Result<(), RubError> {
        self.install_identity_hooks().await;
        self.install_critical_runtime_hooks().await;
        self.install_navigation_listener_hooks().await;
        self.install_runtime_callback_hooks().await;
        self.transaction.finish(self.context).await
    }

    async fn install_identity_hooks(&mut self) {
        if !self.context.identity_policy.stealth_enabled() {
            self.transaction.hook_state.environment_metrics = true;
            self.transaction.hook_state.touch_emulation = true;
            self.transaction.hook_state.stealth_new_document = true;
            self.transaction.hook_state.stealth_live = true;
            self.transaction.hook_state.user_agent = true;
            return;
        }

        let context = self.context;
        let page = self.page.clone();
        let stealth_cfg = context.identity_policy.stealth_config();
        let PageHookInstallTransaction {
            hook_state,
            outcome,
            ..
        } = &mut self.transaction;

        if let Some(environment_profile) = context.identity_policy.environment_profile() {
            install_auxiliary_page_hook(
                &mut hook_state.environment_metrics,
                "stealth.environment_metrics",
                "Stealth environment metrics override failed",
                context,
                outcome,
                false,
                || async {
                    page.execute(SetDeviceMetricsOverrideParams::new(
                        environment_profile.window_width,
                        environment_profile.window_height,
                        environment_profile.device_scale_factor,
                        false,
                    ))
                    .await
                },
            )
            .await;

            let page = self.page.clone();
            install_auxiliary_page_hook(
                &mut hook_state.touch_emulation,
                "stealth.touch_emulation",
                "Stealth touch emulation override failed",
                context,
                outcome,
                false,
                || async {
                    page.execute(SetTouchEmulationEnabledParams::new(
                        environment_profile.touch_enabled,
                    ))
                    .await
                },
            )
            .await;
        } else {
            hook_state.environment_metrics = true;
            hook_state.touch_emulation = true;
        }

        if let Some(script) = crate::stealth::combined_stealth_script(&stealth_cfg) {
            let page = self.page.clone();
            install_auxiliary_page_hook(
                &mut hook_state.stealth_new_document,
                "stealth.evaluate_on_new_document",
                "Stealth patch injection failed (evaluate_on_new_document)",
                context,
                outcome,
                false,
                || async { page.evaluate_on_new_document(script.as_str()).await },
            )
            .await;

            let page = self.page.clone();
            install_auxiliary_page_hook(
                &mut hook_state.stealth_live,
                "stealth.evaluate",
                "Stealth patch injection failed (evaluate)",
                context,
                outcome,
                false,
                || async { page.evaluate(script.as_str()).await },
            )
            .await;
        } else {
            hook_state.stealth_new_document = true;
            hook_state.stealth_live = true;
        }

        self.install_user_agent_hook().await;
    }

    async fn install_user_agent_hook(&mut self) {
        if self.transaction.hook_state.user_agent {
            return;
        }

        let context = self.context;
        let page = self.page.clone();
        match page_hook_with_timeout("ua.read_current_user_agent", || async {
            page.user_agent().await
        })
        .await
        {
            PageHookResult::Completed(Ok(ua)) => {
                if let Some(profile) = context.identity_policy.user_agent_override(&ua) {
                    let page = self.page.clone();
                    let protocol_override_result =
                        page_hook_with_timeout("ua.set_user_agent", || async {
                            page.set_user_agent(profile.params.clone()).await
                        })
                        .await;
                    let protocol_override_applied =
                        user_agent_protocol_override_succeeded(&protocol_override_result);
                    match protocol_override_result {
                        PageHookResult::Completed(Ok(_)) => {
                            self.transaction.hook_state.user_agent = true;
                            context
                                .identity_coverage
                                .lock()
                                .await
                                .record_user_agent_override(profile.has_metadata);
                        }
                        PageHookResult::Completed(Err(error)) => {
                            warn!(error = %error, "User-Agent override failed (set_user_agent)");
                            self.transaction
                                .outcome
                                .mark_auxiliary_failure(context, true)
                                .await;
                        }
                        PageHookResult::TimedOut => {
                            self.transaction
                                .outcome
                                .mark_auxiliary_failure(context, false)
                                .await;
                        }
                    }
                    if !protocol_override_applied {
                        let page = self.page.clone();
                        let fallback_new_document =
                            page_hook_with_timeout("ua.evaluate_on_new_document", || async {
                                page.evaluate_on_new_document(profile.script.as_str()).await
                            })
                            .await;
                        let page = self.page.clone();
                        let fallback_live = page_hook_with_timeout("ua.evaluate", || async {
                            page.evaluate(profile.script.as_str()).await
                        })
                        .await;
                        match (&fallback_new_document, &fallback_live) {
                            (
                                PageHookResult::Completed(Ok(_)),
                                PageHookResult::Completed(Ok(_)),
                            ) => {
                                self.transaction.hook_state.user_agent = true;
                                context
                                    .identity_coverage
                                    .lock()
                                    .await
                                    .record_user_agent_override(false);
                            }
                            _ => {
                                self.transaction
                                    .outcome
                                    .mark_auxiliary_failure(context, true)
                                    .await
                            }
                        }
                        if let PageHookResult::Completed(Err(ref error)) = fallback_new_document {
                            warn!(
                                error = %error,
                                "User-Agent override fallback failed (evaluate_on_new_document)"
                            );
                        }
                        if let PageHookResult::Completed(Err(ref error)) = fallback_live {
                            warn!(error = %error, "User-Agent override fallback failed (evaluate)");
                        }
                    }
                } else {
                    self.transaction.hook_state.user_agent = true;
                }
            }
            PageHookResult::Completed(Err(error)) => {
                warn!(error = %error, "Failed to read current User-Agent for hook installation");
                self.transaction
                    .outcome
                    .mark_auxiliary_failure(context, true)
                    .await;
            }
            PageHookResult::TimedOut => {
                self.transaction
                    .outcome
                    .mark_auxiliary_failure(context, true)
                    .await
            }
        }
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
            &mut hook_state.self_probe,
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
            &mut hook_state.dom_enable,
            "dom.enable",
            None,
            outcome,
            || async { page.enable_dom().await },
        )
        .await;

        let page = self.page.clone();
        install_critical_page_hook(
            &mut hook_state.runtime_probe,
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
        install_frame_listener(&page, context, &mut hook_state.frame_listener, outcome).await;
        install_document_listener(&page, context, &mut hook_state.document_listener, outcome).await;
    }

    async fn install_runtime_callback_hooks(&mut self) {
        let context = self.context;
        let page = self.page.clone();
        let target_key = self.transaction.target_key.clone();
        let observatory_callbacks = context.observatory_callbacks.lock().await.clone();
        let dialog_callbacks = context.dialog_callbacks.lock().await.clone();
        let PageHookInstallTransaction {
            hook_state,
            outcome,
            ..
        } = &mut self.transaction;

        if !hook_state.observatory {
            let observatory_pending_registry = {
                let mut registries = context.observatory_pending_registries.lock().await;
                registries
                    .entry(target_key.clone())
                    .or_insert_with(crate::runtime_observatory::new_shared_pending_request_registry)
                    .clone()
            };
            let observatory_page = page.clone();
            install_critical_page_hook(
                &mut hook_state.observatory,
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
            &mut hook_state.dialogs,
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
            &mut hook_state.network_rules,
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

#[derive(Clone)]
pub(crate) struct ProjectionContext {
    pub(crate) browser: Arc<Browser>,
    pub(crate) page_hook_states: Arc<Mutex<HashMap<String, PageHookInstallState>>>,
    pub(crate) active_target_store: Arc<Mutex<Option<TargetId>>>,
    pub(crate) epoch_callback: Arc<Mutex<Option<EpochCallback>>>,
    pub(crate) observatory_callbacks: Arc<Mutex<ObservatoryCallbacks>>,
    pub(crate) runtime_state_callbacks: Arc<Mutex<RuntimeStateCallbacks>>,
    pub(crate) dialog_callbacks: Arc<Mutex<DialogCallbacks>>,
    pub(crate) dialog_runtime: crate::dialogs::SharedDialogRuntime,
    pub(crate) dialog_intercept: crate::dialogs::SharedDialogIntercept,
    pub(crate) network_rule_runtime: Arc<tokio::sync::RwLock<NetworkRuleRuntime>>,
    pub(crate) request_correlation: Arc<Mutex<RequestCorrelationRegistry>>,
    pub(crate) observatory_pending_registries:
        Arc<Mutex<HashMap<String, crate::runtime_observatory::SharedPendingRequestRegistry>>>,
    pub(crate) identity_policy: IdentityPolicy,
    pub(crate) identity_coverage: Arc<Mutex<IdentityCoverageRegistry>>,
    pub(crate) listener_generation: ListenerGeneration,
    pub(crate) listener_generation_rx: ListenerGenerationRx,
}

pub(crate) async fn sync_tabs_projection_with(
    context: &ProjectionContext,
    pages_store: Arc<Mutex<Vec<Arc<Page>>>>,
    current_page_store: Arc<Mutex<Option<Arc<Page>>>>,
    active_target_store: Arc<Mutex<Option<TargetId>>>,
) -> Result<(), RubError> {
    let fresh_pages = context.browser.pages().await.map_err(|e| {
        RubError::domain(
            ErrorCode::BrowserCrashed,
            format!("Failed to enumerate browser tabs: {e}"),
        )
    })?;

    let projected = fresh_pages.into_iter().map(Arc::new).collect::<Vec<_>>();
    let live_target_ids = projected
        .iter()
        .map(|page| page.target_id().as_ref().to_string())
        .collect::<HashSet<_>>();
    context
        .page_hook_states
        .lock()
        .await
        .retain(|target_id, _| live_target_ids.contains(target_id));
    crate::runtime_observatory::prune_stale_pending_request_registries(
        &context.observatory_pending_registries,
        &live_target_ids,
    )
    .await;

    let active_target = {
        let active_guard = active_target_store.lock().await;
        if active_guard
            .as_ref()
            .is_some_and(|target| projected.iter().any(|page| page.target_id() == target))
        {
            active_guard.clone()
        } else {
            projected.first().map(|page| page.target_id().clone())
        }
    };
    *active_target_store.lock().await = active_target.clone();

    for page in &projected {
        let require_runtime_hooks = active_target
            .as_ref()
            .is_some_and(|target| page.target_id() == target);
        ensure_page_hooks(page.clone(), context, require_runtime_hooks).await?;
    }

    let active_page = active_target.as_ref().and_then(|target| {
        projected
            .iter()
            .find(|page| page.target_id() == target)
            .cloned()
    });

    *current_page_store.lock().await = active_page;
    *pages_store.lock().await = projected;
    if let Some(active_page) = current_page_store.lock().await.clone() {
        probe_runtime_state_for_active_page(active_page, context).await;
    }
    Ok(())
}

async fn page_has_active_target(
    page: &Arc<Page>,
    active_target_store: &Arc<Mutex<Option<TargetId>>>,
) -> bool {
    active_target_store
        .lock()
        .await
        .as_ref()
        .is_some_and(|target| page.target_id() == target)
}

async fn probe_runtime_state_for_active_page(page: Arc<Page>, context: &ProjectionContext) {
    if !page_has_active_target(&page, &context.active_target_store).await {
        return;
    }

    let callbacks = context.runtime_state_callbacks.lock().await.clone();
    if callbacks.is_empty() {
        return;
    }

    let Some(allocate_sequence) = callbacks.allocate_sequence.clone() else {
        return;
    };
    let Some(on_snapshot) = callbacks.on_snapshot.clone() else {
        return;
    };

    let sequence = allocate_sequence();
    let snapshot = crate::runtime_state::capture_runtime_state(&page).await;
    if !page_has_active_target(&page, &context.active_target_store).await {
        return;
    }
    on_snapshot(sequence, snapshot);
}

pub(crate) async fn ensure_page_hooks(
    page: Arc<Page>,
    context: &ProjectionContext,
    require_runtime_hooks: bool,
) -> Result<(), RubError> {
    let Some(installer) = PageHookInstaller::begin(page, context, require_runtime_hooks).await?
    else {
        return Ok(());
    };
    installer.run().await
}

async fn acquire_page_hook_install_state(
    target_key: &str,
    context: &ProjectionContext,
    require_runtime_hooks: bool,
) -> Result<Option<PageHookInstallState>, RubError> {
    loop {
        let maybe_state = {
            let mut hook_states = context.page_hook_states.lock().await;
            let state = hook_states.entry(target_key.to_string()).or_default();
            if state.complete() {
                return Ok(None);
            }
            if state.installing {
                None
            } else {
                state.installing = true;
                Some(state.clone())
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

async fn install_auxiliary_page_hook<F, Fut, T, E>(
    completed: &mut bool,
    label: &'static str,
    warn_message: &'static str,
    context: &ProjectionContext,
    outcome: &mut PageHookInstallOutcome,
    record_timeout_coverage: bool,
    op: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: Display,
{
    if *completed {
        return;
    }
    match page_hook_with_timeout(label, op).await {
        PageHookResult::Completed(Ok(_)) => *completed = true,
        PageHookResult::Completed(Err(error)) => {
            warn!(error = %error, "{warn_message}");
            outcome.mark_auxiliary_failure(context, true).await;
        }
        PageHookResult::TimedOut => {
            outcome
                .mark_auxiliary_failure(context, record_timeout_coverage)
                .await;
        }
    }
}

async fn install_critical_page_hook<F, Fut, T, E>(
    completed: &mut bool,
    label: &'static str,
    warn_message: Option<&'static str>,
    outcome: &mut PageHookInstallOutcome,
    op: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: Debug,
{
    if *completed {
        return;
    }
    match page_hook_with_timeout(label, op).await {
        PageHookResult::Completed(Ok(_)) => *completed = true,
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
    completed: &mut bool,
    outcome: &mut PageHookInstallOutcome,
) {
    if *completed {
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
                if event.frame.parent_id.is_some() {
                    continue;
                }
                if let Some(callback) = callback_store.lock().await.as_ref().cloned() {
                    callback();
                }
                refresh_identity_self_probe(&page, &projection_context).await;
                probe_runtime_state_for_active_page(page.clone(), &projection_context).await;
            }
        });
        *completed = true;
    } else {
        outcome.mark_critical_failure();
    }
}

async fn install_document_listener(
    page: &Arc<Page>,
    context: &ProjectionContext,
    completed: &mut bool,
    outcome: &mut PageHookInstallOutcome,
) {
    if *completed {
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
                if let Some(callback) = callback_store.lock().await.as_ref().cloned() {
                    callback();
                }
                probe_runtime_state_for_active_page(page.clone(), &projection_context).await;
            }
        });
        *completed = true;
    } else {
        outcome.mark_critical_failure();
    }
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
    page_hook_states: &Arc<Mutex<HashMap<String, PageHookInstallState>>>,
) -> Result<(), RubError> {
    let deadline = tokio::time::Instant::now() + PAGE_HOOK_TIMEOUT;
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
        if tokio::time::Instant::now() >= deadline {
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
        sleep(PAGE_HOOK_INSTALL_POLL_INTERVAL).await;
    }
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

async fn refresh_identity_self_probe(page: &Arc<Page>, context: &ProjectionContext) {
    if !context.identity_policy.stealth_enabled() {
        return;
    }

    let probe =
        crate::identity_probe::run_identity_self_probe(page, &context.identity_policy).await;
    context
        .identity_coverage
        .lock()
        .await
        .record_self_probe(probe);
}

pub(crate) fn projected_stealth_patch_names(
    options: &BrowserLaunchOptions,
    connection_target: Option<&ConnectionTarget>,
    config: &crate::stealth::StealthConfig,
) -> Vec<String> {
    let mut patches = crate::stealth::applied_patch_names(config);
    if options.stealth && is_managed_launch_projection(connection_target) {
        patches.push("clean_chrome_args".to_string());
    }
    patches
}

pub(crate) async fn wait_for_startup_page(browser: &mut Browser) -> Result<Page, RubError> {
    const STARTUP_PAGE_POLL_ATTEMPTS: usize = 20;
    const STARTUP_PAGE_POLL_INTERVAL_MS: u64 = 50;
    let mut last_error =
        "Browser did not expose an authoritative startup page before startup commit".to_string();

    for attempt in 0..STARTUP_PAGE_POLL_ATTEMPTS {
        let pages = browser.pages().await.map_err(|e| {
            RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to enumerate startup pages: {e}"),
            )
        })?;

        if pages.is_empty() {
            last_error =
                "Browser did not expose any startup pages before startup commit".to_string();
        } else if pages.len() == 1 {
            return Ok(pages.into_iter().next().expect("single startup page"));
        } else {
            let targets = browser.fetch_targets().await.map_err(|e| {
                RubError::domain(
                    ErrorCode::BrowserLaunchFailed,
                    format!("Failed to resolve startup page authority: {e}"),
                )
            })?;
            let attached_target_ids = targets
                .into_iter()
                .filter(|target| {
                    target.r#type == "page"
                        && target.attached
                        && target.subtype.as_deref() != Some("prerender")
                })
                .map(|target| target.target_id.as_ref().to_string())
                .collect::<Vec<_>>();
            let page_target_ids = pages
                .iter()
                .map(|page| page.target_id().as_ref().to_string())
                .collect::<Vec<_>>();
            if let Some(index) =
                crate::runtime::select_attached_page_index(&page_target_ids, &attached_target_ids)
            {
                return Ok(pages
                    .into_iter()
                    .nth(index)
                    .expect("attached startup page index should be valid"));
            }
            last_error =
                "Browser did not expose a unique authoritative startup page before startup commit"
                    .to_string();
        }

        if attempt + 1 < STARTUP_PAGE_POLL_ATTEMPTS {
            sleep(Duration::from_millis(STARTUP_PAGE_POLL_INTERVAL_MS)).await;
        }
    }

    Err(RubError::domain(ErrorCode::BrowserLaunchFailed, last_error))
}

pub(crate) async fn tab_info_for_page(
    index: u32,
    page: &Arc<Page>,
    active: Option<&TargetId>,
) -> TabInfo {
    let url = match tokio::time::timeout(TAB_INFO_PROBE_TIMEOUT, page.url()).await {
        Ok(Ok(Some(url))) => projected_tab_url(Some(url.to_string())),
        _ => projected_tab_url(None),
    };
    let title = match tokio::time::timeout(TAB_INFO_PROBE_TIMEOUT, page.get_title()).await {
        Ok(Ok(Some(title))) => projected_tab_title(Some(title)),
        _ => projected_tab_title(None),
    };

    TabInfo {
        index,
        target_id: page.target_id().as_ref().to_string(),
        url: normalize_tab_url(url),
        title,
        active: active
            .map(|target| target == page.target_id())
            .unwrap_or(false),
    }
}

pub(crate) fn tab_not_found(index: u32, total: usize) -> RubError {
    RubError::domain(
        ErrorCode::TabNotFound,
        format!(
            "Tab index {} out of range (0..{})",
            index,
            total.saturating_sub(1)
        ),
    )
}

fn is_managed_launch_projection(connection_target: Option<&ConnectionTarget>) -> bool {
    !matches!(
        connection_target,
        Some(ConnectionTarget::CdpUrl { .. } | ConnectionTarget::AutoDiscovered { .. })
    )
}

fn normalize_tab_url(url: String) -> String {
    if url.starts_with("chrome://new-tab-page") || url.starts_with("chrome-search://local-ntp") {
        "about:blank".to_string()
    } else {
        url
    }
}

const TAB_URL_PROBE_UNAVAILABLE: &str = "about:rub-probe-unavailable";
const TAB_TITLE_PROBE_UNAVAILABLE: &str = "[probe unavailable]";

fn projected_tab_url(url: Option<String>) -> String {
    url.map(normalize_tab_url)
        .unwrap_or_else(|| TAB_URL_PROBE_UNAVAILABLE.to_string())
}

fn projected_tab_title(title: Option<String>) -> String {
    title.unwrap_or_else(|| TAB_TITLE_PROBE_UNAVAILABLE.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        PageHookInstallState, PageHookResult, active_page_runtime_commit_ready,
        projected_stealth_patch_names, projected_tab_title, projected_tab_url,
        user_agent_protocol_override_succeeded,
    };
    use crate::browser::BrowserLaunchOptions;
    use crate::identity_policy::{IdentityPolicy, UserAgentOverrideProfile};
    use rub_core::model::ConnectionTarget;

    #[test]
    fn user_agent_override_script_escapes_single_quotes() {
        let script = IdentityPolicy::from_options(&BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        })
        .user_agent_override("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/146.0.0.0 'Test' Safari/537.36")
        .map(|UserAgentOverrideProfile { script, .. }| script)
        .unwrap();
        assert!(script.contains("\\'Test\\'"));
        assert!(script.contains("navigator"));
    }

    #[test]
    fn projected_stealth_patch_names_include_clean_args_for_managed_sessions() {
        let options = BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        };
        let config = crate::stealth::StealthConfig::default();
        let managed =
            projected_stealth_patch_names(&options, Some(&ConnectionTarget::Managed), &config);
        assert!(managed.iter().any(|patch| patch == "clean_chrome_args"));

        let external = projected_stealth_patch_names(
            &options,
            Some(&ConnectionTarget::CdpUrl {
                url: "http://127.0.0.1:9222".to_string(),
            }),
            &config,
        );
        assert!(!external.iter().any(|patch| patch == "clean_chrome_args"));
    }

    #[test]
    fn user_agent_protocol_override_only_counts_completed_success() {
        assert!(user_agent_protocol_override_succeeded(&PageHookResult::<
            (),
            (),
        >::Completed(
            Ok(())
        )));
        assert!(!user_agent_protocol_override_succeeded(&PageHookResult::<
            (),
            (),
        >::Completed(
            Err(())
        )));
        assert!(!user_agent_protocol_override_succeeded(
            &PageHookResult::<(), ()>::TimedOut
        ));
    }

    #[test]
    fn invalidating_runtime_callback_hooks_preserves_non_callback_installation_state() {
        let mut state = PageHookInstallState {
            environment_metrics: true,
            touch_emulation: true,
            stealth_new_document: true,
            stealth_live: true,
            user_agent: true,
            self_probe: true,
            dom_enable: true,
            runtime_probe: true,
            frame_listener: true,
            document_listener: true,
            observatory: true,
            dialogs: true,
            network_rules: true,
            installation_recorded: true,
            ..PageHookInstallState::default()
        };

        state.invalidate_runtime_callback_hooks();

        assert!(state.environment_metrics);
        assert!(state.touch_emulation);
        assert!(state.stealth_new_document);
        assert!(state.stealth_live);
        assert!(state.user_agent);
        assert!(state.self_probe);
        assert!(state.dom_enable);
        assert!(!state.network_rules);
        assert!(state.installation_recorded);
        assert!(!state.runtime_probe);
        assert!(!state.frame_listener);
        assert!(!state.document_listener);
        assert!(!state.observatory);
        assert!(!state.dialogs);
        assert!(!state.complete());
    }

    #[test]
    fn tab_probe_failures_project_truthful_non_blank_sentinels() {
        assert_eq!(projected_tab_url(None), "about:rub-probe-unavailable");
        assert_eq!(projected_tab_title(None), "[probe unavailable]");
        assert_eq!(
            projected_tab_url(Some("about:blank".to_string())),
            "about:blank"
        );
        assert_eq!(projected_tab_title(Some(String::new())), "");
    }

    #[test]
    fn active_page_commit_ready_ignores_auxiliary_identity_hooks() {
        let state = PageHookInstallState {
            self_probe: true,
            dom_enable: true,
            runtime_probe: true,
            frame_listener: true,
            document_listener: true,
            observatory: true,
            dialogs: true,
            network_rules: true,
            ..PageHookInstallState::default()
        };

        assert!(active_page_runtime_commit_ready(&state, false));
        assert!(!state.complete());
    }
}
