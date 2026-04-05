//! ChromiumAdapter — implements BrowserPort trait using chromiumoxide.
//! Bridges the hexagonal architecture boundary.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rub_core::InteractionOutcome;
use rub_core::error::RubError;
use rub_core::locator::LiveLocator;
use rub_core::model::{
    BoundingBox, ContentFindMatch, Cookie, DialogRuntimeInfo, Element, FrameInventoryEntry,
    FrameRuntimeInfo, KeyCombo, LaunchPolicyInfo, LoadStrategy, NetworkRule, Page as RubPage,
    RuntimeStateSnapshot, ScrollDirection, ScrollPosition, SelectOutcome, Snapshot, TabInfo,
    WaitCondition,
};
use rub_core::observation::ObservationScope;
use rub_core::port::BrowserPort;
use rub_core::storage::{StorageArea, StorageSnapshot};

use crate::browser::BrowserManager;
use crate::dom;

/// Adapter connecting BrowserPort to chromiumoxide.
pub struct ChromiumAdapter {
    manager: Arc<BrowserManager>,
    dom_epoch: Arc<AtomicU64>,
    humanize: crate::humanize::HumanizeConfig,
}

impl ChromiumAdapter {
    pub fn new(
        manager: Arc<BrowserManager>,
        dom_epoch: Arc<AtomicU64>,
        humanize: crate::humanize::HumanizeConfig,
    ) -> Self {
        Self {
            manager,
            dom_epoch,
            humanize,
        }
    }

    fn projected_launch_policy(&self) -> LaunchPolicyInfo {
        let mut launch_policy = self.manager.launch_policy_info();
        launch_policy.humanize_enabled = Some(self.humanize.enabled);
        launch_policy.humanize_speed = Some(
            match self.humanize.speed {
                crate::humanize::HumanizeSpeed::Fast => "fast",
                crate::humanize::HumanizeSpeed::Normal => "normal",
                crate::humanize::HumanizeSpeed::Slow => "slow",
            }
            .to_string(),
        );
        if self.humanize.enabled {
            launch_policy.stealth_level = Some("L2".to_string());
        }
        launch_policy
    }
}

#[async_trait]
impl BrowserPort for ChromiumAdapter {
    async fn navigate(
        &self,
        url: &str,
        strategy: LoadStrategy,
        timeout_ms: u64,
    ) -> Result<RubPage, RubError> {
        let page = self.manager.page().await?;
        crate::page::navigate(
            &page,
            url,
            strategy,
            std::time::Duration::from_millis(timeout_ms),
        )
        .await
    }

    async fn snapshot(&self, limit: Option<u32>) -> Result<Snapshot, RubError> {
        let page = self.manager.page().await?;
        let epoch = self.dom_epoch.load(Ordering::SeqCst);
        dom::build_snapshot(&page, epoch, limit).await
    }

    async fn snapshot_for_frame(
        &self,
        frame_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Snapshot, RubError> {
        let page = self.manager.page().await?;
        let epoch = self.dom_epoch.load(Ordering::SeqCst);
        dom::build_snapshot_for_frame(&page, epoch, limit, frame_id).await
    }

    async fn click(&self, element: &Element) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::click(&page, element, &self.humanize, &dialog_runtime).await
    }

