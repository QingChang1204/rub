use super::evaluate_orchestration_probe_for_tab;
use super::matching::network_request_matches;
use async_trait::async_trait;
use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::{
    BoundingBox, ContentFindMatch, Cookie, DialogInterceptPolicy, DialogRuntimeInfo, Element,
    FrameInventoryEntry, FrameRuntimeInfo, InteractionOutcome, KeyCombo, LaunchPolicyInfo,
    LoadStrategy, NetworkRequestLifecycle, NetworkRequestRecord, NetworkRule, OverlayState, Page,
    ReadinessInfo, ReadinessStatus, RouteStability, RuntimeStateSnapshot, ScrollDirection,
    ScrollPosition, SelectOutcome, Snapshot, StateInspectorInfo, TabInfo, TriggerConditionKind,
    TriggerConditionSpec, WaitCondition,
};
use rub_core::observation::ObservationScope;
use rub_core::port::BrowserPort;
use rub_core::storage::{StorageArea, StorageSnapshot};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use crate::session::SessionState;

macro_rules! unexpected_browser_call {
    ($name:expr) => {
        panic!(
            "unexpected BrowserPort call in orchestration probe test: {}",
            $name
        )
    };
}

#[derive(Default)]
struct ReadinessProbeBrowser {
    tabs: Vec<TabInfo>,
    degraded_readiness: bool,
}

fn ready_snapshot() -> RuntimeStateSnapshot {
    RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo::default(),
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("complete".to_string()),
            blocking_signals: Vec::new(),
            degraded_reason: None,
        },
    }
}

fn degraded_ready_snapshot() -> RuntimeStateSnapshot {
    RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo::default(),
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Degraded,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("complete".to_string()),
            blocking_signals: Vec::new(),
            degraded_reason: Some("document_fence_changed".to_string()),
        },
    }
}

#[async_trait]
impl BrowserPort for ReadinessProbeBrowser {
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

    async fn reload(&self, _strategy: LoadStrategy, _timeout_ms: u64) -> Result<Page, RubError> {
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
        Ok(self.tabs.clone())
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
        frame_id: Option<&str>,
    ) -> Result<RuntimeStateSnapshot, RubError> {
        match frame_id {
            Some(frame_id) => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("frame '{frame_id}' is unavailable"),
            )),
            None => Ok(if self.degraded_readiness {
                degraded_ready_snapshot()
            } else {
                ready_snapshot()
            }),
        }
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

fn network_record(frame_id: Option<&str>) -> NetworkRequestRecord {
    NetworkRequestRecord {
        request_id: "req-1".to_string(),
        sequence: 1,
        lifecycle: NetworkRequestLifecycle::Completed,
        url: "https://example.test/api".to_string(),
        method: "GET".to_string(),
        tab_target_id: Some("tab-source".to_string()),
        status: Some(200),
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_body: None,
        response_body: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        frame_id: frame_id.map(str::to_string),
        resource_type: None,
        mime_type: None,
    }
}

fn network_condition() -> TriggerConditionSpec {
    TriggerConditionSpec {
        kind: TriggerConditionKind::NetworkRequest,
        locator: None,
        text: None,
        url_pattern: Some("/api".to_string()),
        readiness_state: None,
        method: Some("GET".to_string()),
        status_code: Some(200),
        storage_area: None,
        key: None,
        value: None,
    }
}

fn sample_text_input_element() -> Element {
    Element {
        index: 1,
        tag: rub_core::model::ElementTag::Input,
        text: String::new(),
        attributes: HashMap::new(),
        element_ref: Some("frame:1".to_string()),
        target_id: None,
        bounding_box: None,
        ax_info: None,
        listeners: None,
        depth: Some(0),
    }
}

#[tokio::test]
#[should_panic(expected = "type_into")]
async fn browser_port_input_default_delegates_to_type_into_lane() {
    let browser = ReadinessProbeBrowser::default();
    let element = sample_text_input_element();

    let _ = BrowserPort::input(&browser, &element, "hello", false).await;
}

