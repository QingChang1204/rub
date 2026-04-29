use async_trait::async_trait;
use std::sync::atomic::Ordering;

use rub_core::InteractionOutcome;
use rub_core::error::RubError;
use rub_core::locator::LiveLocator;
use rub_core::model::{
    BoundingBox, ContentFindMatch, Cookie, DialogRuntimeInfo, Element, FrameInventoryEntry,
    FrameRuntimeInfo, HistoryNavigationResult, KeyCombo, LaunchPolicyInfo, LoadStrategy,
    NetworkRule, Page as RubPage, RuntimeStateSnapshot, ScrollDirection, ScrollPosition,
    SelectOutcome, Snapshot, TabInfo, WaitCondition,
};
use rub_core::observation::ObservationScope;
use rub_core::port::BrowserPort;
use rub_core::storage::{StorageArea, StorageSnapshot};

use super::ChromiumAdapter;
use crate::dom;

fn snapshot_target_authority_error(snapshot: &Snapshot, detail: &str) -> RubError {
    RubError::domain_with_context(
        rub_core::error::ErrorCode::StaleSnapshot,
        "Snapshot-bound operations require the original snapshot tab authority",
        serde_json::json!({
            "reason": "snapshot_target_authority_unavailable",
            "detail": detail,
            "snapshot_id": snapshot.snapshot_id,
            "frame_id": snapshot.frame_context.frame_id,
            "target_id": snapshot.frame_context.target_id,
        }),
    )
}

fn element_target_authority_error(element: &Element, detail: &str) -> RubError {
    RubError::domain_with_context(
        rub_core::error::ErrorCode::StaleSnapshot,
        "Snapshot-bound element operations require the original snapshot tab authority",
        serde_json::json!({
            "reason": "snapshot_element_target_authority_unavailable",
            "detail": detail,
            "element_index": element.index,
            "element_ref": element.element_ref,
            "target_id": element.target_id,
        }),
    )
}

async fn page_for_snapshot_authority(
    adapter: &ChromiumAdapter,
    snapshot: &Snapshot,
) -> Result<std::sync::Arc<chromiumoxide::Page>, RubError> {
    let Some(target_id) = snapshot.frame_context.target_id.as_deref() else {
        return Err(snapshot_target_authority_error(
            snapshot,
            "snapshot does not carry tab target authority",
        ));
    };
    adapter
        .manager
        .page_for_target_id(target_id)
        .await
        .map_err(|_| {
            snapshot_target_authority_error(
                snapshot,
                "snapshot target id no longer resolves to a live tab",
            )
        })
}