    async fn input(
        &self,
        element: &Element,
        text: &str,
        clear: bool,
    ) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        crate::controls::input_text(&page, element, text, clear, &self.humanize).await
    }

    async fn execute_js(&self, code: &str) -> Result<serde_json::Value, RubError> {
        let page = self.manager.page().await?;
        crate::evaluation::execute_js(&page, code).await
    }

    async fn execute_js_in_frame(
        &self,
        frame_id: Option<&str>,
        code: &str,
    ) -> Result<serde_json::Value, RubError> {
        let page = self.manager.page().await?;
        let frame_context = crate::frame_runtime::resolve_frame_context(&page, frame_id).await?;
        crate::evaluation::execute_js_in_context(&page, code, frame_context.execution_context_id)
            .await
    }

    async fn scroll(
        &self,
        direction: ScrollDirection,
        amount: Option<u32>,
    ) -> Result<ScrollPosition, RubError> {
        let page = self.manager.page().await?;
        crate::page::scroll(&page, direction, amount, &self.humanize).await
    }

    async fn back(&self, timeout_ms: u64) -> Result<RubPage, RubError> {
        let page = self.manager.page().await?;
        crate::page::back(&page, std::time::Duration::from_millis(timeout_ms)).await
    }

    async fn forward(&self, timeout_ms: u64) -> Result<RubPage, RubError> {
        let page = self.manager.page().await?;
        crate::page::forward(&page, std::time::Duration::from_millis(timeout_ms)).await
    }

    async fn reload(&self, strategy: LoadStrategy, timeout_ms: u64) -> Result<RubPage, RubError> {
        let page = self.manager.page().await?;
        crate::page::reload(
            &page,
            strategy,
            std::time::Duration::from_millis(timeout_ms),
        )
        .await
    }

    async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> Result<(), RubError> {
        self.manager.handle_dialog(accept, prompt_text).await
    }

    async fn dialog_runtime(&self) -> Result<DialogRuntimeInfo, RubError> {
        Ok(self.manager.dialog_runtime().read().await.clone())
    }

    fn set_dialog_intercept(&self, policy: rub_core::model::DialogInterceptPolicy) {
        self.manager.set_dialog_intercept(policy);
    }

    fn clear_dialog_intercept(&self) {
        self.manager.clear_dialog_intercept();
    }

    async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>, RubError> {
        let page = self.manager.page().await?;
        crate::page::screenshot(&page, full_page).await
    }

    async fn health_check(&self) -> Result<(), RubError> {
        self.manager.health_check().await
    }

    fn launch_policy(&self) -> LaunchPolicyInfo {
        self.projected_launch_policy()
    }

    async fn close(&self) -> Result<(), RubError> {
        self.manager.close().await
    }

    async fn elevate_to_visible(&self) -> Result<LaunchPolicyInfo, RubError> {
        self.manager.elevate_to_visible().await
    }

    // ── v1.1 Stubs ──────────────────────────────────────────────────

    async fn send_keys(&self, combo: &KeyCombo) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let baseline = crate::interaction::capture_active_interaction_baseline(&page).await;
        crate::keyboard::send_keys(&page, combo).await?;
        let confirmation = crate::interaction::confirm_key_combo(&page, baseline).await;
        Ok(InteractionOutcome {
            semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
            element_verified: false,
            actuation: Some(rub_core::model::InteractionActuation::Keyboard),
            confirmation: Some(confirmation),
        })
    }

    async fn type_text(&self, text: &str) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        crate::interaction::ensure_active_text_target_editable(&page).await?;
        let baseline = crate::interaction::capture_active_interaction_baseline(&page).await;
        crate::keyboard::type_text(&page, text, &self.humanize).await?;
        let confirmation = crate::interaction::confirm_typed_text(&page, text, baseline).await;
        Ok(InteractionOutcome {
            semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
            element_verified: false,
            actuation: Some(rub_core::model::InteractionActuation::Keyboard),
            confirmation: Some(confirmation),
        })
    }

    async fn type_text_in_frame(
        &self,
        frame_id: Option<&str>,
        text: &str,
    ) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let frame_context = crate::frame_runtime::resolve_frame_context(&page, frame_id).await?;
        let context_id = frame_context.execution_context_id;
        crate::interaction::ensure_active_text_target_editable_in_context(&page, context_id)
            .await?;
        let baseline =
            crate::interaction::capture_active_interaction_baseline_in_context(&page, context_id)
                .await;
        crate::keyboard::type_text(&page, text, &self.humanize).await?;
        let confirmation =
            crate::interaction::confirm_typed_text_in_context(&page, text, baseline, context_id)
                .await;
        Ok(InteractionOutcome {
            semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
            element_verified: false,
            actuation: Some(rub_core::model::InteractionActuation::Keyboard),
            confirmation: Some(confirmation),
        })
    }

    async fn type_into(
        &self,
        element: &Element,
        text: &str,
        clear: bool,
    ) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        crate::controls::type_into(&page, element, text, clear, &self.humanize).await
    }

    async fn wait_for(&self, condition: &WaitCondition) -> Result<(), RubError> {
        let page = self.manager.page().await?;
        crate::waiting::wait_for_condition(&page, condition).await
    }

    async fn list_tabs(&self) -> Result<Vec<TabInfo>, RubError> {
        self.manager.tab_list().await
    }

    async fn switch_tab(&self, index: u32) -> Result<TabInfo, RubError> {
        self.manager.switch_to_tab(index).await
    }

    async fn close_tab(&self, index: Option<u32>) -> Result<Vec<TabInfo>, RubError> {
        self.manager.close_tab_at(index).await
    }

    async fn get_title(&self) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_title(&page).await
    }

    async fn get_html(&self, selector: Option<&str>) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_html(&page, selector).await
    }

    async fn get_text(&self, element: &Element) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_text(&page, element).await
    }

    async fn get_outer_html(&self, element: &Element) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_outer_html(&page, element).await
    }

    async fn get_value(&self, element: &Element) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_value(&page, element).await
    }

    async fn get_attributes(
        &self,
        element: &Element,
    ) -> Result<std::collections::HashMap<String, String>, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_attributes(&page, element).await
    }

    async fn get_bbox(&self, element: &Element) -> Result<BoundingBox, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::get_bbox(&page, element).await
    }

    async fn query_text(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_text(&page, frame_id, locator).await
    }

    async fn query_text_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::read_query::query_text(&page, frame_id, locator).await
    }

    async fn query_text_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<String>, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_text_many(&page, frame_id, locator).await
    }

    async fn query_html(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_html(&page, frame_id, locator).await
    }

    async fn query_html_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::read_query::query_html(&page, frame_id, locator).await
    }

    async fn query_html_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<String>, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_html_many(&page, frame_id, locator).await
    }

    async fn query_value(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_value(&page, frame_id, locator).await
    }

    async fn query_value_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<String, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::read_query::query_value(&page, frame_id, locator).await
    }

    async fn query_value_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<String>, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_value_many(&page, frame_id, locator).await
    }

    async fn query_attributes(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<std::collections::HashMap<String, String>, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_attributes(&page, frame_id, locator).await
    }

    async fn query_attributes_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<std::collections::HashMap<String, String>, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::read_query::query_attributes(&page, frame_id, locator).await
    }

    async fn query_attributes_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<std::collections::HashMap<String, String>>, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_attributes_many(&page, frame_id, locator).await
    }

    async fn query_bbox(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<BoundingBox, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_bbox(&page, frame_id, locator).await
    }

    async fn query_bbox_many(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<BoundingBox>, RubError> {
        let page = self.manager.page().await?;
        crate::read_query::query_bbox_many(&page, frame_id, locator).await
    }

    async fn probe_runtime_state_for_tab(
        &self,
        target_id: &str,
        _frame_id: Option<&str>,
    ) -> Result<RuntimeStateSnapshot, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        Ok(crate::runtime_state::capture_runtime_state(&page).await)
    }

    async fn tab_has_text(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        text: &str,
    ) -> Result<bool, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::trigger_probe::page_has_text(&page, frame_id, text).await
    }

    async fn find_content_matches_in_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<ContentFindMatch>, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::content_find::find_content_matches(&page, frame_id, locator).await
    }

    async fn find_snapshot_elements_by_selector(
        &self,
        snapshot: &Snapshot,
        selector: &str,
    ) -> Result<Vec<Element>, RubError> {
        let page = self.manager.page().await?;
        crate::inspect::find_snapshot_elements_by_selector(&page, snapshot, selector).await
    }

    async fn find_snapshot_elements_in_observation_scope(
        &self,
        snapshot: &Snapshot,
        scope: &ObservationScope,
    ) -> Result<(Vec<Element>, u32), RubError> {
        let page = self.manager.page().await?;
        crate::observation_scope::find_snapshot_elements_in_observation_scope(
            &page, snapshot, scope,
        )
        .await
    }

    async fn find_content_matches(
        &self,
        frame_id: Option<&str>,
        locator: &LiveLocator,
    ) -> Result<Vec<ContentFindMatch>, RubError> {
        let page = self.manager.page().await?;
        crate::content_find::find_content_matches(&page, frame_id, locator).await
    }

    async fn click_xy(&self, x: f64, y: f64) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::click_xy(&page, x, y, &self.humanize, &dialog_runtime).await
    }

    async fn hover(&self, element: &Element) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        crate::gestures::hover(&page, element, &self.humanize).await
    }

    async fn dblclick_xy(&self, x: f64, y: f64) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::dblclick_xy(&page, x, y, &self.humanize, &dialog_runtime).await
    }

    async fn rightclick_xy(&self, x: f64, y: f64) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::rightclick_xy(&page, x, y, &self.humanize, &dialog_runtime).await
    }

    async fn dblclick(&self, element: &Element) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::dblclick(&page, element, &self.humanize, &dialog_runtime).await
    }

    async fn rightclick(&self, element: &Element) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::rightclick(&page, element, &self.humanize, &dialog_runtime).await
    }

    async fn get_cookies(&self, url: Option<&str>) -> Result<Vec<Cookie>, RubError> {
        let page = self.manager.page().await?;
        crate::cookies::get(&page, url).await
    }

    async fn set_cookie(&self, cookie: &Cookie) -> Result<(), RubError> {
        let page = self.manager.page().await?;
        crate::cookies::set(&page, cookie).await
    }

    async fn delete_cookies(&self, url: Option<&str>) -> Result<(), RubError> {
        let page = self.manager.page().await?;
        crate::cookies::delete(&page, url).await
    }

    async fn storage_snapshot(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
    ) -> Result<StorageSnapshot, RubError> {
        let page = self.manager.page().await?;
        crate::storage::capture_storage_snapshot(&page, frame_id, expected_origin).await
    }

    async fn storage_snapshot_for_tab(
        &self,
        target_id: &str,
        frame_id: Option<&str>,
    ) -> Result<StorageSnapshot, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::storage::capture_storage_snapshot(&page, frame_id, None).await
    }

    async fn set_storage_item(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        area: StorageArea,
        key: &str,
        value: &str,
    ) -> Result<StorageSnapshot, RubError> {
        let page = self.manager.page().await?;
        crate::storage::set_storage_item(&page, frame_id, expected_origin, area, key, value).await
    }

    async fn remove_storage_item(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        area: StorageArea,
        key: &str,
    ) -> Result<StorageSnapshot, RubError> {
        let page = self.manager.page().await?;
        crate::storage::remove_storage_item(&page, frame_id, expected_origin, area, key).await
    }

    async fn clear_storage(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        area: Option<StorageArea>,
    ) -> Result<StorageSnapshot, RubError> {
        let page = self.manager.page().await?;
        crate::storage::clear_storage(&page, frame_id, expected_origin, area).await
    }

    async fn replace_storage(
        &self,
        frame_id: Option<&str>,
        expected_origin: Option<&str>,
        snapshot: &StorageSnapshot,
    ) -> Result<StorageSnapshot, RubError> {
        let page = self.manager.page().await?;
        crate::storage::replace_storage(&page, frame_id, expected_origin, snapshot).await
    }

    async fn upload_file(
        &self,
        element: &Element,
        path: &str,
    ) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        crate::controls::upload_file(&page, element, path).await
    }

    async fn select_option(
        &self,
        element: &Element,
        value: &str,
    ) -> Result<SelectOutcome, RubError> {
        let page = self.manager.page().await?;
        crate::controls::select_option(&page, element, value).await
    }

    async fn snapshot_with_a11y(&self, limit: Option<u32>) -> Result<Snapshot, RubError> {
        let page = self.manager.page().await?;
        let epoch = self.dom_epoch.load(Ordering::SeqCst);
        dom::build_snapshot_with_a11y(&page, epoch, limit).await
    }

    async fn snapshot_with_a11y_for_frame(
        &self,
        frame_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Snapshot, RubError> {
        let page = self.manager.page().await?;
        let epoch = self.dom_epoch.load(Ordering::SeqCst);
        dom::build_snapshot_with_a11y_for_frame(&page, epoch, limit, frame_id).await
    }

    // ── v1.3: Viewport & Highlight ──────────────────────────────────

    async fn viewport_dimensions(&self) -> Result<(f64, f64), RubError> {
        let page = self.manager.page().await?;
        crate::page::viewport_dimensions(&page).await
    }

    async fn highlight_elements(&self, snapshot: &Snapshot) -> Result<u32, RubError> {
        let page = self.manager.page().await?;
        crate::page::highlight_elements(&page, snapshot).await
    }

    async fn cleanup_highlights(&self) -> Result<(), RubError> {
        let page = self.manager.page().await?;
        crate::page::cleanup_highlights(&page).await
    }

    async fn snapshot_with_listeners(
        &self,
        limit: Option<u32>,
        include_a11y: bool,
    ) -> Result<Snapshot, RubError> {
        let page = self.manager.page().await?;
        let epoch = self.dom_epoch.load(Ordering::SeqCst);
        dom::build_snapshot_with_listeners(&page, epoch, limit, include_a11y).await
    }

    async fn snapshot_with_listeners_for_frame(
        &self,
        frame_id: Option<&str>,
        limit: Option<u32>,
        include_a11y: bool,
    ) -> Result<Snapshot, RubError> {
        let page = self.manager.page().await?;
        let epoch = self.dom_epoch.load(Ordering::SeqCst);
        dom::build_snapshot_with_listeners_for_frame(&page, epoch, limit, include_a11y, frame_id)
            .await
    }

    async fn sync_network_rules(&self, rules: &[NetworkRule]) -> Result<(), RubError> {
        self.manager.sync_network_rules(rules.to_vec()).await
    }

    async fn probe_runtime_state(&self) -> Result<RuntimeStateSnapshot, RubError> {
        let page = self.manager.page().await?;
        Ok(crate::runtime_state::capture_runtime_state(&page).await)
    }

    async fn probe_frame_runtime(&self) -> Result<FrameRuntimeInfo, RubError> {
        let page = self.manager.page().await?;
        crate::frame_runtime::capture_frame_runtime(&page).await
    }

    async fn list_frames(&self) -> Result<Vec<FrameInventoryEntry>, RubError> {
        let page = self.manager.page().await?;
        crate::frame_runtime::list_frame_inventory(&page).await
    }

    async fn cancel_download(&self, guid: &str) -> Result<(), RubError> {
        self.manager.cancel_download(guid).await
    }
}

#[cfg(test)]
mod tests {
    use super::ChromiumAdapter;
    use crate::browser::{BrowserLaunchOptions, BrowserManager};
    use crate::humanize::{HumanizeConfig, HumanizeSpeed};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn projected_launch_policy_reports_l2_when_humanize_enabled() {
        let manager = Arc::new(BrowserManager::new(BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        }));
        let adapter = ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            HumanizeConfig {
                enabled: true,
                speed: HumanizeSpeed::Slow,
            },
        );

        let launch_policy = adapter.projected_launch_policy();
        assert_eq!(launch_policy.stealth_level.as_deref(), Some("L2"));
        assert_eq!(launch_policy.humanize_enabled, Some(true));
        assert_eq!(launch_policy.humanize_speed.as_deref(), Some("slow"));
    }
}
