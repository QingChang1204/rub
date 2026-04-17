use async_trait::async_trait;

use crate::error::RubError;
use crate::locator::LiveLocator;
use crate::model::{
    BoundingBox, ContentFindMatch, Cookie, DialogRuntimeInfo, Element, FrameInventoryEntry,
    FrameRuntimeInfo, HistoryNavigationResult, InteractionOutcome, KeyCombo, LaunchPolicyInfo,
    LoadStrategy, NetworkRule, Page, RuntimeStateSnapshot, ScrollDirection, ScrollPosition,
    SelectOutcome, Snapshot, TabInfo, WaitCondition,
};
use crate::observation::ObservationScope;
use crate::storage::{StorageArea, StorageSnapshot};
use std::collections::HashMap;

/// Default snapshot element cap used when callers omit an explicit limit.
pub const DEFAULT_SNAPSHOT_LIMIT: u32 = 500;

/// Infrastructure boundary for browser control.
/// Implemented by `rub-cdp::ChromiumAdapter`.
/// Defined in `rub-core` so the daemon can depend on the trait
/// without depending on the CDP implementation (hexagonal architecture).
///
/// Contract notes:
/// - frame-scoped methods must fail closed for explicit unavailable frames
///   rather than silently degrading to a tab-wide/top-frame probe
/// - one-shot dialog intercept methods must return an error instead of
///   pretending success
/// - query helpers are read-only authorities and must not mutate active-tab
///   ownership as a side effect
#[async_trait]
pub trait BrowserPort: Send + Sync {
    /// Navigate to URL with the specified load strategy.
    async fn navigate(
        &self,
        url: &str,
        strategy: LoadStrategy,
        timeout_ms: u64,
    ) -> Result<Page, RubError>;

    /// Capture a DOM snapshot with interactive elements.
    /// `limit`: max elements to return. `None` = use default (`DEFAULT_SNAPSHOT_LIMIT`).
    /// `Some(0)` = no limit.
    async fn snapshot(&self, limit: Option<u32>) -> Result<Snapshot, RubError>;