#[test]
fn orchestration_network_request_matches_require_source_frame_when_present() {
    let condition = network_condition();
    let record = network_record(Some("frame-a"));

    assert!(network_request_matches(
        &record,
        "tab-source",
        Some("frame-a"),
        &condition
    ));
    assert!(!network_request_matches(
        &record,
        "tab-source",
        Some("frame-b"),
        &condition
    ));
}

#[test]
fn orchestration_network_request_matches_allow_tab_scoped_rules_without_frame() {
    let condition = network_condition();
    let record = network_record(Some("frame-a"));

    assert!(network_request_matches(
        &record,
        "tab-source",
        None,
        &condition
    ));
}

#[tokio::test]
async fn orchestration_readiness_probe_fails_closed_for_explicit_frame_errors() {
    let browser: Arc<dyn BrowserPort> = Arc::new(ReadinessProbeBrowser::default());
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-orchestration-probe-explicit-frame"),
        None,
    ));
    let condition = TriggerConditionSpec {
        kind: TriggerConditionKind::Readiness,
        locator: None,
        text: None,
        url_pattern: None,
        readiness_state: Some("stable".to_string()),
        method: None,
        status_code: None,
        storage_area: None,
        key: None,
        value: None,
    };

    let error = evaluate_orchestration_probe_for_tab(
        &browser,
        &state,
        "tab-source",
        Some("missing-frame"),
        &condition,
        0,
        0,
    )
    .await
    .expect_err("explicit frame readiness probe must fail closed");

    assert!(error.to_string().contains("missing-frame"), "{error}");
}

#[tokio::test]
async fn orchestration_readiness_probe_fails_closed_when_readiness_is_degraded() {
    let browser: Arc<dyn BrowserPort> = Arc::new(ReadinessProbeBrowser {
        tabs: Vec::new(),
        degraded_readiness: true,
    });
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-orchestration-probe-degraded-readiness"),
        None,
    ));
    let condition = TriggerConditionSpec {
        kind: TriggerConditionKind::Readiness,
        locator: None,
        text: None,
        url_pattern: None,
        readiness_state: Some("stable".to_string()),
        method: None,
        status_code: None,
        storage_area: None,
        key: None,
        value: None,
    };

    let result = evaluate_orchestration_probe_for_tab(
        &browser,
        &state,
        "tab-source",
        None,
        &condition,
        0,
        0,
    )
    .await
    .expect("degraded readiness should fail closed as a non-match");

    assert!(!result.matched);
    assert!(result.evidence.is_none());
    assert!(result.degraded_reason.is_none());
}

#[tokio::test]
async fn orchestration_url_match_fails_closed_when_source_tab_page_identity_is_degraded() {
    let browser: Arc<dyn BrowserPort> = Arc::new(ReadinessProbeBrowser {
        tabs: vec![TabInfo {
            index: 0,
            target_id: "tab-source".to_string(),
            url: String::new(),
            title: String::new(),
            active: true,
            active_authority: None,
            degraded_reason: Some("tab_url_and_title_probe_failed".to_string()),
        }],
        degraded_readiness: false,
    });
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-orchestration-probe-degraded-url"),
        None,
    ));
    let condition = TriggerConditionSpec {
        kind: TriggerConditionKind::UrlMatch,
        locator: None,
        text: None,
        url_pattern: Some("/events".to_string()),
        readiness_state: None,
        method: None,
        status_code: None,
        storage_area: None,
        key: None,
        value: None,
    };

    let error = evaluate_orchestration_probe_for_tab(
        &browser,
        &state,
        "tab-source",
        None,
        &condition,
        0,
        0,
    )
    .await
    .expect_err("degraded source tab page identity must fail closed");

    assert!(matches!(
        error,
        RubError::Domain(ref envelope) if envelope.code == ErrorCode::SessionBusy
    ));
    assert!(error.to_string().contains("not authoritative"), "{error}");
}
