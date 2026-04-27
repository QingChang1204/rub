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

pub(crate) fn semantic_replay_orchestration_metadata(
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    orchestration_frame_override(args).map(|frame_id| serde_json::json!({ "frame_id": frame_id }))
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

pub(super) async fn effective_interaction_frame_id(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<Option<String>, RubError> {
    if let Some(frame_id) = orchestration_frame_override(args) {
        ensure_request_frame_switchable(router, frame_id).await?;
        return Ok(Some(frame_id.to_string()));
    }

    let selected_frame_id = state.selected_frame_id().await;
    if let Some(frame_id) = selected_frame_id.as_deref() {
        ensure_request_frame_switchable(router, frame_id).await?;
    }
    Ok(selected_frame_id)
}

pub(super) async fn ensure_request_frame_available(
    router: &DaemonRouter,
    frame_id: &str,
) -> Result<(), RubError> {
    let _entry = request_frame_entry(router, frame_id).await?;
    Ok(())
}

pub(super) async fn ensure_request_frame_switchable(
    router: &DaemonRouter,
    frame_id: &str,
) -> Result<(), RubError> {
    let entry = request_frame_entry(router, frame_id).await?;
    ensure_frame_switchable(&entry)
}

async fn request_frame_entry(
    router: &DaemonRouter,
    frame_id: &str,
) -> Result<FrameInventoryEntry, RubError> {
    let frames = router.browser.list_frames().await.map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Unable to revalidate selected-frame authority because the current frame inventory is unavailable: {error}"
            ),
            serde_json::json!({
                "reason": "continuity_frame_inventory_unavailable",
                "frame_id": frame_id,
            }),
        )
    })?;

    frames
        .into_iter()
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
        })
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
                ErrorCode::SessionBusy,
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
                "Trigger {role} frame '{frame_id}' does not have top-level hit-test authority for trigger execution"
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
        ErrorCode::InvalidInput,
        format!(
            "Selected frame '{}' does not have top-level hit-test authority for frame-scoped interaction",
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

#[cfg(test)]
mod tests {
    use super::{effective_interaction_frame_id, effective_request_frame_id};
    use async_trait::async_trait;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::locator::LiveLocator;
    use rub_core::model::{
        BoundingBox, ContentFindMatch, Cookie, DialogInterceptPolicy, DialogRuntimeInfo, Element,
        FrameContextInfo, FrameInventoryEntry, FrameRuntimeInfo, InteractionOutcome, KeyCombo,
        LaunchPolicyInfo, LoadStrategy, NetworkRule, Page, RuntimeStateSnapshot, ScrollDirection,
        ScrollPosition, SelectOutcome, Snapshot, TabInfo, WaitCondition,
    };
    use rub_core::port::BrowserPort;
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::router::DaemonRouter;
    use crate::session::SessionState;

    macro_rules! unexpected_browser_call {
        ($name:expr) => {
            panic!("unexpected BrowserPort call in frame scope test: {}", $name)
        };
    }

    struct FrameScopeBrowser {
        frames: Vec<FrameInventoryEntry>,
        list_frames_error: Option<String>,
        tab_frames: Vec<FrameInventoryEntry>,
        list_frames_for_tab_error: Option<String>,
    }

    fn frame_inventory(frame_id: &str) -> FrameInventoryEntry {
        FrameInventoryEntry {
            index: 0,
            frame: FrameContextInfo {
                frame_id: frame_id.to_string(),
                name: Some(frame_id.to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some(format!("https://example.test/{frame_id}")),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            is_primary: frame_id == "root",
            is_current: frame_id == "root",
        }
    }

    fn frame_inventory_with_hit_test_authority(
        frame_id: &str,
        same_origin_accessible: Option<bool>,
    ) -> FrameInventoryEntry {
        FrameInventoryEntry {
            frame: FrameContextInfo {
                same_origin_accessible,
                ..frame_inventory(frame_id).frame
            },
            is_primary: false,
            is_current: false,
            ..frame_inventory(frame_id)
        }
    }

    #[async_trait]
    impl BrowserPort for FrameScopeBrowser {
        async fn navigate(
            &self,
            _url: &str,
            _strategy: LoadStrategy,
            _timeout_ms: u64,
        ) -> Result<Page, RubError> {
            unexpected_browser_call!("navigate")
        }

        async fn snapshot(&self, _limit: Option<u32>) -> Result<Snapshot, RubError> {
            unexpected_browser_call!("snapshot")
        }

        async fn snapshot_for_frame(
            &self,
            _frame_id: Option<&str>,
            _limit: Option<u32>,
        ) -> Result<Snapshot, RubError> {
            unexpected_browser_call!("snapshot_for_frame")
        }

        async fn click(&self, _element: &Element) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("click")
        }

        async fn execute_js(&self, _code: &str) -> Result<serde_json::Value, RubError> {
            unexpected_browser_call!("execute_js")
        }

        async fn execute_js_in_frame(
            &self,
            _frame_id: Option<&str>,
            _code: &str,
        ) -> Result<serde_json::Value, RubError> {
            unexpected_browser_call!("execute_js_in_frame")
        }

        async fn back(&self, _timeout_ms: u64) -> Result<Page, RubError> {
            unexpected_browser_call!("back")
        }

        async fn forward(&self, _timeout_ms: u64) -> Result<Page, RubError> {
            unexpected_browser_call!("forward")
        }

        async fn reload(
            &self,
            _strategy: LoadStrategy,
            _timeout_ms: u64,
        ) -> Result<Page, RubError> {
            unexpected_browser_call!("reload")
        }

        async fn handle_dialog(
            &self,
            _accept: bool,
            _prompt_text: Option<String>,
        ) -> Result<(), RubError> {
            unexpected_browser_call!("handle_dialog")
        }

        async fn dialog_runtime(&self) -> Result<DialogRuntimeInfo, RubError> {
            unexpected_browser_call!("dialog_runtime")
        }

        fn set_dialog_intercept(&self, _policy: DialogInterceptPolicy) -> Result<(), RubError> {
            unexpected_browser_call!("set_dialog_intercept")
        }

        fn clear_dialog_intercept(&self) -> Result<(), RubError> {
            unexpected_browser_call!("clear_dialog_intercept")
        }

        async fn scroll(
            &self,
            _frame_id: Option<&str>,
            _direction: ScrollDirection,
            _amount: Option<u32>,
        ) -> Result<ScrollPosition, RubError> {
            unexpected_browser_call!("scroll")
        }

        async fn screenshot(&self, _full_page: bool) -> Result<Vec<u8>, RubError> {
            unexpected_browser_call!("screenshot")
        }

        async fn health_check(&self) -> Result<(), RubError> {
            unexpected_browser_call!("health_check")
        }

        fn launch_policy(&self) -> LaunchPolicyInfo {
            unexpected_browser_call!("launch_policy")
        }

        async fn close(&self) -> Result<(), RubError> {
            unexpected_browser_call!("close")
        }

        async fn elevate_to_visible(&self) -> Result<LaunchPolicyInfo, RubError> {
            unexpected_browser_call!("elevate_to_visible")
        }

        async fn send_keys(&self, _combo: &KeyCombo) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("send_keys")
        }

        async fn send_keys_in_frame(
            &self,
            _frame_id: Option<&str>,
            _combo: &KeyCombo,
        ) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("send_keys_in_frame")
        }

        async fn type_text(&self, _text: &str) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("type_text")
        }

        async fn type_text_in_frame(
            &self,
            _frame_id: Option<&str>,
            _text: &str,
        ) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("type_text_in_frame")
        }

        async fn type_into(
            &self,
            _element: &Element,
            _text: &str,
            _clear: bool,
        ) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("type_into")
        }

        async fn wait_for(&self, _condition: &WaitCondition) -> Result<(), RubError> {
            unexpected_browser_call!("wait_for")
        }

        async fn list_tabs(&self) -> Result<Vec<TabInfo>, RubError> {
            unexpected_browser_call!("list_tabs")
        }

        async fn switch_tab(&self, _index: u32) -> Result<TabInfo, RubError> {
            unexpected_browser_call!("switch_tab")
        }

        async fn close_tab(&self, _index: Option<u32>) -> Result<Vec<TabInfo>, RubError> {
            unexpected_browser_call!("close_tab")
        }

        async fn get_title(&self) -> Result<String, RubError> {
            unexpected_browser_call!("get_title")
        }

        async fn get_html(&self, _selector: Option<&str>) -> Result<String, RubError> {
            unexpected_browser_call!("get_html")
        }

        async fn get_text(&self, _element: &Element) -> Result<String, RubError> {
            unexpected_browser_call!("get_text")
        }

        async fn get_outer_html(&self, _element: &Element) -> Result<String, RubError> {
            unexpected_browser_call!("get_outer_html")
        }

        async fn get_value(&self, _element: &Element) -> Result<String, RubError> {
            unexpected_browser_call!("get_value")
        }

        async fn get_attributes(
            &self,
            _element: &Element,
        ) -> Result<HashMap<String, String>, RubError> {
            unexpected_browser_call!("get_attributes")
        }

        async fn get_bbox(&self, _element: &Element) -> Result<BoundingBox, RubError> {
            unexpected_browser_call!("get_bbox")
        }

        async fn query_text(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<String, RubError> {
            unexpected_browser_call!("query_text")
        }

        async fn query_text_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<String, RubError> {
            unexpected_browser_call!("query_text_in_tab")
        }

        async fn query_text_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<String>, RubError> {
            unexpected_browser_call!("query_text_many")
        }

        async fn query_html(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<String, RubError> {
            unexpected_browser_call!("query_html")
        }

        async fn query_html_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<String, RubError> {
            unexpected_browser_call!("query_html_in_tab")
        }

        async fn query_html_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<String>, RubError> {
            unexpected_browser_call!("query_html_many")
        }

        async fn query_value(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<String, RubError> {
            unexpected_browser_call!("query_value")
        }

        async fn query_value_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<String, RubError> {
            unexpected_browser_call!("query_value_in_tab")
        }

        async fn query_value_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<String>, RubError> {
            unexpected_browser_call!("query_value_many")
        }

        async fn query_attributes(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<HashMap<String, String>, RubError> {
            unexpected_browser_call!("query_attributes")
        }

        async fn query_attributes_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<HashMap<String, String>, RubError> {
            unexpected_browser_call!("query_attributes_in_tab")
        }

        async fn query_attributes_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<HashMap<String, String>>, RubError> {
            unexpected_browser_call!("query_attributes_many")
        }

        async fn query_bbox(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<BoundingBox, RubError> {
            unexpected_browser_call!("query_bbox")
        }

        async fn query_bbox_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<BoundingBox>, RubError> {
            unexpected_browser_call!("query_bbox_many")
        }

        async fn probe_runtime_state_for_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
        ) -> Result<RuntimeStateSnapshot, RubError> {
            unexpected_browser_call!("probe_runtime_state_for_tab")
        }

        async fn tab_has_text(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _text: &str,
        ) -> Result<bool, RubError> {
            unexpected_browser_call!("tab_has_text")
        }

        async fn find_content_matches_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<ContentFindMatch>, RubError> {
            unexpected_browser_call!("find_content_matches_in_tab")
        }

        async fn find_snapshot_elements_by_selector(
            &self,
            _snapshot: &Snapshot,
            _selector: &str,
        ) -> Result<Vec<Element>, RubError> {
            unexpected_browser_call!("find_snapshot_elements_by_selector")
        }

        async fn filter_snapshot_elements_by_hit_test(
            &self,
            _snapshot: &Snapshot,
            _elements: &[Element],
        ) -> Result<Vec<Element>, RubError> {
            unexpected_browser_call!("filter_snapshot_elements_by_hit_test")
        }

        async fn find_snapshot_elements_in_observation_scope(
            &self,
            _snapshot: &Snapshot,
            _scope: &rub_core::observation::ObservationScope,
        ) -> Result<(Vec<Element>, u32), RubError> {
            unexpected_browser_call!("find_snapshot_elements_in_observation_scope")
        }

        async fn find_content_matches(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<ContentFindMatch>, RubError> {
            unexpected_browser_call!("find_content_matches")
        }

        async fn click_xy(&self, _x: f64, _y: f64) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("click_xy")
        }

        async fn hover(&self, _element: &Element) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("hover")
        }

        async fn dblclick_xy(&self, _x: f64, _y: f64) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("dblclick_xy")
        }

        async fn rightclick_xy(&self, _x: f64, _y: f64) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("rightclick_xy")
        }

        async fn dblclick(&self, _element: &Element) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("dblclick")
        }

        async fn rightclick(&self, _element: &Element) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("rightclick")
        }

        async fn get_cookies(&self, _url: Option<&str>) -> Result<Vec<Cookie>, RubError> {
            unexpected_browser_call!("get_cookies")
        }

        async fn set_cookie(&self, _cookie: &Cookie) -> Result<(), RubError> {
            unexpected_browser_call!("set_cookie")
        }

        async fn delete_cookies(&self, _url: Option<&str>) -> Result<(), RubError> {
            unexpected_browser_call!("delete_cookies")
        }

        async fn storage_snapshot(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
        ) -> Result<StorageSnapshot, RubError> {
            unexpected_browser_call!("storage_snapshot")
        }

        async fn storage_snapshot_for_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
        ) -> Result<StorageSnapshot, RubError> {
            unexpected_browser_call!("storage_snapshot_for_tab")
        }

        async fn set_storage_item(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _area: StorageArea,
            _key: &str,
            _value: &str,
        ) -> Result<StorageSnapshot, RubError> {
            unexpected_browser_call!("set_storage_item")
        }

        async fn remove_storage_item(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _area: StorageArea,
            _key: &str,
        ) -> Result<StorageSnapshot, RubError> {
            unexpected_browser_call!("remove_storage_item")
        }

        async fn clear_storage(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _area: Option<StorageArea>,
        ) -> Result<StorageSnapshot, RubError> {
            unexpected_browser_call!("clear_storage")
        }

        async fn replace_storage(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _snapshot: &StorageSnapshot,
        ) -> Result<StorageSnapshot, RubError> {
            unexpected_browser_call!("replace_storage")
        }

        async fn upload_file(
            &self,
            _element: &Element,
            _path: &str,
        ) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("upload_file")
        }

        async fn select_option(
            &self,
            _element: &Element,
            _value: &str,
        ) -> Result<SelectOutcome, RubError> {
            unexpected_browser_call!("select_option")
        }

        async fn snapshot_with_a11y(&self, _limit: Option<u32>) -> Result<Snapshot, RubError> {
            unexpected_browser_call!("snapshot_with_a11y")
        }

        async fn snapshot_with_a11y_for_frame(
            &self,
            _frame_id: Option<&str>,
            _limit: Option<u32>,
        ) -> Result<Snapshot, RubError> {
            unexpected_browser_call!("snapshot_with_a11y_for_frame")
        }

        async fn viewport_dimensions(&self) -> Result<(f64, f64), RubError> {
            unexpected_browser_call!("viewport_dimensions")
        }

        async fn highlight_elements(&self, _snapshot: &Snapshot) -> Result<u32, RubError> {
            unexpected_browser_call!("highlight_elements")
        }

        async fn cleanup_highlights(&self) -> Result<(), RubError> {
            unexpected_browser_call!("cleanup_highlights")
        }

        async fn snapshot_with_listeners(
            &self,
            _limit: Option<u32>,
            _include_a11y: bool,
        ) -> Result<Snapshot, RubError> {
            unexpected_browser_call!("snapshot_with_listeners")
        }

        async fn snapshot_with_listeners_for_frame(
            &self,
            _frame_id: Option<&str>,
            _limit: Option<u32>,
            _include_a11y: bool,
        ) -> Result<Snapshot, RubError> {
            unexpected_browser_call!("snapshot_with_listeners_for_frame")
        }

        async fn sync_network_rules(&self, _rules: &[NetworkRule]) -> Result<(), RubError> {
            unexpected_browser_call!("sync_network_rules")
        }

        async fn probe_runtime_state(&self) -> Result<RuntimeStateSnapshot, RubError> {
            unexpected_browser_call!("probe_runtime_state")
        }

        async fn probe_frame_runtime(&self) -> Result<FrameRuntimeInfo, RubError> {
            unexpected_browser_call!("probe_frame_runtime")
        }

        async fn list_frames(&self) -> Result<Vec<FrameInventoryEntry>, RubError> {
            if let Some(error) = &self.list_frames_error {
                return Err(RubError::Internal(error.clone()));
            }
            Ok(self.frames.clone())
        }

        async fn list_frames_for_tab(
            &self,
            _target_id: &str,
        ) -> Result<Vec<FrameInventoryEntry>, RubError> {
            if let Some(error) = &self.list_frames_for_tab_error {
                return Err(RubError::Internal(error.clone()));
            }
            Ok(self.tab_frames.clone())
        }

        async fn cancel_download(&self, _guid: &str) -> Result<(), RubError> {
            unexpected_browser_call!("cancel_download")
        }
    }

    fn test_router(frames: Vec<FrameInventoryEntry>) -> DaemonRouter {
        DaemonRouter::new(Arc::new(FrameScopeBrowser {
            tab_frames: frames.clone(),
            frames,
            list_frames_error: None,
            list_frames_for_tab_error: None,
        }))
    }

    fn test_router_with_inventory_error(error: &str) -> DaemonRouter {
        DaemonRouter::new(Arc::new(FrameScopeBrowser {
            frames: Vec::new(),
            list_frames_error: Some(error.to_string()),
            tab_frames: Vec::new(),
            list_frames_for_tab_error: None,
        }))
    }

    fn test_router_with_tab_inventory_error(error: &str) -> DaemonRouter {
        DaemonRouter::new(Arc::new(FrameScopeBrowser {
            frames: Vec::new(),
            list_frames_error: None,
            tab_frames: Vec::new(),
            list_frames_for_tab_error: Some(error.to_string()),
        }))
    }

    fn test_state() -> Arc<SessionState> {
        Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-frame-scope-test"),
            None,
        ))
    }

    #[tokio::test]
    async fn effective_request_frame_id_uses_selected_frame_authority() {
        let router = test_router(vec![frame_inventory("root"), frame_inventory("child")]);
        let state = test_state();
        state.select_frame(Some("child".to_string())).await;

        let frame_id = effective_request_frame_id(&router, &serde_json::json!({}), &state)
            .await
            .expect("selected frame should remain authoritative");

        assert_eq!(frame_id.as_deref(), Some("child"));
    }

    #[tokio::test]
    async fn effective_request_frame_id_accepts_selected_frame_without_hit_test_authority() {
        let router = test_router(vec![
            frame_inventory("root"),
            frame_inventory_with_hit_test_authority("child", Some(false)),
        ]);
        let state = test_state();
        state.select_frame(Some("child".to_string())).await;

        let frame_id = effective_request_frame_id(&router, &serde_json::json!({}), &state)
            .await
            .expect("read/storage/execution paths only need selected-frame continuity");

        assert_eq!(frame_id.as_deref(), Some("child"));
    }

    #[tokio::test]
    async fn effective_interaction_frame_id_rejects_selected_frame_without_hit_test_authority() {
        let router = test_router(vec![
            frame_inventory("root"),
            frame_inventory_with_hit_test_authority("child", Some(false)),
        ]);
        let state = test_state();
        state.select_frame(Some("child".to_string())).await;

        let error = effective_interaction_frame_id(&router, &serde_json::json!({}), &state)
            .await
            .expect_err("interaction paths need top-level hit-test authority");
        let envelope = error.into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("top-level hit-test authority"));
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_frame_unavailable")
        );
    }

    #[tokio::test]
    async fn effective_request_frame_id_reports_selected_frame_drift_as_invalid_input() {
        let router = test_router(vec![frame_inventory("root")]);
        let state = test_state();
        state.select_frame(Some("child".to_string())).await;

        let error = effective_request_frame_id(&router, &serde_json::json!({}), &state)
            .await
            .expect_err("missing selected frame must fail closed");
        let envelope = error.into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_frame_unavailable")
        );
    }

    #[tokio::test]
    async fn effective_request_frame_id_reports_inventory_probe_failure_as_frame_continuity_loss() {
        let router = test_router_with_inventory_error("list frames failed");
        let state = test_state();
        state.select_frame(Some("child".to_string())).await;

        let error = effective_request_frame_id(&router, &serde_json::json!({}), &state)
            .await
            .expect_err("inventory probe failure must fail closed");
        let envelope = error.into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_frame_inventory_unavailable")
        );
        assert!(envelope.message.contains("selected-frame authority"));
    }

    #[tokio::test]
    async fn ensure_tab_frame_available_reports_inventory_probe_failure_as_degraded_authority() {
        let router = test_router_with_tab_inventory_error("list tab frames failed");
        let browser = router.browser_port();

        let error = super::ensure_tab_frame_available(&browser, "target-1", "child", "source")
            .await
            .expect_err("tab inventory probe failure must fail closed");
        let envelope = error.into_envelope();

        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("trigger_source_frame_inventory_unavailable")
        );
        assert!(envelope.message.contains("frame inventory"));
    }
}