    /// Capture a DOM snapshot for an explicit frame context (`None` = top/primary frame).
    async fn snapshot_for_frame(
        &self,
        frame_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Snapshot, RubError>;

    /// Click an element resolved from a previously published snapshot.
    async fn click(&self, element: &Element) -> Result<InteractionOutcome, RubError>;

    /// Click an element, wait for focus, then type text.
    /// Atomic click+type prevents focus-stealing race in multi-client sessions.
    async fn input(
        &self,
        element: &Element,
        text: &str,
        clear: bool,
    ) -> Result<InteractionOutcome, RubError>;

    /// Execute JavaScript and return the result as JSON.
    async fn execute_js(&self, code: &str) -> Result<serde_json::Value, RubError>;

    /// Execute JavaScript inside an explicit frame context (`None` = top/primary frame).
    async fn execute_js_in_frame(
        &self,
        frame_id: Option<&str>,
        code: &str,
    ) -> Result<serde_json::Value, RubError>;

    /// Navigate back in browser history.
    async fn back(&self, timeout_ms: u64) -> Result<Page, RubError>;

    /// Navigate back in browser history and capture the history boundary truth
    /// from the same authoritative page handle.
    async fn back_with_boundary(
        &self,
        timeout_ms: u64,
    ) -> Result<HistoryNavigationResult, RubError> {
        let page = self.back(timeout_ms).await?;
        Ok(HistoryNavigationResult {
            page,
            at_boundary: None,
        })
    }

    /// Navigate forward in browser history.
    async fn forward(&self, timeout_ms: u64) -> Result<Page, RubError>;

    /// Navigate forward in browser history and capture the history boundary
    /// truth from the same authoritative page handle.
    async fn forward_with_boundary(
        &self,
        timeout_ms: u64,
    ) -> Result<HistoryNavigationResult, RubError> {
        let page = self.forward(timeout_ms).await?;
        Ok(HistoryNavigationResult {
            page,
            at_boundary: None,
        })
    }

    /// Reload the current page with the specified load strategy.
    async fn reload(&self, strategy: LoadStrategy, timeout_ms: u64) -> Result<Page, RubError>;

    /// Accept or dismiss a pending JavaScript dialog on the active page.
    async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> Result<(), RubError>;

    /// Current browser-side JavaScript dialog runtime projection.
    async fn dialog_runtime(&self) -> Result<DialogRuntimeInfo, RubError>;

    /// Arm a one-shot intercept policy for the next JavaScript dialog.
    ///
    /// The CDP listener task consumes this immediately when a dialog opens,
    /// calling `Page.handleJavaScriptDialog` before Chrome's auto-handler fires.
    /// Implementations must return an error rather than silently no-op.
    fn set_dialog_intercept(
        &self,
        policy: crate::model::DialogInterceptPolicy,
    ) -> Result<(), RubError>;

    /// Cancel any pending one-shot dialog intercept policy.
    /// Implementations must return an error rather than silently no-op.
    fn clear_dialog_intercept(&self) -> Result<(), RubError>;

    /// Scroll the viewport inside an explicit frame context (`None` = top/primary frame).
    async fn scroll(
        &self,
        frame_id: Option<&str>,
        direction: ScrollDirection,
        amount: Option<u32>,
    ) -> Result<ScrollPosition, RubError>;

    /// Capture a screenshot. Returns PNG bytes.
    /// `full_page`: if true, capture the entire scrollable area.
    async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>, RubError>;

    /// CDP health check: `Browser.getVersion()`.
    async fn health_check(&self) -> Result<(), RubError>;

    /// Launch policy currently applied to the backing browser instance.
    fn launch_policy(&self) -> LaunchPolicyInfo;

    /// Gracefully close the browser.
    async fn close(&self) -> Result<(), RubError>;

    /// Relaunch a managed headless session into a visible managed browser.
    /// Implementations must not pretend external sessions or live browsers can
    /// hot-switch headed/headless in place.
    async fn elevate_to_visible(&self) -> Result<LaunchPolicyInfo, RubError>;

    // ── v1.1: Keyboard ──────────────────────────────────────────────

    /// Send a key combination (e.g., Enter, Control+a).
    async fn send_keys(&self, combo: &KeyCombo) -> Result<InteractionOutcome, RubError>;

    /// Type text character-by-character (keyDown → char → keyUp per char).
    async fn type_text(&self, text: &str) -> Result<InteractionOutcome, RubError>;

    /// Type text into the active element inside an explicit frame context
    /// (`None` = top/primary frame).
    async fn type_text_in_frame(
        &self,
        frame_id: Option<&str>,
        text: &str,
    ) -> Result<InteractionOutcome, RubError>;

    /// Focus a resolved element, then type text character-by-character using
    /// the same keyboard semantics as `type_text`.
    async fn type_into(
        &self,
        element: &Element,
        text: &str,
        clear: bool,
    ) -> Result<InteractionOutcome, RubError>;

    // ── v1.1: Wait ──────────────────────────────────────────────────

    /// Wait for a condition to be met (selector or text). Returns when
    /// condition is satisfied or returns WaitTimeout error.
    async fn wait_for(&self, condition: &WaitCondition) -> Result<(), RubError>;

    // ── v1.1: Tabs ──────────────────────────────────────────────────

    /// List all browser tabs.
    async fn list_tabs(&self) -> Result<Vec<TabInfo>, RubError>;

    /// Switch to a tab by index. Returns the activated tab info.
    async fn switch_tab(&self, index: u32) -> Result<TabInfo, RubError>;

    /// Close a tab by index (None = current tab). Returns remaining tabs.
    async fn close_tab(&self, index: Option<u32>) -> Result<Vec<TabInfo>, RubError>;

    // ── v1.1: DOM Extraction ────────────────────────────────────────

    /// Get the page title.
    async fn get_title(&self) -> Result<String, RubError>;

    /// Get page HTML or HTML of a selector-matched element.
    async fn get_html(&self, selector: Option<&str>) -> Result<String, RubError>;

    /// Get text content of a snapshot element.
    async fn get_text(&self, element: &Element) -> Result<String, RubError>;

    /// Get outer HTML of a snapshot element.
    async fn get_outer_html(&self, element: &Element) -> Result<String, RubError>;

    /// Get value of an input/textarea element.
    async fn get_value(&self, element: &Element) -> Result<String, RubError>;

    /// Get all attributes of a snapshot element.
    async fn get_attributes(&self, element: &Element) -> Result<HashMap<String, String>, RubError>;

    /// Get bounding box of a snapshot element.
    async fn get_bbox(&self, element: &Element) -> Result<BoundingBox, RubError>;

    /// Query text content through the live read-only DOM authority inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn query_text(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError>;

    /// Query text content through the live read-only DOM authority of the tab
    /// identified by stable `target_id`, without mutating the current
    /// active-tab authority.
    async fn query_text_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError>;

    /// Query text content for every selected live DOM match inside an explicit
    /// frame context (`None` = top/primary frame).
    async fn query_text_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<String>, RubError>;

    /// Query outer HTML through the live read-only DOM authority inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn query_html(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError>;

    /// Query outer HTML through the live read-only DOM authority of the tab
    /// identified by stable `target_id`, without mutating the current
    /// active-tab authority.
    async fn query_html_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError>;

    /// Query outer HTML for every selected live DOM match inside an explicit
    /// frame context (`None` = top/primary frame).
    async fn query_html_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<String>, RubError>;

    /// Query form value through the live read-only DOM authority inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn query_value(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError>;

    /// Query form value through the live read-only DOM authority of the tab
    /// identified by stable `target_id`, without mutating the current
    /// active-tab authority.
    async fn query_value_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError>;

    /// Query form values for every selected live DOM match inside an explicit
    /// frame context (`None` = top/primary frame).
    async fn query_value_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<String>, RubError>;

    /// Query attributes through the live read-only DOM authority inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn query_attributes(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<HashMap<String, String>, RubError>;

    /// Query attributes through the live read-only DOM authority of the tab
    /// identified by stable `target_id`, without mutating the current
    /// active-tab authority.
    async fn query_attributes_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<HashMap<String, String>, RubError>;

    /// Query attributes for every selected live DOM match inside an explicit
    /// frame context (`None` = top/primary frame).
    async fn query_attributes_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<HashMap<String, String>>, RubError>;

    /// Query bounding box through the live read-only DOM authority inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn query_bbox(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<BoundingBox, RubError>;

    /// Query bounding boxes for every selected live DOM match inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn query_bbox_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<BoundingBox>, RubError>;

    /// Capture a live runtime-state snapshot directly from one tab identified
    /// by stable `target_id`, without mutating the current active-tab
    /// authority. Explicit `frame_id` requests must fail closed when the frame
    /// context cannot be resolved truthfully.
    async fn probe_runtime_state_for_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
    ) -> Result<RuntimeStateSnapshot, RubError>;

