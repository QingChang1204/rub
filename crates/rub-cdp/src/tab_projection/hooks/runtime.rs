use chromiumoxide::Page;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::RuntimeStateSnapshot;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;

use super::install::PageHookInstaller;
use super::{
    BACKGROUND_DIALOG_HOOK_MASK, BACKGROUND_NETWORK_RULES_HOOK_MASK,
    BACKGROUND_OBSERVATORY_HOOK_MASK, CRITICAL_RUNTIME_HOOKS_MASK, EpochCallback,
    PageHookInstallState,
};
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
    pub(crate) authority_release_in_progress: Arc<AtomicBool>,
    pub(crate) runtime_callback_reconfigure_in_progress: Arc<AtomicBool>,
    pub(crate) listener_generation: ListenerGeneration,
    pub(crate) listener_generation_rx: ListenerGenerationRx,
    #[cfg(test)]
    pub(crate) force_required_page_hook_install_failure: Arc<AtomicBool>,
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
    let browser_truth_active_target =
        crate::tab_projection::resolve_active_target_from_browser_truth(&projected).await;

    let Some(staged_projection) = stage_tabs_projection_state_if_current(
        context,
        &tab_projection_store,
        projected,
        live_target_ids,
        browser_truth_active_target,
    )
    .await
    else {
        return Ok(());
    };
    let active_target = staged_projection
        .committed_projection
        .active_target_id
        .clone();

    for page in &staged_projection.committed_projection.pages {
        if !projection_generation_current(context) {
            return Ok(());
        }
        let required_runtime_hook_mask =
            required_runtime_hook_mask_for_page(page, active_target.as_ref(), context).await;
        ensure_page_hooks(page.clone(), context, required_runtime_hook_mask).await?;
        if !projection_generation_current(context) {
            return Ok(());
        }
    }

    if !projection_generation_current(context) {
        return Ok(());
    }

    if !staged_projection_still_current(context, &staged_projection).await? {
        return Ok(());
    }

    let Some(committed_projection) = commit_staged_tabs_projection_state_if_current(
        context,
        &tab_projection_store,
        staged_projection,
    )
    .await
    else {
        return Ok(());
    };
    let live_target_ids = committed_projection
        .pages
        .iter()
        .map(|page| page.target_id().as_ref().to_string())
        .collect::<HashSet<_>>();
    prune_stale_runtime_hook_state(context, &live_target_ids).await;
    let stale_dialog_projection = crate::dialogs::clear_stale_pending_dialog_for_live_targets_if(
        &context.dialog_runtime,
        &live_target_ids,
        || {
            projection_generation_current(context)
                && !projection_runtime_callback_commit_in_progress(context)
        },
    )
    .await;
    let callback = if stale_dialog_projection.is_some()
        && !projection_runtime_callback_commit_in_progress(context)
    {
        context.dialog_callbacks.lock().await.on_runtime.clone()
    } else {
        None
    };
    let callback_still_allowed =
        callback.is_some() && !projection_runtime_callback_commit_in_progress(context);
    if let (Some(runtime_projection), Some(callback), true) =
        (stale_dialog_projection, callback, callback_still_allowed)
    {
        callback(crate::dialogs::DialogRuntimeUpdate {
            generation: context.listener_generation,
            runtime: runtime_projection,
        });
    }

    if let Some(active_page) = committed_projection.current_page {
        probe_runtime_state_for_active_page(active_page, context).await;
    }
    Ok(())
}

struct StagedTabsProjectionCommit {
    committed_projection: CommittedTabProjection,
    next_local_active_target_authority: Option<LocalActiveTargetAuthority>,
    live_target_ids: HashSet<String>,
}

async fn stage_tabs_projection_state_if_current(
    context: &ProjectionContext,
    tab_projection_store: &Arc<Mutex<CommittedTabProjection>>,
    projected: Vec<Arc<Page>>,
    live_target_ids: HashSet<String>,
    browser_truth_active_target: Option<TargetId>,
) -> Option<StagedTabsProjectionCommit> {
    let projection_store = tab_projection_store.lock().await;
    let local_active_target_authority = context.local_active_target_authority.lock().await;
    if !projection_generation_current(context) {
        return None;
    }
    let active_target_resolution = crate::tab_projection::resolve_active_target_authority(
        projected.iter().map(|page| page.target_id()),
        browser_truth_active_target.as_ref(),
        local_active_target_authority.as_ref(),
    );
    let previous_continuity_target_id = projection_store.continuity_target_id();
    let committed_projection = CommittedTabProjection::from_projected_pages(
        projected,
        active_target_resolution.active_target,
        active_target_resolution.active_target_authority,
        previous_continuity_target_id.as_ref(),
    );
    Some(StagedTabsProjectionCommit {
        committed_projection,
        next_local_active_target_authority: active_target_resolution
            .next_local_active_target_authority,
        live_target_ids,
    })
}

