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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActiveTabProjection {
    pub tab: Option<serde_json::Value>,
    pub degraded_reason: Option<&'static str>,
}

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
    let Some(active_tab) = tabs.and_then(|tabs| {
        tabs.iter()
            .find(|tab| tab.active && tab.page_url_authoritative())
    }) else {
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

pub(super) async fn active_tab_projection(router: &DaemonRouter) -> ActiveTabProjection {
    match router.browser.list_tabs().await {
        Ok(tabs) => active_tab_projection_from_tabs(&tabs),
        Err(_) => degraded_active_tab_projection("active_tab_probe_failed"),
    }
}

pub(super) fn active_tab_projection_from_tabs(tabs: &[TabInfo]) -> ActiveTabProjection {
    match tabs.iter().find(|tab| tab.active) {
        Some(active_tab) if !active_tab.page_url_authoritative() => {
            degraded_active_tab_projection("active_tab_probe_failed")
        }
        Some(active_tab) => ActiveTabProjection {
            tab: Some(crate::router::projection::tab_entity(active_tab)),
            degraded_reason: None,
        },
        None => degraded_active_tab_projection("active_tab_unavailable"),
    }
}

fn degraded_active_tab_projection(reason: &'static str) -> ActiveTabProjection {
    ActiveTabProjection {
        tab: None,
        degraded_reason: Some(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::{active_tab_projection_from_tabs, degraded_active_tab_projection};
    use rub_core::model::TabInfo;

    fn tab(index: u32, target_id: &str, active: bool) -> TabInfo {
        TabInfo {
            index,
            target_id: target_id.to_string(),
            url: format!("https://example.com/{index}"),
            title: format!("Tab {index}"),
            active,
            active_authority: None,
            degraded_reason: None,
        }
    }

    #[test]
    fn active_tab_projection_from_tabs_returns_active_tab_without_degradation() {
        let projection =
            active_tab_projection_from_tabs(&[tab(0, "tab-a", false), tab(1, "tab-b", true)]);
        assert!(projection.degraded_reason.is_none());
        assert_eq!(
            projection
                .tab
                .as_ref()
                .and_then(|tab| tab.get("target_id"))
                .and_then(|value| value.as_str()),
            Some("tab-b")
        );
    }

    #[test]
    fn active_tab_projection_from_tabs_degrades_when_no_active_tab_is_reported() {
        let projection = active_tab_projection_from_tabs(&[tab(0, "tab-a", false)]);
        assert!(projection.tab.is_none());
        assert_eq!(projection.degraded_reason, Some("active_tab_unavailable"));
    }

    #[test]
    fn active_tab_projection_preserves_active_tab_when_only_title_degraded() {
        let mut active = tab(0, "tab-a", true);
        active.title = String::new();
        active.degraded_reason = Some("tab_title_probe_failed".to_string());

        let projection = active_tab_projection_from_tabs(&[active]);
        assert!(projection.degraded_reason.is_none());
        let tab = projection
            .tab
            .expect("URL-authoritative tab should project");
        assert_eq!(tab["url"], "https://example.com/0");
        assert_eq!(tab["degraded_reason"], "tab_title_probe_failed");
    }

    #[test]
    fn active_tab_projection_degrades_when_url_probe_failed() {
        let mut active = tab(0, "tab-a", true);
        active.url = String::new();
        active.degraded_reason = Some("tab_url_probe_failed".to_string());

        let projection = active_tab_projection_from_tabs(&[active]);
        assert!(projection.tab.is_none());
        assert_eq!(projection.degraded_reason, Some("active_tab_probe_failed"));
    }

    #[test]
    fn degraded_active_tab_projection_clears_tab_payload() {
        let projection = degraded_active_tab_projection("active_tab_probe_failed");
        assert!(projection.tab.is_none());
        assert_eq!(projection.degraded_reason, Some("active_tab_probe_failed"));
    }
}
