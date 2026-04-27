use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{FrameInventoryEntry, TabInfo};
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
                .mark_runtime_state_probe_degraded(
                    sequence,
                    runtime_state_probe_degraded_reason(&error),
                )
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
                .mark_dialog_runtime_degraded(
                    0,
                    stable_surface_probe_degraded_reason(&error, "dialog_probe_failed"),
                )
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
                .mark_frame_runtime_degraded(stable_surface_probe_degraded_reason(
                    &error,
                    "frame_probe_failed",
                ))
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
    let effective_frame_id = match selected_frame_id.as_deref() {
        Some(frame_id) => match browser.list_frames().await {
            Ok(frames) => match resolve_storage_runtime_frame_id(frame_id, &frames) {
                Ok(frame_id) => Some(frame_id),
                Err(error) => {
                    state
                        .mark_storage_runtime_degraded(storage_runtime_degraded_reason(&error))
                        .await;
                    return;
                }
            },
            Err(error) => {
                let degraded_reason =
                    storage_runtime_frame_inventory_degraded_reason(frame_id, &error);
                state.mark_storage_runtime_degraded(degraded_reason).await;
                return;
            }
        },
        None => None,
    };
    match browser
        .storage_snapshot(effective_frame_id.as_deref(), None)
        .await
    {
        Ok(snapshot) => {
            state.set_storage_snapshot(snapshot).await;
        }
        Err(error) => {
            state
                .mark_storage_runtime_degraded(storage_runtime_degraded_reason(&error))
                .await;
        }
    }
}

fn runtime_state_probe_degraded_reason(error: &RubError) -> String {
    match error {
        RubError::Domain(envelope) => {
            if let Some(reason) = envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str())
            {
                return reason.to_string();
            }
            if envelope.code == ErrorCode::BrowserCrashed && envelope.message == "No active page" {
                return "page_unavailable".to_string();
            }
            "runtime_state_probe_failed".to_string()
        }
        _ => "runtime_state_probe_failed".to_string(),
    }
}

fn storage_runtime_frame_inventory_degraded_reason(frame_id: &str, error: &RubError) -> String {
    let _ = error;
    let _ = frame_id;
    "continuity_frame_inventory_unavailable".to_string()
}

fn resolve_storage_runtime_frame_id(
    frame_id: &str,
    frames: &[FrameInventoryEntry],
) -> Result<String, RubError> {
    frames
        .iter()
        .find(|entry| entry.frame.frame_id == frame_id)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Selected frame '{frame_id}' is no longer present in the current tab inventory"
                ),
                serde_json::json!({
                    "reason": "continuity_frame_unavailable",
                    "frame_id": frame_id,
                }),
            )
        })?;
    Ok(frame_id.to_string())
}

fn storage_runtime_degraded_reason(error: &RubError) -> String {
    match error {
        RubError::Domain(envelope) => envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| "storage_probe_failed".to_string()),
        _ => "storage_probe_failed".to_string(),
    }
}

fn stable_surface_probe_degraded_reason(error: &RubError, default_reason: &'static str) -> String {
    match error {
        RubError::Domain(envelope) => envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| default_reason.to_string()),
        _ => default_reason.to_string(),
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
                .mark_trigger_runtime_degraded(stable_surface_probe_degraded_reason(
                    &error,
                    "tab_probe_failed",
                ))
                .await;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_storage_runtime_frame_id, runtime_state_probe_degraded_reason,
        stable_surface_probe_degraded_reason, storage_runtime_degraded_reason,
    };
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::{FrameContextInfo, FrameInventoryEntry};

    #[test]
    fn runtime_state_probe_reason_prefers_stable_reason_over_display_string() {
        let error = RubError::domain_with_context(
            ErrorCode::TabNotFound,
            "projection ambiguous",
            serde_json::json!({
                "reason": "active_tab_authority_unavailable",
            }),
        );

        assert_eq!(
            runtime_state_probe_degraded_reason(&error),
            "active_tab_authority_unavailable"
        );
    }

    #[test]
    fn runtime_state_probe_reason_uses_surface_specific_fallback() {
        let error = RubError::Internal("cdp transport dropped".to_string());

        assert_eq!(
            runtime_state_probe_degraded_reason(&error),
            "runtime_state_probe_failed"
        );
    }

    #[test]
    fn resolve_storage_runtime_frame_id_rejects_missing_selected_frame() {
        let error = resolve_storage_runtime_frame_id("child", &[])
            .expect_err("missing selected frame must fail closed");
        assert_eq!(
            error
                .into_envelope()
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_frame_unavailable")
        );
    }

    #[test]
    fn resolve_storage_runtime_frame_id_accepts_frame_inventory_without_hit_test_authority() {
        let frames = vec![FrameInventoryEntry {
            index: 1,
            frame: FrameContextInfo {
                frame_id: "child".to_string(),
                name: Some("child".to_string()),
                parent_frame_id: Some("main".to_string()),
                target_id: None,
                url: Some("https://example.test/frame".to_string()),
                depth: 1,
                same_origin_accessible: Some(false),
            },
            is_current: true,
            is_primary: false,
        }];

        let frame_id = resolve_storage_runtime_frame_id("child", &frames)
            .expect("storage refresh only needs frame continuity; CDP checks execution context");
        assert_eq!(frame_id, "child");
    }

    #[test]
    fn storage_runtime_probe_reason_does_not_embed_error_display() {
        let error = RubError::Internal("opaque origin".to_string());

        assert_eq!(
            storage_runtime_degraded_reason(&error),
            "storage_probe_failed"
        );
    }

    #[test]
    fn generic_surface_probe_reason_prefers_machine_readable_context() {
        let error = RubError::domain_with_context(
            ErrorCode::SessionBusy,
            "dialog runtime unavailable",
            serde_json::json!({
                "reason": "dialog_listener_unavailable",
            }),
        );

        assert_eq!(
            stable_surface_probe_degraded_reason(&error, "dialog_probe_failed"),
            "dialog_listener_unavailable"
        );
    }

    #[test]
    fn generic_surface_probe_reason_falls_back_without_error_display() {
        let error = RubError::Internal("transport reset".to_string());

        assert_eq!(
            stable_surface_probe_degraded_reason(&error, "frame_probe_failed"),
            "frame_probe_failed"
        );
    }
}
