use crate::runtime_refresh::refresh_live_frame_runtime;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::Snapshot;
use std::sync::Arc;

use super::super::snapshot::build_stable_snapshot;
use super::super::{DaemonRouter, TransactionDeadline};

pub(super) async fn load_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    prefer_a11y: bool,
) -> Result<Arc<Snapshot>, RubError> {
    if let Some(snapshot_id) = args.get("snapshot_id").and_then(|value| value.as_str()) {
        refresh_live_frame_runtime(&router.browser, state).await;
        let snapshot = state.get_snapshot(snapshot_id).await.ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!("Snapshot {snapshot_id} is unknown or evicted"),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "current_epoch": state.current_epoch(),
                }),
            )
        })?;

        let current_epoch = state.current_epoch();
        if snapshot.dom_epoch != current_epoch {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!(
                    "Snapshot {snapshot_id} is stale: snapshot epoch {} != current epoch {}",
                    snapshot.dom_epoch, current_epoch
                ),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "snapshot_epoch": snapshot.dom_epoch,
                    "current_epoch": current_epoch,
                }),
            ));
        }

        let frame_runtime = state.frame_runtime().await;
        if matches!(
            frame_runtime.status,
            rub_core::model::FrameContextStatus::Stale
        ) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!(
                    "Snapshot {snapshot_id} cannot be used because the selected frame context is stale"
                ),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "snapshot_frame_id": snapshot.frame_context.frame_id,
                    "frame_runtime": frame_runtime,
                }),
            ));
        }
        let current_frame_id = frame_runtime
            .current_frame
            .as_ref()
            .map(|frame| frame.frame_id.as_str());
        if current_frame_id != Some(snapshot.frame_context.frame_id.as_str()) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!(
                    "Snapshot {snapshot_id} belongs to frame '{}' but current frame context is '{}'",
                    snapshot.frame_context.frame_id,
                    current_frame_id.unwrap_or("unknown"),
                ),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "snapshot_frame_id": snapshot.frame_context.frame_id,
                    "current_frame_id": current_frame_id,
                    "frame_runtime": frame_runtime,
                }),
            ));
        }

        return Ok(snapshot);
    }

    let snapshot =
        build_stable_snapshot(router, args, state, deadline, Some(0), prefer_a11y, false).await?;
    let snapshot = state.cache_snapshot(snapshot).await;
    Ok(snapshot)
}