async fn staged_projection_still_current(
    context: &ProjectionContext,
    staged: &StagedTabsProjectionCommit,
) -> Result<bool, RubError> {
    let fresh_pages = context.browser.pages().await.map_err(|e| {
        RubError::domain(
            ErrorCode::BrowserCrashed,
            format!("Failed to enumerate browser tabs: {e}"),
        )
    })?;
    if !projection_generation_current(context) {
        return Ok(false);
    }
    let refreshed_live_target_ids = fresh_pages
        .iter()
        .map(|page| page.target_id().as_ref().to_string())
        .collect::<HashSet<_>>();
    if refreshed_live_target_ids != staged.live_target_ids {
        return Ok(false);
    }
    let refreshed_pages = fresh_pages.into_iter().map(Arc::new).collect::<Vec<_>>();
    let refreshed_browser_truth =
        crate::tab_projection::resolve_active_target_from_browser_truth(&refreshed_pages).await;
    let local_active_target_authority = context.local_active_target_authority.lock().await.clone();
    let refreshed_resolution = crate::tab_projection::resolve_active_target_authority(
        refreshed_pages.iter().map(|page| page.target_id()),
        refreshed_browser_truth.as_ref(),
        local_active_target_authority.as_ref(),
    );
    Ok(
        refreshed_resolution.active_target == staged.committed_projection.active_target_id
            && refreshed_resolution.active_target_authority
                == staged.committed_projection.active_target_authority,
    )
}

async fn commit_staged_tabs_projection_state_if_current(
    context: &ProjectionContext,
    tab_projection_store: &Arc<Mutex<CommittedTabProjection>>,
    staged: StagedTabsProjectionCommit,
) -> Option<CommittedTabProjection> {
    let mut projection_store = tab_projection_store.lock().await;
    let mut local_active_target_authority = context.local_active_target_authority.lock().await;
    if !projection_generation_current(context) {
        return None;
    }
    *projection_store = staged.committed_projection.clone();
    *local_active_target_authority = staged.next_local_active_target_authority;
    Some(staged.committed_projection)
}