async fn page_for_element_authority(
    adapter: &ChromiumAdapter,
    element: &Element,
) -> Result<std::sync::Arc<chromiumoxide::Page>, RubError> {
    let Some(target_id) = element.target_id.as_deref() else {
        return Err(element_target_authority_error(
            element,
            "snapshot element does not carry tab target authority",
        ));
    };
    adapter
        .manager
        .page_for_target_id(target_id)
        .await
        .map_err(|_| {
            element_target_authority_error(
                element,
                "snapshot element target id no longer resolves to a live tab",
            )
        })
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
        let page = page_for_element_authority(self, element).await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::click(&page, element, &self.humanize, &dialog_runtime).await
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
        frame_id: Option<&str>,
        direction: ScrollDirection,
        amount: Option<u32>,
    ) -> Result<ScrollPosition, RubError> {
        let page = self.manager.page().await?;
        let frame_context = crate::frame_runtime::resolve_frame_context(&page, frame_id).await?;
        crate::page::scroll(
            &page,
            frame_context.execution_context_id,
            direction,
            amount,
            &self.humanize,
        )
        .await
    }

    async fn back(&self, timeout_ms: u64) -> Result<RubPage, RubError> {
        let page = self.manager.page().await?;
        crate::page::back(&page, std::time::Duration::from_millis(timeout_ms)).await
    }

    async fn back_with_boundary(
        &self,
        timeout_ms: u64,
    ) -> Result<HistoryNavigationResult, RubError> {
        let page = self.manager.page().await?;
        crate::page::back_with_boundary(&page, std::time::Duration::from_millis(timeout_ms)).await
    }

    async fn forward(&self, timeout_ms: u64) -> Result<RubPage, RubError> {
        let page = self.manager.page().await?;
        crate::page::forward(&page, std::time::Duration::from_millis(timeout_ms)).await
    }

    async fn forward_with_boundary(
        &self,
        timeout_ms: u64,
    ) -> Result<HistoryNavigationResult, RubError> {
        let page = self.manager.page().await?;
        crate::page::forward_with_boundary(&page, std::time::Duration::from_millis(timeout_ms))
            .await
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
        self.manager.dialog_runtime_snapshot().await
    }

    fn set_dialog_intercept(
        &self,
        policy: rub_core::model::DialogInterceptPolicy,
    ) -> Result<(), RubError> {
        self.manager.set_dialog_intercept(policy)
    }

    fn clear_dialog_intercept(&self) -> Result<(), RubError> {
        self.manager.clear_dialog_intercept()
    }

    async fn screenshot(&self, full_page: bool) -> Result<Vec<u8>, RubError> {
        let page = self.manager.page().await?;
        crate::page::screenshot(&page, full_page).await
    }

    async fn screenshot_for_snapshot(
        &self,
        snapshot: &Snapshot,
        full_page: bool,
    ) -> Result<Vec<u8>, RubError> {
        let page = page_for_snapshot_authority(self, snapshot).await?;
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

    async fn send_keys(&self, combo: &KeyCombo) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        let expected_target_id = page.target_id().as_ref().to_string();
        let baseline = crate::interaction::capture_active_interaction_baseline(&page).await;
        let page_for_keys = page.clone();
        let combo_for_keys = combo.clone();
        let fence = crate::interaction::await_actuation_or_dialog(
            async move { crate::keyboard::send_keys(&page_for_keys, &combo_for_keys).await },
            dialog_runtime.clone(),
            "send_keys",
            &expected_target_id,
        )
        .await?;
        if let Some(confirmation) = crate::interaction::dialog_confirmation(
            &dialog_runtime,
            &expected_target_id,
            &fence.dialog_baseline,
        )
        .await
        {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(confirmation),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::DialogOpened
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::Indeterminate
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                    "send_keys",
                )),
            });
        }
        let confirmation = crate::interaction::confirm_key_combo(
            &page,
            baseline,
            &dialog_runtime,
            &fence.dialog_baseline,
        )
        .await;
        Ok(InteractionOutcome {
            semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
            element_verified: false,
            actuation: Some(rub_core::model::InteractionActuation::Keyboard),
            confirmation: Some(confirmation),
        })
    }

    async fn send_keys_in_frame(
        &self,
        frame_id: Option<&str>,
        combo: &KeyCombo,
    ) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        let expected_target_id = page.target_id().as_ref().to_string();
        let frame_context = crate::frame_runtime::resolve_frame_context(&page, frame_id).await?;
        let context_id = frame_context.execution_context_id;
        crate::interaction::ensure_frame_owns_page_global_keyboard_focus(&page, context_id).await?;
        let baseline =
            crate::interaction::capture_active_interaction_baseline_in_context(&page, context_id)
                .await;
        let page_for_keys = page.clone();
        let combo_for_keys = combo.clone();
        let context_id_for_keys = context_id;
        let fence = crate::interaction::await_actuation_or_dialog(
            async move {
                crate::interaction::ensure_frame_owns_page_global_keyboard_focus(
                    &page_for_keys,
                    context_id_for_keys,
                )
                .await?;
                crate::keyboard::send_keys(&page_for_keys, &combo_for_keys).await
            },
            dialog_runtime.clone(),
            "send_keys_in_frame",
            &expected_target_id,
        )
        .await?;
        if let Some(confirmation) = crate::interaction::dialog_confirmation(
            &dialog_runtime,
            &expected_target_id,
            &fence.dialog_baseline,
        )
        .await
        {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(confirmation),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::DialogOpened
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::Indeterminate
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                    "send_keys_in_frame",
                )),
            });
        }
        let confirmation = crate::interaction::confirm_key_combo_in_context(
            &page,
            baseline,
            context_id,
            &dialog_runtime,
            &fence.dialog_baseline,
        )
        .await;
        Ok(InteractionOutcome {
            semantic_class: rub_core::model::InteractionSemanticClass::InvokeWorkflow,
            element_verified: false,
            actuation: Some(rub_core::model::InteractionActuation::Keyboard),
            confirmation: Some(confirmation),
        })
    }

    async fn type_text(&self, text: &str) -> Result<InteractionOutcome, RubError> {
        let page = self.manager.page().await?;
        let dialog_runtime = self.manager.dialog_runtime();
        let expected_target_id = page.target_id().as_ref().to_string();
        crate::interaction::ensure_active_text_target_editable(&page).await?;
        let baseline = crate::interaction::capture_active_interaction_baseline(&page).await;
        let page_for_typing = page.clone();
        let text_for_typing = text.to_string();
        let humanize = self.humanize.clone();
        let fence = crate::interaction::await_actuation_or_dialog(
            async move {
                crate::keyboard::type_text(&page_for_typing, &text_for_typing, &humanize).await
            },
            dialog_runtime.clone(),
            "type_text",
            &expected_target_id,
        )
        .await?;
        if let Some(confirmation) = crate::interaction::dialog_confirmation(
            &dialog_runtime,
            &expected_target_id,
            &fence.dialog_baseline,
        )
        .await
        {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(confirmation),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::DialogOpened
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::Indeterminate
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                    "type_text",
                )),
            });
        }
        let confirmation = crate::interaction::confirm_typed_text(
            &page,
            text,
            baseline,
            &dialog_runtime,
            &fence.dialog_baseline,
        )
        .await;
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
        let dialog_runtime = self.manager.dialog_runtime();
        let expected_target_id = page.target_id().as_ref().to_string();
        let frame_context = crate::frame_runtime::resolve_frame_context(&page, frame_id).await?;
        let context_id = frame_context.execution_context_id;
        crate::interaction::ensure_active_text_target_editable_in_context(&page, context_id)
            .await?;
        let baseline =
            crate::interaction::capture_active_interaction_baseline_in_context(&page, context_id)
                .await;
        let page_for_typing = page.clone();
        let text_for_typing = text.to_string();
        let humanize = self.humanize.clone();
        let context_id_for_typing = context_id;
        let fence = crate::interaction::await_actuation_or_dialog(
            async move {
                crate::keyboard::focus_pause(&humanize).await;
                crate::keyboard::type_text_with_pre_dispatch_guard(
                    &page_for_typing,
                    &text_for_typing,
                    &humanize,
                    || async {
                        crate::interaction::ensure_active_text_target_editable_in_context(
                            &page_for_typing,
                            context_id_for_typing,
                        )
                        .await
                    },
                )
                .await
            },
            dialog_runtime.clone(),
            "type_text_in_frame",
            &expected_target_id,
        )
        .await?;
        if let Some(confirmation) = crate::interaction::dialog_confirmation(
            &dialog_runtime,
            &expected_target_id,
            &fence.dialog_baseline,
        )
        .await
        {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(confirmation),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::DialogOpened
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
            });
        }
        if matches!(
            fence.fence,
            crate::interaction::ActuationFence::Indeterminate
        ) {
            return Ok(InteractionOutcome {
                semantic_class: rub_core::model::InteractionSemanticClass::SetValue,
                element_verified: false,
                actuation: Some(rub_core::model::InteractionActuation::Keyboard),
                confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                    "type_text_in_frame",
                )),
            });
        }
        let confirmation = crate::interaction::confirm_typed_text_in_context(
            &page,
            text,
            baseline,
            context_id,
            &dialog_runtime,
            &fence.dialog_baseline,
        )
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
        let page = page_for_element_authority(self, element).await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::controls::type_into(&page, element, text, clear, &self.humanize, &dialog_runtime)
            .await
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
        let page = page_for_element_authority(self, element).await?;
        crate::inspect::get_text(&page, element).await
    }

    async fn get_outer_html(&self, element: &Element) -> Result<String, RubError> {
        let page = page_for_element_authority(self, element).await?;
        crate::inspect::get_outer_html(&page, element).await
    }

    async fn get_value(&self, element: &Element) -> Result<String, RubError> {
        let page = page_for_element_authority(self, element).await?;
        crate::inspect::get_value(&page, element).await
    }

    async fn get_attributes(
        &self,
        element: &Element,
    ) -> Result<std::collections::HashMap<String, String>, RubError> {
        let page = page_for_element_authority(self, element).await?;
        crate::inspect::get_attributes(&page, element).await
    }

    async fn get_bbox(&self, element: &Element) -> Result<BoundingBox, RubError> {
        let page = page_for_element_authority(self, element).await?;
        crate::inspect::get_bbox(&page, element).await
    }

    #[rustfmt::skip]
    async fn query_text(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<String, RubError> { let page = self.manager.page().await?; crate::read_query::query_text(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_text_in_tab(&self, target_id: &str, frame_id: Option<&str>, locator: &LiveLocator) -> Result<String, RubError> { let page = self.manager.page_for_target_id(target_id).await?; crate::read_query::query_text(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_text_many(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<Vec<String>, RubError> { let page = self.manager.page().await?; crate::read_query::query_text_many(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_html(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<String, RubError> { let page = self.manager.page().await?; crate::read_query::query_html(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_html_in_tab(&self, target_id: &str, frame_id: Option<&str>, locator: &LiveLocator) -> Result<String, RubError> { let page = self.manager.page_for_target_id(target_id).await?; crate::read_query::query_html(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_html_many(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<Vec<String>, RubError> { let page = self.manager.page().await?; crate::read_query::query_html_many(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_value(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<String, RubError> { let page = self.manager.page().await?; crate::read_query::query_value(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_value_in_tab(&self, target_id: &str, frame_id: Option<&str>, locator: &LiveLocator) -> Result<String, RubError> { let page = self.manager.page_for_target_id(target_id).await?; crate::read_query::query_value(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_value_many(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<Vec<String>, RubError> { let page = self.manager.page().await?; crate::read_query::query_value_many(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_attributes(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<std::collections::HashMap<String, String>, RubError> { let page = self.manager.page().await?; crate::read_query::query_attributes(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_attributes_in_tab(&self, target_id: &str, frame_id: Option<&str>, locator: &LiveLocator) -> Result<std::collections::HashMap<String, String>, RubError> { let page = self.manager.page_for_target_id(target_id).await?; crate::read_query::query_attributes(&page, frame_id, locator).await }

    #[rustfmt::skip]
    async fn query_attributes_many(&self, frame_id: Option<&str>, locator: &LiveLocator) -> Result<Vec<std::collections::HashMap<String, String>>, RubError> { let page = self.manager.page().await?; crate::read_query::query_attributes_many(&page, frame_id, locator).await }

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
        frame_id: Option<&str>,
    ) -> Result<RuntimeStateSnapshot, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        match frame_id {
            Some(frame_id) => {
                crate::runtime_state::capture_runtime_state_for_explicit_frame(&page, frame_id)
                    .await
            }
            None => Ok(crate::runtime_state::capture_runtime_state(&page).await),
        }
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
        let page = page_for_snapshot_authority(self, snapshot).await?;
        crate::inspect::find_snapshot_elements_by_selector(&page, snapshot, selector).await
    }

    async fn filter_snapshot_elements_by_hit_test(
        &self,
        snapshot: &Snapshot,
        elements: &[Element],
    ) -> Result<Vec<Element>, RubError> {
        let page = page_for_snapshot_authority(self, snapshot).await?;
        crate::targeting::filter_snapshot_elements_by_hit_test(&page, snapshot, elements).await
    }

    async fn find_snapshot_elements_in_observation_scope(
        &self,
        snapshot: &Snapshot,
        scope: &ObservationScope,
    ) -> Result<(Vec<Element>, u32), RubError> {
        let page = page_for_snapshot_authority(self, snapshot).await?;
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
        let page = page_for_element_authority(self, element).await?;
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
        let page = page_for_element_authority(self, element).await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::gestures::dblclick(&page, element, &self.humanize, &dialog_runtime).await
    }

    async fn rightclick(&self, element: &Element) -> Result<InteractionOutcome, RubError> {
        let page = page_for_element_authority(self, element).await?;
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
        let page = page_for_element_authority(self, element).await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::controls::upload_file(&page, element, path, &dialog_runtime).await
    }

    async fn select_option(
        &self,
        element: &Element,
        value: &str,
    ) -> Result<SelectOutcome, RubError> {
        let page = page_for_element_authority(self, element).await?;
        let dialog_runtime = self.manager.dialog_runtime();
        crate::controls::select_option(&page, element, value, &dialog_runtime).await
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

    async fn viewport_dimensions(&self) -> Result<(f64, f64), RubError> {
        let page = self.manager.page().await?;
        crate::page::viewport_dimensions(&page).await
    }

    async fn highlight_elements(&self, snapshot: &Snapshot) -> Result<u32, RubError> {
        let page = page_for_snapshot_authority(self, snapshot).await?;
        crate::page::highlight_elements(&page, snapshot).await
    }

    async fn cleanup_highlights(&self) -> Result<(), RubError> {
        let page = self.manager.page().await?;
        crate::page::cleanup_highlights(&page).await
    }

    async fn cleanup_highlights_for_snapshot(&self, snapshot: &Snapshot) -> Result<(), RubError> {
        let page = page_for_snapshot_authority(self, snapshot).await?;
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

    async fn list_frames_for_tab(
        &self,
        target_id: &str,
    ) -> Result<Vec<FrameInventoryEntry>, RubError> {
        let page = self.manager.page_for_target_id(target_id).await?;
        crate::frame_runtime::list_frame_inventory(&page).await
    }

    async fn cancel_download(&self, guid: &str) -> Result<(), RubError> {
        self.manager.cancel_download(guid).await
    }
}
