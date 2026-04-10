use std::future::Future;
use std::sync::Arc;
use tokio::time::Duration;

use super::super::*;
use crate::runtime_refresh::{
    refresh_live_frame_runtime, refresh_live_interference_state, refresh_live_runtime_state,
};
use crate::session::SessionState;
use rub_core::model::{
    DownloadEvent, InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
    InterferenceRuntimeInfo, InterferenceRuntimeStatus, RuntimeStateSnapshot,
};

const INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT: Duration = Duration::from_millis(250);
const INTERACTION_BROWSER_EVENT_QUIET_PERIOD: Duration = Duration::from_millis(20);

pub(super) struct InteractionObservationBaseline {
    pub(super) observatory_cursor: u64,
    pub(super) observatory_drop_count: u64,
    pub(super) request_cursor: u64,
    pub(super) network_request_drop_count: u64,
    pub(super) download_cursor: u64,
    pub(super) browser_event_cursor: u64,
    pub(super) runtime_before: Option<RuntimeStateSnapshot>,
    pub(super) interference_before: InterferenceRuntimeInfo,
}

struct InteractionTraceWindows {
    observatory_events: Vec<rub_core::model::RuntimeObservatoryEvent>,
    observatory_authoritative: bool,
    observatory_degraded_reason: Option<String>,
    network_requests: Vec<rub_core::model::NetworkRequestRecord>,
    network_authoritative: bool,
    network_degraded_reason: Option<String>,
}

pub(super) struct InteractionProjectionState {
    pub(super) runtime_after: RuntimeStateSnapshot,
    pub(super) frame_runtime: rub_core::model::FrameRuntimeInfo,
    pub(super) interference_after: InterferenceRuntimeInfo,
}

pub(super) async fn capture_interaction_baseline(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> InteractionObservationBaseline {
    if let Ok(tabs) = router.browser.list_tabs().await {
        state.prime_interference_baseline(&tabs).await;
    }
    InteractionObservationBaseline {
        observatory_cursor: state.observatory_cursor().await,
        observatory_drop_count: state.observatory().await.dropped_event_count,
        request_cursor: state.network_request_cursor().await,
        network_request_drop_count: state.network_request_drop_count().await,
        download_cursor: state.download_cursor().await,
        browser_event_cursor: state.browser_event_cursor(),
        runtime_before: crate::interaction_trace::probe_runtime_state(&router.browser).await,
        interference_before: state.interference_runtime().await,
    }
}

async fn capture_interaction_trace_windows(
    state: &Arc<SessionState>,
    baseline: &InteractionObservationBaseline,
) -> InteractionTraceWindows {
    let observatory_window = state
        .observatory_event_window_after(
            baseline.observatory_cursor,
            baseline.observatory_drop_count,
        )
        .await;
    let network_window = state
        .network_request_window_after(baseline.request_cursor, baseline.network_request_drop_count)
        .await;
    InteractionTraceWindows {
        observatory_events: observatory_window.events,
        observatory_authoritative: observatory_window.authoritative,
        observatory_degraded_reason: observatory_window.degraded_reason,
        network_requests: network_window.records,
        network_authoritative: network_window.authoritative,
        network_degraded_reason: network_window.degraded_reason,
    }
}

pub(super) async fn finalize_interaction_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    data: &mut serde_json::Value,
    outcome: &InteractionOutcome,
    baseline: &InteractionObservationBaseline,
) {
    let projection_state = collect_post_interaction_projection(state, || async {
        refresh_live_runtime_state(&router.browser, state).await;
        refresh_live_frame_runtime(&router.browser, state).await;
        let _ = refresh_live_interference_state(&router.browser, state).await;
        let interference_after = state.interference_runtime().await;
        if should_promote_primary_context(outcome, &interference_after)
            && let Ok(tabs) = router.browser.list_tabs().await
        {
            state.adopt_interference_primary_context(&tabs).await;
        }
    })
    .await;
    state
        .wait_for_browser_event_quiescence_since(
            baseline.browser_event_cursor,
            INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT,
            INTERACTION_BROWSER_EVENT_QUIET_PERIOD,
        )
        .await;
    let trace_windows = capture_interaction_trace_windows(state, baseline).await;
    let download_events: Vec<DownloadEvent> =
        state.download_events_after(baseline.download_cursor).await;
    attach_interaction_projection(
        data,
        outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &projection_state.frame_runtime,
            runtime_before: baseline.runtime_before.as_ref(),
            runtime_after: Some(&projection_state.runtime_after),
            interference_before: Some(&baseline.interference_before),
            interference_after: Some(&projection_state.interference_after),
            observatory_events: &trace_windows.observatory_events,
            observatory_authoritative: trace_windows.observatory_authoritative,
            observatory_degraded_reason: trace_windows.observatory_degraded_reason.as_deref(),
            network_requests: &trace_windows.network_requests,
            network_authoritative: trace_windows.network_authoritative,
            network_degraded_reason: trace_windows.network_degraded_reason.as_deref(),
            download_events: &download_events,
        },
    );
}

fn should_promote_primary_context(
    outcome: &InteractionOutcome,
    interference_after: &InterferenceRuntimeInfo,
) -> bool {
    matches!(
        interference_after.status,
        InterferenceRuntimeStatus::Inactive
    ) && matches!(
        outcome
            .confirmation
            .as_ref()
            .map(|confirmation| (confirmation.status, confirmation.kind)),
        Some((
            InteractionConfirmationStatus::Confirmed,
            Some(InteractionConfirmationKind::ContextChange)
        ))
    )
}

pub(super) async fn finalize_select_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    data: &mut serde_json::Value,
    outcome: &rub_core::model::SelectOutcome,
    baseline: &InteractionObservationBaseline,
) {
    let projection_state = collect_post_interaction_projection(state, || async {
        refresh_live_runtime_state(&router.browser, state).await;
        refresh_live_frame_runtime(&router.browser, state).await;
        let _ = refresh_live_interference_state(&router.browser, state).await;
    })
    .await;
    state
        .wait_for_browser_event_quiescence_since(
            baseline.browser_event_cursor,
            INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT,
            INTERACTION_BROWSER_EVENT_QUIET_PERIOD,
        )
        .await;
    let trace_windows = capture_interaction_trace_windows(state, baseline).await;
    let download_events = state.download_events_after(baseline.download_cursor).await;
    attach_select_projection(
        data,
        outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &projection_state.frame_runtime,
            runtime_before: baseline.runtime_before.as_ref(),
            runtime_after: Some(&projection_state.runtime_after),
            interference_before: Some(&baseline.interference_before),
            interference_after: Some(&projection_state.interference_after),
            observatory_events: &trace_windows.observatory_events,
            observatory_authoritative: trace_windows.observatory_authoritative,
            observatory_degraded_reason: trace_windows.observatory_degraded_reason.as_deref(),
            network_requests: &trace_windows.network_requests,
            network_authoritative: trace_windows.network_authoritative,
            network_degraded_reason: trace_windows.network_degraded_reason.as_deref(),
            download_events: &download_events,
        },
    );
}

pub(super) async fn collect_post_interaction_projection<F, Fut>(
    state: &Arc<SessionState>,
    refresh: F,
) -> InteractionProjectionState
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    refresh().await;
    InteractionProjectionState {
        runtime_after: state.runtime_state_snapshot().await,
        frame_runtime: state.frame_runtime().await,
        interference_after: state.interference_runtime().await,
    }
}