async fn prune_stale_runtime_hook_state(
    context: &ProjectionContext,
    live_target_ids: &HashSet<String>,
) {
    let mut hook_states = context.page_hook_states.lock().await;
    if !projection_generation_current(context) {
        return;
    }
    hook_states.retain(|target_id, _| live_target_ids.contains(target_id));
    drop(hook_states);

    let mut registries = context.observatory_pending_registries.lock().await;
    if !projection_generation_current(context) {
        return;
    }
    registries.retain(|target_id, _| live_target_ids.contains(target_id));
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

async fn page_still_has_active_target(
    page: &Arc<Page>,
    active_target_id: &str,
    tab_projection_store: &Arc<Mutex<CommittedTabProjection>>,
) -> bool {
    tab_projection_store
        .lock()
        .await
        .active_target_id
        .as_ref()
        .is_some_and(|target| page.target_id() == target && target.as_ref() == active_target_id)
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
    let active_target_id = page.target_id().as_ref().to_string();

    let callbacks = context.runtime_state_callbacks.lock().await.clone();
    if callbacks.is_empty() {
        return;
    }
    if projection_runtime_callback_commit_in_progress(context) {
        return;
    }

    let snapshot = crate::runtime_state::capture_runtime_state(&page).await;
    if !projection_generation_current(context) {
        return;
    }
    if projection_runtime_callback_commit_in_progress(context) {
        return;
    }
    if !page_still_has_active_target(&page, &active_target_id, &context.tab_projection_store).await
    {
        return;
    }

    let callbacks = context.runtime_state_callbacks.lock().await.clone();
    if projection_runtime_callback_commit_in_progress(context) {
        return;
    }
    let Some(allocate_sequence) = callbacks.allocate_sequence.clone() else {
        return;
    };
    let Some(on_snapshot) = callbacks.on_snapshot.clone() else {
        return;
    };
    let on_snapshot = guard_runtime_state_snapshot_callback(
        on_snapshot,
        context.authority_commit_in_progress.clone(),
        context.runtime_callback_reconfigure_in_progress.clone(),
    );

    if projection_runtime_callback_commit_in_progress(context) {
        return;
    }
    let sequence = allocate_sequence();
    on_snapshot(
        sequence,
        context.listener_generation,
        Some(active_target_id),
        snapshot,
    );
}

fn guard_runtime_state_snapshot_callback(
    callback: Arc<dyn Fn(u64, u64, Option<String>, RuntimeStateSnapshot) + Send + Sync>,
    authority_commit_in_progress: Arc<AtomicBool>,
    runtime_callback_reconfigure_in_progress: Arc<AtomicBool>,
) -> Arc<dyn Fn(u64, u64, Option<String>, RuntimeStateSnapshot) + Send + Sync> {
    Arc::new(
        move |sequence, listener_generation, active_target_id, snapshot| {
            if authority_commit_in_progress.load(Ordering::SeqCst)
                || runtime_callback_reconfigure_in_progress.load(Ordering::SeqCst)
            {
                return;
            }
            callback(sequence, listener_generation, active_target_id, snapshot);
        },
    )
}

pub(crate) async fn replay_runtime_state_for_committed_active_page(context: &ProjectionContext) {
    let active_page = context
        .tab_projection_store
        .lock()
        .await
        .current_page
        .clone();
    if let Some(page) = active_page {
        probe_runtime_state_for_active_page(page, context).await;
    }
}

pub(crate) async fn ensure_page_hooks(
    page: Arc<Page>,
    context: &ProjectionContext,
    required_runtime_hook_mask: u16,
) -> Result<(), RubError> {
    #[cfg(test)]
    if context
        .force_required_page_hook_install_failure
        .load(Ordering::SeqCst)
    {
        return Err(RubError::Internal(
            "forced required page hook install failure".to_string(),
        ));
    }
    let Some(installer) =
        PageHookInstaller::begin(page, context, required_runtime_hook_mask).await?
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

pub(super) fn projection_runtime_callback_commit_in_progress(context: &ProjectionContext) -> bool {
    projection_authority_commit_in_progress(context)
        || context
            .runtime_callback_reconfigure_in_progress
            .load(Ordering::SeqCst)
}

fn background_required_runtime_hook_mask(
    observatory_callbacks_configured: bool,
    has_active_network_rules: bool,
) -> u16 {
    let mut mask = BACKGROUND_DIALOG_HOOK_MASK;
    if observatory_callbacks_configured {
        mask |= BACKGROUND_OBSERVATORY_HOOK_MASK;
    }
    if has_active_network_rules {
        mask |= BACKGROUND_NETWORK_RULES_HOOK_MASK;
    }
    mask
}

async fn required_runtime_hook_mask_for_page(
    page: &Arc<Page>,
    active_target: Option<&TargetId>,
    context: &ProjectionContext,
) -> u16 {
    if active_target.is_some_and(|target| page.target_id() == target) {
        return CRITICAL_RUNTIME_HOOKS_MASK;
    }

    let observatory_callbacks_configured = !context.observatory_callbacks.lock().await.is_empty();
    let has_active_network_rules = context.network_rule_runtime.read().await.has_active_rules();
    background_required_runtime_hook_mask(
        observatory_callbacks_configured,
        has_active_network_rules,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        BACKGROUND_DIALOG_HOOK_MASK, BACKGROUND_NETWORK_RULES_HOOK_MASK,
        BACKGROUND_OBSERVATORY_HOOK_MASK, CRITICAL_RUNTIME_HOOKS_MASK,
        background_required_runtime_hook_mask, guard_runtime_state_snapshot_callback,
    };
    use rub_core::model::{ReadinessInfo, RuntimeStateSnapshot, StateInspectorInfo};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    #[test]
    fn background_required_runtime_hook_mask_always_keeps_dialog_authority() {
        assert_eq!(
            background_required_runtime_hook_mask(false, false),
            BACKGROUND_DIALOG_HOOK_MASK
        );
    }

    #[test]
    fn background_required_runtime_hook_mask_adds_observatory_and_network_rule_authority() {
        assert_eq!(
            background_required_runtime_hook_mask(true, false),
            BACKGROUND_DIALOG_HOOK_MASK | BACKGROUND_OBSERVATORY_HOOK_MASK
        );
        assert_eq!(
            background_required_runtime_hook_mask(false, true),
            BACKGROUND_DIALOG_HOOK_MASK | BACKGROUND_NETWORK_RULES_HOOK_MASK
        );
        assert_eq!(
            background_required_runtime_hook_mask(true, true),
            BACKGROUND_DIALOG_HOOK_MASK
                | BACKGROUND_OBSERVATORY_HOOK_MASK
                | BACKGROUND_NETWORK_RULES_HOOK_MASK
        );
        assert_ne!(
            background_required_runtime_hook_mask(true, true),
            CRITICAL_RUNTIME_HOOKS_MASK,
            "background pages should not inherit the active-page runtime probe fence"
        );
    }

    #[test]
    fn runtime_state_snapshot_callback_guard_suppresses_in_flight_reconfigure_delivery() {
        let authority_commit_in_progress = Arc::new(AtomicBool::new(false));
        let runtime_callback_reconfigure_in_progress = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicU64::new(0));
        let callback = {
            let delivered = delivered.clone();
            Arc::new(
                move |_sequence, _listener_generation, _active_target_id, _snapshot| {
                    delivered.fetch_add(1, Ordering::SeqCst);
                },
            )
        };
        let guarded = guard_runtime_state_snapshot_callback(
            callback,
            authority_commit_in_progress,
            runtime_callback_reconfigure_in_progress.clone(),
        );
        let snapshot = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo::default(),
            readiness_state: ReadinessInfo::default(),
        };

        runtime_callback_reconfigure_in_progress.store(true, Ordering::SeqCst);
        guarded(1, 7, Some("active-tab".to_string()), snapshot.clone());
        assert_eq!(delivered.load(Ordering::SeqCst), 0);

        runtime_callback_reconfigure_in_progress.store(false, Ordering::SeqCst);
        guarded(2, 7, Some("active-tab".to_string()), snapshot);
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
    }
}
