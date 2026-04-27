use std::sync::Arc;

use super::dispatch::command_supports_post_wait;
use super::timeout::{effective_wait_frame_id, parse_wait_condition, post_wait_timeout_error};
use super::{DaemonRouter, TransactionDeadline};
use crate::session::SessionState;
use rub_core::DEFAULT_WAIT_AFTER_TIMEOUT_MS;
use rub_core::error::{ErrorCode, RubError};
use rub_core::port::BrowserPort;

pub(super) async fn apply_post_wait_if_requested(
    router: &DaemonRouter,
    browser: Arc<dyn BrowserPort>,
    command: &str,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    mut data: serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let Some(wait_args) = raw_wait_after_args(command, args) else {
        return Ok(data);
    };

    let mut parsed = parse_wait_condition(&wait_args, DEFAULT_WAIT_AFTER_TIMEOUT_MS)?;
    let requested_wait_timeout_ms = parsed.condition.timeout_ms;
    let effective_wait_timeout_ms =
        bounded_wait_after_timeout_ms(requested_wait_timeout_ms, deadline.remaining_ms());
    if effective_wait_timeout_ms == 0 {
        return Err(post_wait_timeout_error(
            command,
            &wait_args,
            Some(deadline.timeout_ms),
            Some(0),
        ));
    }
    parsed.condition.timeout_ms = effective_wait_timeout_ms;
    parsed.condition.frame_id =
        effective_wait_frame_id(router, args, state, &parsed.condition.kind).await?;
    let start = std::time::Instant::now();
    match browser.wait_for(&parsed.condition).await {
        Ok(()) => {
            if let Some(object) = data.as_object_mut() {
                object.insert(
                    "wait_after".to_string(),
                    serde_json::json!({
                        "matched": true,
                        "kind": parsed.kind_name,
                        "value": parsed.probe_value,
                        "elapsed_ms": start.elapsed().as_millis() as u64,
                    }),
                );
            }
            Ok(data)
        }
        Err(RubError::Domain(envelope)) if envelope.code == ErrorCode::WaitTimeout => {
            Err(post_wait_timeout_error(
                command,
                &wait_args,
                Some(deadline.timeout_ms),
                Some(effective_wait_timeout_ms),
            ))
        }
        Err(other) => Err(other),
    }
}

pub(super) fn bounded_wait_after_timeout_ms(
    requested_wait_timeout_ms: u64,
    remaining_transaction_timeout_ms: u64,
) -> u64 {
    requested_wait_timeout_ms.min(remaining_transaction_timeout_ms)
}

pub(super) fn wait_after_args(args: &serde_json::Value) -> Option<serde_json::Value> {
    args.get("wait_after").cloned()
}

pub(super) fn validate_wait_after_args_if_requested(
    command: &str,
    args: &serde_json::Value,
) -> Result<(), RubError> {
    let Some(wait_after) = raw_wait_after_args(command, args) else {
        return Ok(());
    };
    let _ = parse_wait_condition(&wait_after, DEFAULT_WAIT_AFTER_TIMEOUT_MS)?;
    Ok(())
}

