use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::FrameInventoryEntry;
use rub_core::port::BrowserPort;

use crate::session::SessionState;

use super::DaemonRouter;

pub(super) fn orchestration_frame_override(args: &serde_json::Value) -> Option<&str> {
    args.get("_orchestration")
        .and_then(|value| value.get("frame_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) async fn effective_request_frame_id(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<Option<String>, RubError> {
    if let Some(frame_id) = orchestration_frame_override(args) {
        ensure_request_frame_available(router, frame_id).await?;
        return Ok(Some(frame_id.to_string()));
    }

    let selected_frame_id = state.selected_frame_id().await;
    if let Some(frame_id) = selected_frame_id.as_deref() {
        ensure_request_frame_available(router, frame_id).await?;
    }
    Ok(selected_frame_id)
}

pub(super) async fn explicit_or_top_frame_request_id(
    router: &DaemonRouter,
    args: &serde_json::Value,
) -> Result<Option<String>, RubError> {
    if let Some(frame_id) = orchestration_frame_override(args) {
        ensure_request_frame_available(router, frame_id).await?;
        return Ok(Some(frame_id.to_string()));
    }
    Ok(None)
}

pub(super) async fn ensure_request_frame_available(
    router: &DaemonRouter,
    frame_id: &str,
) -> Result<(), RubError> {
    let frames = router.browser.list_frames().await.map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::BrowserCrashed,
            format!("Unable to inspect frame inventory for orchestration frame override: {error}"),
            serde_json::json!({
                "reason": "continuity_frame_inventory_unavailable",
                "frame_id": frame_id,
            }),
        )
    })?;

    let entry = frames
        .iter()
        .find(|entry| entry.frame.frame_id == frame_id)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::BrowserCrashed,
                format!(
                    "Orchestration frame '{frame_id}' is not present in the current tab inventory"
                ),
                serde_json::json!({
                    "reason": "continuity_frame_unavailable",
                    "frame_id": frame_id,
                }),
            )
        })?;
    ensure_frame_switchable(entry)
}

pub(super) async fn ensure_tab_frame_available(
    browser: &Arc<dyn BrowserPort>,
    target_id: &str,
    frame_id: &str,
    role: &str,
) -> Result<(), RubError> {
    let frames = browser
        .list_frames_for_tab(target_id)
        .await
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::BrowserCrashed,
                format!("Unable to inspect frame inventory for trigger {role} tab: {error}"),
                serde_json::json!({
                    "reason": format!("trigger_{}_frame_inventory_unavailable", role),
                    "tab_target_id": target_id,
                    "frame_id": frame_id,
                }),
            )
        })?;

    let entry = frames
        .iter()
        .find(|entry| entry.frame.frame_id == frame_id)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("Trigger {role} frame '{frame_id}' is not present in tab '{target_id}'"),
                serde_json::json!({
                    "reason": format!("trigger_{}_frame_missing", role),
                    "tab_target_id": target_id,
                    "frame_id": frame_id,
                }),
            )
        })?;
    ensure_frame_switchable(entry).map_err(|error| {
        let envelope = error.into_envelope();
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Trigger {role} frame '{frame_id}' is not same-origin accessible for trigger execution"
            ),
            serde_json::json!({
                "reason": format!("trigger_{}_frame_unavailable", role),
                "tab_target_id": target_id,
                "frame_id": frame_id,
                "same_origin_accessible": entry.frame.same_origin_accessible,
                "index": entry.index,
                "cause": envelope.message,
            }),
        )
    })
}

fn ensure_frame_switchable(entry: &FrameInventoryEntry) -> Result<(), RubError> {
    if entry.is_primary || matches!(entry.frame.same_origin_accessible, Some(true)) {
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::BrowserCrashed,
        format!(
            "Orchestration frame '{}' is not same-origin accessible for frame-scoped execution",
            entry.frame.frame_id
        ),
        serde_json::json!({
            "reason": "continuity_frame_unavailable",
            "frame_id": entry.frame.frame_id,
            "same_origin_accessible": entry.frame.same_origin_accessible,
            "index": entry.index,
        }),
    ))
}
