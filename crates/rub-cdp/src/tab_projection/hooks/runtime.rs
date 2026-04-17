use chromiumoxide::Page;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use rub_core::error::{ErrorCode, RubError};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;

use super::install::PageHookInstaller;
use super::{EpochCallback, PageHookInstallState};
use crate::dialogs::DialogCallbacks;
use crate::identity_coverage::IdentityCoverageRegistry;
use crate::identity_policy::IdentityPolicy;
use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, is_current_generation};
use crate::network_rules::NetworkRuleRuntime;
use crate::request_correlation::RequestCorrelationRegistry;
use crate::runtime_observatory::ObservatoryCallbacks;
use crate::runtime_state::RuntimeStateCallbacks;
use crate::tab_projection::{CommittedTabProjection, LocalActiveTargetAuthority};

#[derive(Clone)]
pub(crate) struct ProjectionContext {
    pub(crate) browser: Arc<Browser>,
    pub(crate) page_hook_states: Arc<Mutex<HashMap<String, PageHookInstallState>>>,
    pub(crate) tab_projection_store: Arc<Mutex<CommittedTabProjection>>,
    pub(crate) local_active_target_authority: Arc<Mutex<Option<LocalActiveTargetAuthority>>>,
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
    pub(crate) authority_commit_in_progress: Arc<AtomicBool>,
    pub(crate) listener_generation: ListenerGeneration,
    pub(crate) listener_generation_rx: ListenerGenerationRx,
}

pub(crate) async fn sync_tabs_projection_with(
    context: &ProjectionContext,
    tab_projection_store: Arc<Mutex<CommittedTabProjection>>,
) -> Result<(), RubError> {
    let fresh_pages = context.browser.pages().await.map_err(|e| {
        RubError::domain(
            ErrorCode::BrowserCrashed,
            format!("Failed to enumerate browser tabs: {e}"),
        )
    })?;
    if !projection_generation_current(context) {
        return Ok(());
    }

    let projected = fresh_pages.into_iter().map(Arc::new).collect::<Vec<_>>();
    let live_target_ids = projected
        .iter()
        .map(|page| page.target_id().as_ref().to_string())
        .collect::<HashSet<_>>();
    if !projection_generation_current(context) {
        return Ok(());
    }
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

    let browser_truth_active_target =
        crate::tab_projection::resolve_active_target_from_browser_truth(&projected).await;
    let local_active_target_authority = context.local_active_target_authority.lock().await.clone();
    let active_target_resolution = crate::tab_projection::resolve_active_target_authority(
        projected.iter().map(|page| page.target_id()),
        browser_truth_active_target.as_ref(),
        local_active_target_authority.as_ref(),
    );
    drop(local_active_target_authority);
    let active_target = active_target_resolution.active_target;
    *context.local_active_target_authority.lock().await =
        active_target_resolution.next_local_active_target_authority;

    for page in &projected {
        if !projection_generation_current(context) {
            return Ok(());
        }
        let require_runtime_hooks = active_target
            .as_ref()
            .is_some_and(|target| page.target_id() == target);
        ensure_page_hooks(page.clone(), context, require_runtime_hooks).await?;
        if !projection_generation_current(context) {
            return Ok(());
        }
    }

    if !projection_generation_current(context) {
        return Ok(());
    }

    let committed_projection =
        commit_tabs_projection_state(&tab_projection_store, projected, active_target).await;
    if let Some(active_page) = committed_projection.current_page {
        probe_runtime_state_for_active_page(active_page, context).await;
    }
    Ok(())
}

async fn commit_tabs_projection_state(
    tab_projection_store: &Arc<Mutex<CommittedTabProjection>>,
    projected: Vec<Arc<Page>>,
    active_target: Option<TargetId>,
) -> CommittedTabProjection {
    let mut projection_store = tab_projection_store.lock().await;
    let previous_continuity_target_id = projection_store.continuity_target_id();
    let committed_projection = CommittedTabProjection::from_projected_pages(
        projected,
        active_target,
        previous_continuity_target_id.as_ref(),
    );
    *projection_store = committed_projection.clone();
    committed_projection
}

async fn page_has_active_target(
    page: &Arc<Page>,
    tab_projection_store: &Arc<Mutex<CommittedTabProjection>>,
) -> bool {
    tab_projection_store
        .lock()
        .await
        .active_target_id
        .as_ref()
        .is_some_and(|target| page.target_id() == target)
}

pub(super) async fn probe_runtime_state_for_active_page(
    page: Arc<Page>,
    context: &ProjectionContext,
) {
    if !projection_generation_current(context) {
        return;
    }
    if !page_has_active_target(&page, &context.tab_projection_store).await {
        return;
    }

    let callbacks = context.runtime_state_callbacks.lock().await.clone();
    if callbacks.is_empty() {
        return;
    }
    if projection_authority_commit_in_progress(context) {
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
    if !projection_generation_current(context) {
        return;
    }
    if !page_has_active_target(&page, &context.tab_projection_store).await {
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

pub(super) async fn refresh_identity_self_probe(page: &Arc<Page>, context: &ProjectionContext) {
    if !projection_generation_current(context) {
        return;
    }
    if !context.identity_policy.stealth_enabled() {
        return;
    }

    let probe =
        crate::identity_probe::run_identity_self_probe(page, &context.identity_policy).await;
    if !projection_generation_current(context) {
        return;
    }
    context
        .identity_coverage
        .lock()
        .await
        .record_self_probe(probe);
}

pub(super) fn projection_generation_current(context: &ProjectionContext) -> bool {
    is_current_generation(&context.listener_generation_rx, context.listener_generation)
}

pub(super) fn projection_authority_commit_in_progress(context: &ProjectionContext) -> bool {
    context.authority_commit_in_progress.load(Ordering::SeqCst)
}