fn raw_wait_after_args(command: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    command_supports_post_wait(command)
        .then(|| wait_after_args(args))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_wait_after_timeout_ms, command_supports_post_wait, effective_wait_frame_id,
        parse_wait_condition, validate_wait_after_args_if_requested, wait_after_args,
    };
    use crate::router::DaemonRouter;
    use async_trait::async_trait;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::locator::LiveLocator;
    use rub_core::model::{
        BoundingBox, ContentFindMatch, Cookie, DialogInterceptPolicy, DialogRuntimeInfo, Element,
        FrameInventoryEntry, FrameRuntimeInfo, InteractionOutcome, KeyCombo, LaunchPolicyInfo,
        LoadStrategy, NetworkRule, Page, RuntimeStateSnapshot, ScrollDirection, ScrollPosition,
        SelectOutcome, Snapshot, TabInfo,
    };
    use rub_core::observation::ObservationScope;
    use rub_core::port::BrowserPort;
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use std::sync::Arc;

    use crate::session::SessionState;

    macro_rules! unexpected_browser_call {
        ($name:expr) => {
            panic!("unexpected BrowserPort call in wait_after test: {}", $name)
        };
    }

    #[derive(Default)]
    struct WaitAfterFrameProbeBrowser;

    #[async_trait]
    impl BrowserPort for WaitAfterFrameProbeBrowser {
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
        async fn wait_for(
            &self,
            _condition: &rub_core::model::WaitCondition,
        ) -> Result<(), RubError> {
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
        ) -> Result<std::collections::HashMap<String, String>, RubError> {
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
        ) -> Result<std::collections::HashMap<String, String>, RubError> {
            unexpected_browser_call!("query_attributes")
        }
        async fn query_attributes_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<std::collections::HashMap<String, String>, RubError> {
            unexpected_browser_call!("query_attributes_in_tab")
        }
        async fn query_attributes_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &LiveLocator,
        ) -> Result<Vec<std::collections::HashMap<String, String>>, RubError> {
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
            _scope: &ObservationScope,
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
            unexpected_browser_call!("list_frames")
        }
        async fn list_frames_for_tab(
            &self,
            _target_id: &str,
        ) -> Result<Vec<FrameInventoryEntry>, RubError> {
            unexpected_browser_call!("list_frames_for_tab")
        }
        async fn cancel_download(&self, _guid: &str) -> Result<(), RubError> {
            unexpected_browser_call!("cancel_download")
        }
    }

    #[test]
    fn wait_after_supports_forward_and_reload() {
        assert!(command_supports_post_wait("forward"));
        assert!(command_supports_post_wait("reload"));
    }

    #[test]
    fn wait_after_args_recognizes_page_level_wait_probes() {
        assert!(
            wait_after_args(&serde_json::json!({
                "wait_after": {
                    "url_contains": "/activate"
                }
            }))
            .is_some()
        );
        assert!(
            wait_after_args(&serde_json::json!({
                "wait_after": {
                    "title_contains": "Confirm your account"
                }
            }))
            .is_some()
        );
    }

    #[test]
    fn wait_after_args_recognizes_state_and_description_probes() {
        assert!(
            wait_after_args(&serde_json::json!({
                "wait_after": {
                    "state": "interactable"
                }
            }))
            .is_some()
        );
        assert!(
            wait_after_args(&serde_json::json!({
                "wait_after": {
                    "label": "Email",
                    "description_contains": "We will email you to confirm"
                }
            }))
            .is_some()
        );
    }

    #[test]
    fn wait_after_validation_rejects_unknown_fields_before_actuation() {
        let error = validate_wait_after_args_if_requested(
            "click",
            &serde_json::json!({
                "wait_after": {
                    "selector": "#done",
                    "mystery": true,
                }
            }),
        )
        .expect_err("unknown wait_after fields must fail before actuation")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("unknown field") || error.message.contains("mystery"));
    }

    #[test]
    fn wait_after_validation_rejects_unknown_only_payloads_before_actuation() {
        let error = validate_wait_after_args_if_requested(
            "click",
            &serde_json::json!({
                "wait_after": {
                    "mystery": true,
                }
            }),
        )
        .expect_err("unknown-only wait_after payload must not degrade to no-op")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn wait_after_timeout_is_bounded_by_remaining_transaction_budget() {
        assert_eq!(bounded_wait_after_timeout_ms(1_500, 400), 400);
        assert_eq!(bounded_wait_after_timeout_ms(250, 400), 250);
        assert_eq!(bounded_wait_after_timeout_ms(250, 0), 0);
    }

    #[tokio::test]
    async fn page_level_wait_after_ignores_selected_frame_authority() {
        let browser = Arc::new(WaitAfterFrameProbeBrowser);
        let router = DaemonRouter::new(browser);
        let home = std::env::temp_dir().join(format!(
            "rub-wait-after-page-global-{}",
            uuid::Uuid::now_v7()
        ));
        let state = Arc::new(SessionState::new("default", home, None));
        state.select_frame(Some("stale-child".to_string())).await;

        let parsed = parse_wait_condition(
            &serde_json::json!({
                "url_contains": "/activate",
            }),
            rub_core::DEFAULT_WAIT_AFTER_TIMEOUT_MS,
        )
        .expect("page-level wait_after condition should parse");

        let frame_id = effective_wait_frame_id(
            &router,
            &serde_json::json!({}),
            &state,
            &parsed.condition.kind,
        )
        .await
        .expect("page-level wait_after should not require selected-frame authority");
        assert_eq!(frame_id, None);
    }
}