    /// Check whether the top-frame content of the tab identified by stable
    /// `target_id` contains the requested text, without mutating the current
    /// active-tab authority.
    async fn tab_has_text(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        text: &str,
    ) -> Result<bool, RubError>;

    /// Resolve content/static anchors through the live DOM authority of the
    /// tab identified by stable `target_id`, without mutating the current
    /// active-tab authority.
    async fn find_content_matches_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<ContentFindMatch>, RubError>;

    /// Resolve a CSS selector against the live page and map matching nodes back
    /// to the published snapshot projection.
    async fn find_snapshot_elements_by_selector(
        &self,
        snapshot: &Snapshot,
        selector: &str,
    ) -> Result<Vec<Element>, RubError>;

    /// Filter published snapshot candidates through the live hit-test authority,
    /// keeping only elements that currently expose a hittable point.
    async fn filter_snapshot_elements_by_hit_test(
        &self,
        snapshot: &Snapshot,
        elements: &[Element],
    ) -> Result<Vec<Element>, RubError>;

    /// Resolve interactive snapshot elements that are descendants of one or
    /// more content roots matched by an observation scope.
    async fn find_snapshot_elements_in_observation_scope(
        &self,
        snapshot: &Snapshot,
        scope: &ObservationScope,
    ) -> Result<(Vec<Element>, u32), RubError>;

    /// Resolve content/static anchors through the live DOM authority inside an
    /// explicit frame context (`None` = top/primary frame).
    async fn find_content_matches(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<ContentFindMatch>, RubError>;

    // ── v1.1: Extended Clicks ───────────────────────────────────────

    /// Click at arbitrary viewport coordinates.
    async fn click_xy(&self, x: f64, y: f64) -> Result<InteractionOutcome, RubError>;

    /// Hover (mouseover) on an element.
    async fn hover(&self, element: &Element) -> Result<InteractionOutcome, RubError>;

    /// Double-click at arbitrary viewport coordinates.
    async fn dblclick_xy(&self, x: f64, y: f64) -> Result<InteractionOutcome, RubError>;

    /// Right-click at arbitrary viewport coordinates.
    async fn rightclick_xy(&self, x: f64, y: f64) -> Result<InteractionOutcome, RubError>;

    /// Double-click on an element.
    async fn dblclick(&self, element: &Element) -> Result<InteractionOutcome, RubError>;

    /// Right-click (context menu) on an element.
    async fn rightclick(&self, element: &Element) -> Result<InteractionOutcome, RubError>;

    // ── v1.1: Cookies ───────────────────────────────────────────────

    /// Get browser cookies, optionally filtered by URL.
    async fn get_cookies(&self, url: Option<&str>) -> Result<Vec<Cookie>, RubError>;

    /// Set a cookie.
    async fn set_cookie(&self, cookie: &Cookie) -> Result<(), RubError>;

    /// Delete cookies (all, or filtered by URL).
    async fn delete_cookies(&self, url: Option<&str>) -> Result<(), RubError>;

    // ── v1.1: Web Storage ──────────────────────────────────────────

    /// Snapshot local/session storage for the current frame/current origin.
    async fn storage_snapshot(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
    ) -> Result<StorageSnapshot, RubError>;

    /// Capture a browser-authoritative storage snapshot from the top/primary
    /// frame of the tab identified by stable `target_id`, without mutating the
    /// current active-tab authority.
    async fn storage_snapshot_for_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
    ) -> Result<StorageSnapshot, RubError>;

