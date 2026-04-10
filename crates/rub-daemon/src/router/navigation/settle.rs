use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::TabInfo;

use crate::router::snapshot::{
    ExternalDomFenceOutcome, settle_external_dom_fence, sleep_full_settle_window,
};
use crate::router::{DaemonRouter, PendingExternalDomCommit, TransactionDeadline};
use crate::session::SessionState;

const NAVIGATION_PROJECTION_SETTLE_RETRIES: usize = 6;
const NAVIGATION_PROJECTION_SETTLE_DELAY_MS: u64 = 100;

pub(super) async fn settle_navigation_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    reason: &str,
    deadline: TransactionDeadline,
) -> PendingExternalDomCommit {
    state.clear_all_snapshots().await;
    state.select_frame(None).await;
    let pending_external_dom_commit =
        match settle_external_dom_fence(state, reason, Some(deadline)).await {
            ExternalDomFenceOutcome::Settled => PendingExternalDomCommit::Clear,
            ExternalDomFenceOutcome::IncompleteDueToDeadline
            | ExternalDomFenceOutcome::Unstable => PendingExternalDomCommit::Preserve,
        };

    for attempt in 0..NAVIGATION_PROJECTION_SETTLE_RETRIES {
        if deadline.remaining_duration().is_none() {
            tracing::debug!(
                reason,
                attempt,
                "settle_navigation_projection: deadline exhausted, stopping early"
            );
            return pending_external_dom_commit;
        }
        let tabs = router.browser.list_tabs().await.ok();
        if let Some(tabs) = tabs.as_ref() {
            state.adopt_interference_primary_context(tabs).await;
        }
        crate::runtime_refresh::refresh_live_frame_runtime(&router.browser, state).await;
        if active_tab_and_frame_runtime_converged(state, tabs.as_deref()).await {
            return pending_external_dom_commit;
        }
        if attempt + 1 < NAVIGATION_PROJECTION_SETTLE_RETRIES
            && !sleep_full_settle_window(Some(deadline), NAVIGATION_PROJECTION_SETTLE_DELAY_MS)
                .await
        {
            tracing::debug!(
                reason,
                attempt,
                "settle_navigation_projection: deadline too close for another full settle window, stopping early"
            );
            return pending_external_dom_commit;
        }
    }

    pending_external_dom_commit
}

pub(super) fn is_page_load_timeout(error: &RubError) -> bool {
    matches!(error, RubError::Domain(envelope) if envelope.code == ErrorCode::PageLoadTimeout)
}

async fn active_tab_and_frame_runtime_converged(
    state: &Arc<SessionState>,
    tabs: Option<&[TabInfo]>,
) -> bool {
    let Some(active_tab) = tabs.and_then(|tabs| tabs.iter().find(|tab| tab.active)) else {
        return false;
    };
    state
        .frame_runtime()
        .await
        .current_frame
        .is_some_and(|frame| {
            frame.target_id.as_deref() == Some(active_tab.target_id.as_str())
                && frame.url.as_deref() == Some(active_tab.url.as_str())
        })
}

pub(super) async fn active_tab_entity(
    router: &DaemonRouter,
) -> Result<serde_json::Value, RubError> {
    let tabs = router.browser.list_tabs().await?;
    let active_tab = tabs.iter().find(|tab| tab.active).ok_or_else(|| {
        RubError::Internal("navigation completed without an active tab".to_string())
    })?;
    Ok(crate::router::projection::tab_entity(active_tab))
}
