use std::future::Future;
use std::sync::Arc;
use tokio::time::Duration;

use super::super::*;
use crate::runtime_refresh::{
    InterferenceRefreshIntent, refresh_live_frame_runtime, refresh_live_interference_state,
    refresh_live_runtime_state,
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
    pub(super) observatory_ingress_drop_count: u64,
    pub(super) request_cursor: u64,
    pub(super) network_request_ingress_drop_count: u64,
    pub(super) download_cursor: u64,
    pub(super) download_ingress_drop_count: u64,
    pub(super) download_degraded_reason_before: Option<String>,
    pub(super) browser_event_cursor: u64,
    pub(super) runtime_before: Option<RuntimeStateSnapshot>,
    pub(super) interference_before: InterferenceRuntimeInfo,
}

pub(super) struct InteractionObservationFence {
    pub(super) observatory_cursor: u64,
    pub(super) observatory_ingress_drop_count: u64,
    pub(super) request_cursor: u64,
    pub(super) network_request_ingress_drop_count: u64,
    pub(super) download_cursor: u64,
    pub(super) download_ingress_drop_count: u64,
    pub(super) download_degraded_reason_after: Option<String>,
    pub(super) browser_event_cursor: u64,
}

pub(super) struct InteractionTraceWindows {
    pub(super) observatory_events: Vec<rub_core::model::RuntimeObservatoryEvent>,
    pub(super) observatory_authoritative: bool,
    pub(super) observatory_degraded_reason: Option<String>,
    pub(super) network_requests: Vec<rub_core::model::NetworkRequestRecord>,
    pub(super) network_authoritative: bool,
    pub(super) network_degraded_reason: Option<String>,
    pub(super) download_events: Vec<DownloadEvent>,
    pub(super) download_authoritative: bool,
    pub(super) download_degraded_reason: Option<String>,
}

pub(super) struct StableInteractionProjection {
    pub(super) projection_state: InteractionProjectionState,
    pub(super) trace_windows: InteractionTraceWindows,
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
    let download_runtime = state.download_runtime().await;
    InteractionObservationBaseline {
        observatory_cursor: state.observatory_cursor().await,
        observatory_ingress_drop_count: state.observatory_ingress_drop_count(),
        request_cursor: state.network_request_cursor().await,
        network_request_ingress_drop_count: state.network_request_ingress_drop_count(),
        download_cursor: state.download_cursor().await,
        download_ingress_drop_count: state.download_event_ingress_drop_count(),
        download_degraded_reason_before: download_runtime.degraded_reason,
        browser_event_cursor: state.browser_event_cursor(),
        runtime_before: crate::interaction_trace::probe_runtime_state(&router.browser).await,
        interference_before: state.interference_runtime().await,
    }
}

async fn capture_interaction_observation_fence(
    state: &Arc<SessionState>,
) -> InteractionObservationFence {
    let download_runtime = state.download_runtime().await;
    InteractionObservationFence {
        observatory_cursor: state.observatory_cursor().await,
        observatory_ingress_drop_count: state.observatory_ingress_drop_count(),
        request_cursor: state.network_request_cursor().await,
        network_request_ingress_drop_count: state.network_request_ingress_drop_count(),
        download_cursor: state.download_cursor().await,
        download_ingress_drop_count: state.download_event_ingress_drop_count(),
        download_degraded_reason_after: download_runtime.degraded_reason,
        browser_event_cursor: state.browser_event_cursor(),
    }
}

pub(super) async fn capture_interaction_trace_windows(
    state: &Arc<SessionState>,
    baseline: &InteractionObservationBaseline,
    fence: &InteractionObservationFence,
) -> InteractionTraceWindows {
    let observatory_window = state
        .observatory_event_window_between(
            baseline.observatory_cursor,
            fence.observatory_cursor,
            baseline.observatory_ingress_drop_count,
            fence.observatory_ingress_drop_count,
        )
        .await;
    let network_window = state
        .network_request_window_between(
            baseline.request_cursor,
            fence.request_cursor,
            baseline.network_request_ingress_drop_count,
            fence.network_request_ingress_drop_count,
        )
        .await;
    let download_window = state
        .download_event_window_between(
            baseline.download_cursor,
            fence.download_cursor,
            baseline.download_ingress_drop_count,
            fence.download_ingress_drop_count,
            baseline.download_degraded_reason_before.as_deref(),
            fence.download_degraded_reason_after.as_deref(),
        )
        .await;
    InteractionTraceWindows {
        observatory_events: observatory_window.events,
        observatory_authoritative: observatory_window.authoritative,
        observatory_degraded_reason: observatory_window.degraded_reason,
        network_requests: network_window.records,
        network_authoritative: network_window.authoritative,
        network_degraded_reason: network_window.degraded_reason,
        download_events: download_window.events,
        download_authoritative: download_window.authoritative,
        download_degraded_reason: download_window.degraded_reason,
    }
}