    /// Set one storage item in the current frame/current origin.
    async fn set_storage_item(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        area: StorageArea,
        key: &str,
        value: &str,
    ) -> Result<StorageSnapshot, RubError>;

    /// Remove one storage item from the current frame/current origin.
    async fn remove_storage_item(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        area: StorageArea,
        key: &str,
    ) -> Result<StorageSnapshot, RubError>;

    /// Clear storage for one area or both areas in the current frame/current origin.
    async fn clear_storage(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        area: Option<StorageArea>,
    ) -> Result<StorageSnapshot, RubError>;

    /// Replace current-origin storage contents with the provided snapshot.
    async fn replace_storage(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        snapshot: &StorageSnapshot,
    ) -> Result<StorageSnapshot, RubError>;

    // ── v1.1: File Upload ───────────────────────────────────────────

    /// Upload a file to a file input element.
    async fn upload_file(
        &self,
        element: &Element,
        path: &str,
    ) -> Result<InteractionOutcome, RubError>;

    // ── v1.1: Select ────────────────────────────────────────────────

    /// Select an option in a <select> element by text or value.
    async fn select_option(
        &self,
        element: &Element,
        value: &str,
    ) -> Result<SelectOutcome, RubError>;

    // ── v1.1: Accessibility ─────────────────────────────────────────

    /// Capture a DOM snapshot augmented with accessibility info.
    async fn snapshot_with_a11y(&self, limit: Option<u32>) -> Result<Snapshot, RubError>;

    /// Capture a DOM snapshot augmented with accessibility info for an explicit frame context.
    async fn snapshot_with_a11y_for_frame(
        &self,
        frame_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Snapshot, RubError>;

    // ── v1.3: Viewport & Highlight ──────────────────────────────────

    /// Get viewport dimensions (width, height) in CSS pixels.
    async fn viewport_dimensions(&self) -> Result<(f64, f64), RubError>;

    /// Inject visual index overlays derived from a published snapshot.
    /// Returns the number of elements highlighted.
    async fn highlight_elements(&self, snapshot: &Snapshot) -> Result<u32, RubError>;

    /// Remove all injected highlight overlays.
    async fn cleanup_highlights(&self) -> Result<(), RubError>;

    /// Capture a DOM snapshot and promote nodes with JavaScript listeners into
    /// the interactive projection. When `include_a11y` is true, accessibility
    /// metadata is merged into the same snapshot.
    async fn snapshot_with_listeners(
        &self,
        limit: Option<u32>,
        include_a11y: bool,
    ) -> Result<Snapshot, RubError>;

    /// Capture a DOM snapshot for an explicit frame context and promote nodes with JS listeners.
    async fn snapshot_with_listeners_for_frame(
        &self,
        frame_id: Option<&str>,
        limit: Option<u32>,
        include_a11y: bool,
    ) -> Result<Snapshot, RubError>;

    /// Replace the browser-side network rule runtime with the current
    /// session-scoped canonical rule list.
    async fn sync_network_rules(&self, rules: &[NetworkRule]) -> Result<(), RubError>;

    /// Capture a live runtime-state snapshot directly from the current page.
    /// Used for per-command traces where session-scoped projections may lag.
    async fn probe_runtime_state(&self) -> Result<RuntimeStateSnapshot, RubError>;

    /// Capture the current canonical frame runtime projection for the active page context.
    async fn probe_frame_runtime(&self) -> Result<FrameRuntimeInfo, RubError>;

    /// List the live frame inventory for the active page context.
    async fn list_frames(&self) -> Result<Vec<FrameInventoryEntry>, RubError>;

    /// List the live frame inventory for one tab identified by stable `target_id`,
    /// without mutating the current active-tab authority.
    async fn list_frames_for_tab(
        &self,
        target_id: &str,
    ) -> Result<Vec<FrameInventoryEntry>, RubError>;

    /// Cancel an in-progress browser download by GUID.
    async fn cancel_download(&self, guid: &str) -> Result<(), RubError>;
}
