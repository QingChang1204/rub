use super::*;
const SNAPSHOT_SETTLE_DELAY_MS: u64 = 100;
const SNAPSHOT_SETTLE_RETRIES: usize = 6;

pub(super) async fn settle_external_dom_fence(state: &Arc<SessionState>, command_name: &str) {
    state.clear_pending_external_dom_change();
    for attempt in 0..SNAPSHOT_SETTLE_RETRIES {
        tokio::time::sleep(std::time::Duration::from_millis(SNAPSHOT_SETTLE_DELAY_MS)).await;
        if !state.take_pending_external_dom_change() {
            return;
        }

        tracing::debug!(
            attempt,
            command = command_name,
            "Read-only transaction observed pending external DOM change during settle window; extending fence",
        );
    }
}

pub(super) async fn build_stable_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    limit: Option<u32>,
    a11y: bool,
    listeners: bool,
) -> Result<rub_core::model::Snapshot, RubError> {
    let explicit_frame_override = super::frame_scope::orchestration_frame_override(args);
    crate::runtime_refresh::refresh_live_frame_runtime(&router.browser, state).await;
    let selected_frame_id = if let Some(frame_id) = explicit_frame_override {
        super::frame_scope::ensure_request_frame_available(router, frame_id).await?;
        Some(frame_id.to_string())
    } else {
        let frame_runtime = state.frame_runtime().await;
        let selected_frame_id = state.selected_frame_id().await;
        if matches!(
            frame_runtime.status,
            rub_core::model::FrameContextStatus::Stale
        ) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Selected frame context is stale",
                serde_json::json!({
                    "frame_runtime": frame_runtime,
                    "selected_frame_id": selected_frame_id,
                }),
            ));
        }
        if matches!(
            frame_runtime.status,
            rub_core::model::FrameContextStatus::Degraded
        ) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Selected frame context is unavailable",
                serde_json::json!({
                    "frame_runtime": frame_runtime,
                    "selected_frame_id": selected_frame_id,
                }),
            ));
        }
        selected_frame_id
    };

    state.clear_pending_external_dom_change();

    for attempt in 0..SNAPSHOT_SETTLE_RETRIES {
        let snapshot = if listeners {
            router
                .browser
                .snapshot_with_listeners_for_frame(selected_frame_id.as_deref(), limit, a11y)
                .await?
        } else if a11y {
            router
                .browser
                .snapshot_with_a11y_for_frame(selected_frame_id.as_deref(), limit)
                .await?
        } else {
            router
                .browser
                .snapshot_for_frame(selected_frame_id.as_deref(), limit)
                .await?
        };

        tokio::time::sleep(std::time::Duration::from_millis(SNAPSHOT_SETTLE_DELAY_MS)).await;

        if !state.take_pending_external_dom_change() {
            return Ok(snapshot);
        }

        if attempt + 1 == SNAPSHOT_SETTLE_RETRIES {
            state.mark_pending_external_dom_change();
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Snapshot could not stabilize before publish fence",
                serde_json::json!({
                    "reason": "snapshot_unstable",
                    "snapshot_epoch": snapshot.dom_epoch,
                    "attempts": SNAPSHOT_SETTLE_RETRIES,
                    "selected_frame_id": selected_frame_id,
                }),
            ));
        }

        tracing::debug!(
            attempt,
            snapshot_epoch = snapshot.dom_epoch,
            "Snapshot publication observed pending external DOM change during settle window; rebuilding before publish",
        );
    }

    Err(RubError::Internal(
        "Snapshot settle loop exhausted without producing a stable snapshot".to_string(),
    ))
}
