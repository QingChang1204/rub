use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::model::TabInfo;
use rub_core::port::BrowserPort;

use crate::session::SessionState;

pub(crate) async fn refresh_live_runtime_state(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    let sequence = state.allocate_runtime_state_sequence();
    match browser.probe_runtime_state().await {
        Ok(runtime_state) => {
            state
                .publish_runtime_state_snapshot(sequence, runtime_state)
                .await;
        }
        Err(error) => {
            state
                .mark_runtime_state_probe_degraded(sequence, error.to_string())
                .await;
        }
    }
}

pub(crate) async fn refresh_live_dialog_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    match browser.dialog_runtime().await {
        Ok(runtime) => {
            state.set_dialog_projection(0, runtime).await;
        }
        Err(error) => {
            state
                .mark_dialog_runtime_degraded(0, format!("dialog_probe_failed:{error}"))
                .await;
        }
    }
}

pub(crate) async fn refresh_live_frame_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    match browser.list_frames().await {
        Ok(frames) => {
            state.apply_frame_inventory(&frames).await;
        }
        Err(error) => {
            state
                .mark_frame_runtime_degraded(format!("frame_probe_failed:{error}"))
                .await;
        }
    }
}

pub(crate) async fn refresh_takeover_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    let launch_policy = browser.launch_policy();
    state.refresh_takeover_runtime(&launch_policy).await;
}

pub(crate) async fn refresh_live_storage_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    let selected_frame_id = state.selected_frame_id().await;
    match browser
        .storage_snapshot(selected_frame_id.as_deref(), None)
        .await
    {
        Ok(snapshot) => {
            state.set_storage_snapshot(snapshot).await;
        }
        Err(error) => {
            state
                .mark_storage_runtime_degraded(format!("storage_probe_failed:{error}"))
                .await;
        }
    }
}

pub(crate) async fn refresh_live_trigger_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) -> Result<Vec<TabInfo>, RubError> {
    match browser.list_tabs().await {
        Ok(tabs) => {
            state.reconcile_trigger_runtime(&tabs).await;
            state.clear_trigger_runtime_degraded().await;
            Ok(tabs)
        }
        Err(error) => {
            state
                .mark_trigger_runtime_degraded(format!("tab_probe_failed:{error}"))
                .await;
            Err(error)
        }
    }
}