pub(super) async fn finalize_interaction_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    data: &mut serde_json::Value,
    outcome: &InteractionOutcome,
    baseline: &InteractionObservationBaseline,
) {
    let stable_projection = collect_stable_post_interaction_projection(state, baseline, || async {
        refresh_live_runtime_state(&router.browser, state).await;
        refresh_live_frame_runtime(&router.browser, state).await;
        let _ = refresh_live_interference_state(
            &router.browser,
            state,
            InterferenceRefreshIntent::PolicyDriven,
        )
        .await;
        let interference_after = state.interference_runtime().await;
        if should_promote_primary_context(outcome, &interference_after)
            && let Ok(tabs) = router.browser.list_tabs().await
        {
            state.adopt_interference_primary_context(&tabs).await;
        }
    })
    .await;
    attach_interaction_projection(
        data,
        outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &stable_projection.projection_state.frame_runtime,
            runtime_before: baseline.runtime_before.as_ref(),
            runtime_after: Some(&stable_projection.projection_state.runtime_after),
            interference_before: Some(&baseline.interference_before),
            interference_after: Some(&stable_projection.projection_state.interference_after),
            observatory_events: &stable_projection.trace_windows.observatory_events,
            observatory_authoritative: stable_projection.trace_windows.observatory_authoritative,
            observatory_degraded_reason: stable_projection
                .trace_windows
                .observatory_degraded_reason
                .as_deref(),
            network_requests: &stable_projection.trace_windows.network_requests,
            network_authoritative: stable_projection.trace_windows.network_authoritative,
            network_degraded_reason: stable_projection
                .trace_windows
                .network_degraded_reason
                .as_deref(),
            download_events: &stable_projection.trace_windows.download_events,
            download_authoritative: stable_projection.trace_windows.download_authoritative,
            download_degraded_reason: stable_projection
                .trace_windows
                .download_degraded_reason
                .as_deref(),
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
    let stable_projection = collect_stable_post_interaction_projection(state, baseline, || async {
        refresh_live_runtime_state(&router.browser, state).await;
        refresh_live_frame_runtime(&router.browser, state).await;
        let _ = refresh_live_interference_state(
            &router.browser,
            state,
            InterferenceRefreshIntent::PolicyDriven,
        )
        .await;
    })
    .await;
    attach_select_projection(
        data,
        outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &stable_projection.projection_state.frame_runtime,
            runtime_before: baseline.runtime_before.as_ref(),
            runtime_after: Some(&stable_projection.projection_state.runtime_after),
            interference_before: Some(&baseline.interference_before),
            interference_after: Some(&stable_projection.projection_state.interference_after),
            observatory_events: &stable_projection.trace_windows.observatory_events,
            observatory_authoritative: stable_projection.trace_windows.observatory_authoritative,
            observatory_degraded_reason: stable_projection
                .trace_windows
                .observatory_degraded_reason
                .as_deref(),
            network_requests: &stable_projection.trace_windows.network_requests,
            network_authoritative: stable_projection.trace_windows.network_authoritative,
            network_degraded_reason: stable_projection
                .trace_windows
                .network_degraded_reason
                .as_deref(),
            download_events: &stable_projection.trace_windows.download_events,
            download_authoritative: stable_projection.trace_windows.download_authoritative,
            download_degraded_reason: stable_projection
                .trace_windows
                .download_degraded_reason
                .as_deref(),
        },
    );
}

pub(super) async fn collect_stable_post_interaction_projection<F, Fut>(
    state: &Arc<SessionState>,
    baseline: &InteractionObservationBaseline,
    refresh: F,
) -> StableInteractionProjection
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    state
        .wait_for_browser_event_quiescence_since(
            baseline.browser_event_cursor,
            INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT,
            INTERACTION_BROWSER_EVENT_QUIET_PERIOD,
        )
        .await;
    refresh().await;
    let mut fence_cursor = state.browser_event_cursor();
    loop {
        state
            .wait_for_browser_event_quiescence_since(
                fence_cursor,
                INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT,
                INTERACTION_BROWSER_EVENT_QUIET_PERIOD,
            )
            .await;
        let fence = capture_interaction_observation_fence(state).await;
        let projection_state = read_post_interaction_projection(state).await;
        if state.browser_event_cursor() <= fence.browser_event_cursor {
            let trace_windows = capture_interaction_trace_windows(state, baseline, &fence).await;
            return StableInteractionProjection {
                projection_state,
                trace_windows,
            };
        }
        fence_cursor = fence.browser_event_cursor;
    }
}

async fn read_post_interaction_projection(state: &Arc<SessionState>) -> InteractionProjectionState {
    InteractionProjectionState {
        runtime_after: state.runtime_state_snapshot().await,
        frame_runtime: state.frame_runtime().await,
        interference_after: state.interference_runtime().await,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn collect_post_interaction_projection<F, Fut>(
    state: &Arc<SessionState>,
    refresh: F,
) -> InteractionProjectionState
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    refresh().await;
    read_post_interaction_projection(state).await
}
