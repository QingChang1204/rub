mod args;
mod html;
mod result_projection;
mod text_like;

use self::args::{ExecArgs, GetCommand, GetReadKind, InspectReadArgs, InspectReadKind};
use self::html::{cmd_get_html, cmd_inspect_html};
use self::result_projection::{page_subject, read_payload, scalar_read_result};
use self::text_like::{cmd_get_text_like, cmd_inspect_text_like};
use super::request_args::parse_json_args;
use super::*;
use crate::router::timeout_projection::record_mutating_possible_commit_timeout_projection;
use rub_core::locator::CanonicalLocator;

pub(super) async fn cmd_exec(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ExecArgs = parse_json_args(args, "exec")?;
    let frame_id = super::frame_scope::effective_request_frame_id(router, args, state).await?;
    record_mutating_possible_commit_timeout_projection(
        "exec",
        serde_json::json!({
            "kind": "script_execution_possible_commit",
            "frame_id": frame_id.as_deref(),
            "same_command_retry_requires_same_command_id": true,
        }),
    );
    let result = router
        .browser
        .execute_js_in_frame(frame_id.as_deref(), &parsed.code)
        .await?;
    let mut subject = serde_json::Map::new();
    subject.insert(
        "kind".to_string(),
        serde_json::Value::String("script_execution".to_string()),
    );
    if let Some(frame_id) = frame_id {
        subject.insert("frame_id".to_string(), serde_json::Value::String(frame_id));
    }
    Ok(serde_json::json!({
        "subject": serde_json::Value::Object(subject),
        "result": result,
    }))
}

pub(super) async fn cmd_get(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match parse_json_args::<GetCommand>(args, "get")? {
        GetCommand::Title => {
            let title = router.browser.get_title().await?;
            Ok(read_payload(
                page_subject(None),
                scalar_read_result("title", serde_json::json!(title)),
            ))
        }
        GetCommand::Html(parsed) => cmd_get_html(router, args, parsed, state).await,
        GetCommand::Text(parsed) => {
            cmd_get_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                GetReadKind::Text,
                GetReadKind::Text.command_name(),
            )
            .await
        }
        GetCommand::Value(parsed) => {
            cmd_get_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                GetReadKind::Value,
                GetReadKind::Value.command_name(),
            )
            .await
        }
        GetCommand::Attributes(parsed) => {
            cmd_get_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                GetReadKind::Attributes,
                GetReadKind::Attributes.command_name(),
            )
            .await
        }
        GetCommand::Bbox(parsed) => {
            cmd_get_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                GetReadKind::Bbox,
                GetReadKind::Bbox.command_name(),
            )
            .await
        }
    }
}

pub(super) async fn cmd_inspect_read(
    router: &DaemonRouter,
    args: &serde_json::Value,
    inspect_sub: &str,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    // "sub" has already been stripped by cmd_inspect; dispatch using the explicit
    // inspect_sub parameter instead of re-reading it from args via a serde tag enum.
    let parsed: InspectReadArgs = parse_json_args(args, "inspect")?;
    match inspect_sub {
        "text" => {
            cmd_inspect_text_like(router, args, parsed, deadline, state, InspectReadKind::Text)
                .await
        }
        "html" => cmd_inspect_html(router, args, parsed, deadline, state).await,
        "value" => {
            cmd_inspect_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                InspectReadKind::Value,
            )
            .await
        }
        "attributes" => {
            cmd_inspect_text_like(
                router,
                args,
                parsed,
                deadline,
                state,
                InspectReadKind::Attributes,
            )
            .await
        }
        "bbox" => {
            cmd_inspect_text_like(router, args, parsed, deadline, state, InspectReadKind::Bbox)
                .await
        }
        other => Err(RubError::Internal(format!(
            "Unexpected inspect read sub-command reached handler: '{other}'"
        ))),
    }
}

pub(super) fn reject_live_many_locator_selection(
    locator: Option<&CanonicalLocator>,
    kind: &str,
) -> Result<(), RubError> {
    if locator.and_then(CanonicalLocator::selection).is_none() {
        return Ok(());
    }

    Err(RubError::domain_with_context_and_suggestion(
        ErrorCode::InvalidInput,
        format!("inspect {kind} --many does not support --first, --last, or --nth"),
        serde_json::json!({
            "kind": kind,
            "many": true,
            "locator": locator,
        }),
        "Drop --first/--last/--nth to read every live match, or remove --many to inspect one selected element",
    ))
}

pub(super) fn reject_snapshot_without_locator(
    command_name: &str,
    snapshot_id: Option<&str>,
    locator: Option<&CanonicalLocator>,
) -> Result<(), RubError> {
    if snapshot_id.is_none() || locator.is_some() {
        return Ok(());
    }

    Err(RubError::domain_with_context_and_suggestion(
        ErrorCode::InvalidInput,
        format!("{command_name} --snapshot requires --index, --ref, or a locator"),
        serde_json::json!({
            "command": command_name,
            "snapshot_id": snapshot_id,
        }),
        "Add --index/--ref or a locator to stay on snapshot authority, or drop --snapshot to inspect the current live page/frame",
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ExecArgs, GetCommand, InspectReadArgs, cmd_exec, cmd_inspect_read, page_subject,
        read_payload, reject_live_many_locator_selection, reject_snapshot_without_locator,
        scalar_read_result,
    };
    use crate::router::DaemonRouter;
    use crate::router::TransactionDeadline;
    use crate::router::query::result_projection::{live_read_subject, multi_read_result};
    use crate::router::request_args::parse_json_args;
    use crate::session::SessionState;
    use async_trait::async_trait;
    use rub_core::error::ErrorCode;
    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        BoundingBox, ContentFindMatch, Cookie, DialogInterceptPolicy, DialogRuntimeInfo, Element,
        FrameContextInfo, FrameInventoryEntry, FrameRuntimeInfo, InteractionOutcome, KeyCombo,
        LaunchPolicyInfo, LoadStrategy, NetworkRule, Page, RuntimeStateSnapshot, ScrollDirection,
        ScrollPosition, SelectOutcome, Snapshot, TabInfo, WaitCondition,
    };
    use rub_core::observation::ObservationScope;
    use rub_core::port::BrowserPort;
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    macro_rules! unexpected_browser_call {
        ($name:expr) => {
            panic!("unexpected BrowserPort call in query test: {}", $name)
        };
    }

    struct QueryFrameBrowser {
        frames: Vec<FrameInventoryEntry>,
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

    #[async_trait]
    impl BrowserPort for QueryFrameBrowser {
        async fn navigate(
            &self,
            _url: &str,
            _strategy: LoadStrategy,
            _timeout_ms: u64,
        ) -> Result<Page, rub_core::error::RubError> {
            unexpected_browser_call!("navigate")
        }

        async fn snapshot(
            &self,
            _limit: Option<u32>,
        ) -> Result<Snapshot, rub_core::error::RubError> {
            unexpected_browser_call!("snapshot")
        }

        async fn snapshot_for_frame(
            &self,
            _frame_id: Option<&str>,
            _limit: Option<u32>,
        ) -> Result<Snapshot, rub_core::error::RubError> {
            unexpected_browser_call!("snapshot_for_frame")
        }

        async fn click(
            &self,
            _element: &Element,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("click")
        }

        async fn execute_js(
            &self,
            _code: &str,
        ) -> Result<serde_json::Value, rub_core::error::RubError> {
            unexpected_browser_call!("execute_js")
        }

        async fn execute_js_in_frame(
            &self,
            frame_id: Option<&str>,
            _code: &str,
        ) -> Result<serde_json::Value, rub_core::error::RubError> {
            Ok(serde_json::json!(frame_id.unwrap_or("top")))
        }

        async fn back(&self, _timeout_ms: u64) -> Result<Page, rub_core::error::RubError> {
            unexpected_browser_call!("back")
        }

        async fn forward(&self, _timeout_ms: u64) -> Result<Page, rub_core::error::RubError> {
            unexpected_browser_call!("forward")
        }

        async fn reload(
            &self,
            _strategy: LoadStrategy,
            _timeout_ms: u64,
        ) -> Result<Page, rub_core::error::RubError> {
            unexpected_browser_call!("reload")
        }

        async fn handle_dialog(
            &self,
            _accept: bool,
            _prompt_text: Option<String>,
        ) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("handle_dialog")
        }

        async fn dialog_runtime(&self) -> Result<DialogRuntimeInfo, rub_core::error::RubError> {
            unexpected_browser_call!("dialog_runtime")
        }

        fn set_dialog_intercept(
            &self,
            _policy: DialogInterceptPolicy,
        ) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("set_dialog_intercept")
        }

        fn clear_dialog_intercept(&self) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("clear_dialog_intercept")
        }

        async fn scroll(
            &self,
            _frame_id: Option<&str>,
            _direction: ScrollDirection,
            _amount: Option<u32>,
        ) -> Result<ScrollPosition, rub_core::error::RubError> {
            unexpected_browser_call!("scroll")
        }

        async fn screenshot(&self, _full_page: bool) -> Result<Vec<u8>, rub_core::error::RubError> {
            unexpected_browser_call!("screenshot")
        }

        async fn health_check(&self) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("health_check")
        }

        fn launch_policy(&self) -> LaunchPolicyInfo {
            unexpected_browser_call!("launch_policy")
        }

        async fn close(&self) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("close")
        }

        async fn elevate_to_visible(&self) -> Result<LaunchPolicyInfo, rub_core::error::RubError> {
            unexpected_browser_call!("elevate_to_visible")
        }

        async fn send_keys(
            &self,
            _combo: &KeyCombo,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("send_keys")
        }

        async fn send_keys_in_frame(
            &self,
            _frame_id: Option<&str>,
            _combo: &KeyCombo,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("send_keys_in_frame")
        }

        async fn type_text(
            &self,
            _text: &str,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("type_text")
        }

        async fn type_text_in_frame(
            &self,
            _frame_id: Option<&str>,
            _text: &str,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("type_text_in_frame")
        }

        async fn type_into(
            &self,
            _element: &Element,
            _text: &str,
            _clear: bool,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("type_into")
        }

        async fn wait_for(
            &self,
            _condition: &WaitCondition,
        ) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("wait_for")
        }

        async fn list_tabs(&self) -> Result<Vec<TabInfo>, rub_core::error::RubError> {
            unexpected_browser_call!("list_tabs")
        }

        async fn switch_tab(&self, _index: u32) -> Result<TabInfo, rub_core::error::RubError> {
            unexpected_browser_call!("switch_tab")
        }

        async fn close_tab(
            &self,
            _index: Option<u32>,
        ) -> Result<Vec<TabInfo>, rub_core::error::RubError> {
            unexpected_browser_call!("close_tab")
        }

        async fn get_title(&self) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("get_title")
        }

        async fn get_html(
            &self,
            _selector: Option<&str>,
        ) -> Result<String, rub_core::error::RubError> {
            Ok("top".to_string())
        }

        async fn get_text(&self, _element: &Element) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("get_text")
        }

        async fn get_outer_html(
            &self,
            _element: &Element,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("get_outer_html")
        }

        async fn get_value(&self, _element: &Element) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("get_value")
        }

        async fn get_attributes(
            &self,
            _element: &Element,
        ) -> Result<HashMap<String, String>, rub_core::error::RubError> {
            unexpected_browser_call!("get_attributes")
        }

        async fn get_bbox(
            &self,
            _element: &Element,
        ) -> Result<BoundingBox, rub_core::error::RubError> {
            unexpected_browser_call!("get_bbox")
        }

        async fn query_text(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("query_text")
        }

        async fn query_text_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("query_text_in_tab")
        }

        async fn query_text_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<String>, rub_core::error::RubError> {
            unexpected_browser_call!("query_text_many")
        }

        async fn query_html(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("query_html")
        }

        async fn query_html_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("query_html_in_tab")
        }

        async fn query_html_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<String>, rub_core::error::RubError> {
            unexpected_browser_call!("query_html_many")
        }

        async fn query_value(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("query_value")
        }

        async fn query_value_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<String, rub_core::error::RubError> {
            unexpected_browser_call!("query_value_in_tab")
        }

        async fn query_value_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<String>, rub_core::error::RubError> {
            unexpected_browser_call!("query_value_many")
        }

        async fn query_attributes(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<HashMap<String, String>, rub_core::error::RubError> {
            unexpected_browser_call!("query_attributes")
        }

        async fn query_attributes_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<HashMap<String, String>, rub_core::error::RubError> {
            unexpected_browser_call!("query_attributes_in_tab")
        }

        async fn query_attributes_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<HashMap<String, String>>, rub_core::error::RubError> {
            unexpected_browser_call!("query_attributes_many")
        }

        async fn query_bbox(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<BoundingBox, rub_core::error::RubError> {
            unexpected_browser_call!("query_bbox")
        }

        async fn query_bbox_many(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<BoundingBox>, rub_core::error::RubError> {
            unexpected_browser_call!("query_bbox_many")
        }

        async fn probe_runtime_state_for_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
        ) -> Result<RuntimeStateSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("probe_runtime_state_for_tab")
        }

        async fn tab_has_text(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _text: &str,
        ) -> Result<bool, rub_core::error::RubError> {
            unexpected_browser_call!("tab_has_text")
        }

        async fn find_content_matches_in_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<ContentFindMatch>, rub_core::error::RubError> {
            unexpected_browser_call!("find_content_matches_in_tab")
        }

        async fn find_snapshot_elements_by_selector(
            &self,
            _snapshot: &Snapshot,
            _selector: &str,
        ) -> Result<Vec<Element>, rub_core::error::RubError> {
            unexpected_browser_call!("find_snapshot_elements_by_selector")
        }

        async fn filter_snapshot_elements_by_hit_test(
            &self,
            _snapshot: &Snapshot,
            _elements: &[Element],
        ) -> Result<Vec<Element>, rub_core::error::RubError> {
            unexpected_browser_call!("filter_snapshot_elements_by_hit_test")
        }

        async fn find_snapshot_elements_in_observation_scope(
            &self,
            _snapshot: &Snapshot,
            _scope: &ObservationScope,
        ) -> Result<(Vec<Element>, u32), rub_core::error::RubError> {
            unexpected_browser_call!("find_snapshot_elements_in_observation_scope")
        }

        async fn find_content_matches(
            &self,
            _frame_id: Option<&str>,
            _locator: &rub_core::locator::LiveLocator,
        ) -> Result<Vec<ContentFindMatch>, rub_core::error::RubError> {
            unexpected_browser_call!("find_content_matches")
        }

        async fn click_xy(
            &self,
            _x: f64,
            _y: f64,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("click_xy")
        }

        async fn hover(
            &self,
            _element: &Element,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("hover")
        }

        async fn dblclick_xy(
            &self,
            _x: f64,
            _y: f64,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("dblclick_xy")
        }

        async fn rightclick_xy(
            &self,
            _x: f64,
            _y: f64,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("rightclick_xy")
        }

        async fn dblclick(
            &self,
            _element: &Element,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("dblclick")
        }

        async fn rightclick(
            &self,
            _element: &Element,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("rightclick")
        }

        async fn get_cookies(
            &self,
            _url: Option<&str>,
        ) -> Result<Vec<Cookie>, rub_core::error::RubError> {
            unexpected_browser_call!("get_cookies")
        }

        async fn set_cookie(&self, _cookie: &Cookie) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("set_cookie")
        }

        async fn delete_cookies(
            &self,
            _url: Option<&str>,
        ) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("delete_cookies")
        }

        async fn storage_snapshot(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
        ) -> Result<StorageSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("storage_snapshot")
        }

        async fn storage_snapshot_for_tab(
            &self,
            _target_id: &str,
            _frame_id: Option<&str>,
        ) -> Result<StorageSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("storage_snapshot_for_tab")
        }

        async fn set_storage_item(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _area: StorageArea,
            _key: &str,
            _value: &str,
        ) -> Result<StorageSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("set_storage_item")
        }

        async fn remove_storage_item(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _area: StorageArea,
            _key: &str,
        ) -> Result<StorageSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("remove_storage_item")
        }

        async fn clear_storage(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _area: Option<StorageArea>,
        ) -> Result<StorageSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("clear_storage")
        }

        async fn replace_storage(
            &self,
            _frame_id: Option<&str>,
            _expected_origin: Option<&str>,
            _snapshot: &StorageSnapshot,
        ) -> Result<StorageSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("replace_storage")
        }

        async fn upload_file(
            &self,
            _element: &Element,
            _path: &str,
        ) -> Result<InteractionOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("upload_file")
        }

        async fn select_option(
            &self,
            _element: &Element,
            _value: &str,
        ) -> Result<SelectOutcome, rub_core::error::RubError> {
            unexpected_browser_call!("select_option")
        }

        async fn snapshot_with_a11y(
            &self,
            _limit: Option<u32>,
        ) -> Result<Snapshot, rub_core::error::RubError> {
            unexpected_browser_call!("snapshot_with_a11y")
        }

        async fn snapshot_with_a11y_for_frame(
            &self,
            _frame_id: Option<&str>,
            _limit: Option<u32>,
        ) -> Result<Snapshot, rub_core::error::RubError> {
            unexpected_browser_call!("snapshot_with_a11y_for_frame")
        }

        async fn viewport_dimensions(&self) -> Result<(f64, f64), rub_core::error::RubError> {
            unexpected_browser_call!("viewport_dimensions")
        }

        async fn highlight_elements(
            &self,
            _snapshot: &Snapshot,
        ) -> Result<u32, rub_core::error::RubError> {
            unexpected_browser_call!("highlight_elements")
        }

        async fn cleanup_highlights(&self) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("cleanup_highlights")
        }

        async fn snapshot_with_listeners(
            &self,
            _limit: Option<u32>,
            _include_a11y: bool,
        ) -> Result<Snapshot, rub_core::error::RubError> {
            unexpected_browser_call!("snapshot_with_listeners")
        }

        async fn snapshot_with_listeners_for_frame(
            &self,
            _frame_id: Option<&str>,
            _limit: Option<u32>,
            _include_a11y: bool,
        ) -> Result<Snapshot, rub_core::error::RubError> {
            unexpected_browser_call!("snapshot_with_listeners_for_frame")
        }

        async fn sync_network_rules(
            &self,
            _rules: &[NetworkRule],
        ) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("sync_network_rules")
        }

        async fn probe_runtime_state(
            &self,
        ) -> Result<RuntimeStateSnapshot, rub_core::error::RubError> {
            unexpected_browser_call!("probe_runtime_state")
        }

        async fn probe_frame_runtime(&self) -> Result<FrameRuntimeInfo, rub_core::error::RubError> {
            unexpected_browser_call!("probe_frame_runtime")
        }

        async fn list_frames(&self) -> Result<Vec<FrameInventoryEntry>, rub_core::error::RubError> {
            Ok(self.frames.clone())
        }

        async fn list_frames_for_tab(
            &self,
            _target_id: &str,
        ) -> Result<Vec<FrameInventoryEntry>, rub_core::error::RubError> {
            unexpected_browser_call!("list_frames_for_tab")
        }

        async fn cancel_download(&self, _guid: &str) -> Result<(), rub_core::error::RubError> {
            unexpected_browser_call!("cancel_download")
        }
    }

    fn test_router() -> DaemonRouter {
        DaemonRouter::new(Arc::new(QueryFrameBrowser {
            frames: vec![frame_inventory("root"), frame_inventory("child")],
        }))
    }

    fn test_state() -> Arc<SessionState> {
        Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-query-frame-test"),
            None,
        ))
    }

    #[test]
    fn live_read_subject_projects_canonical_locator_identity() {
        let locator = CanonicalLocator::Selector {
            css: ".cta".to_string(),
            selection: None,
        };
        let subject = live_read_subject("text", &locator, Some("root"));
        assert_eq!(subject["kind"], "live_read");
        assert_eq!(subject["read_kind"], "text");
        assert_eq!(subject["frame_id"], "root");
        assert_eq!(subject["locator"]["selector"], ".cta");
    }

    #[test]
    fn scalar_and_multi_read_results_share_canonical_shape() {
        let scalar = scalar_read_result("text", json!("Alpha"));
        assert_eq!(scalar["kind"], "text");
        assert_eq!(scalar["value"], "Alpha");

        let many = multi_read_result("text", json!(["Alpha", "Beta"]));
        assert_eq!(many["kind"], "text");
        assert_eq!(many["items"], json!(["Alpha", "Beta"]));
        assert_eq!(many["item_count"], 2);
    }

    #[test]
    fn read_payload_wraps_subject_and_result() {
        let payload = read_payload(
            page_subject(None),
            scalar_read_result("title", json!("Example")),
        );
        assert_eq!(payload["subject"]["kind"], "page");
        assert_eq!(payload["result"]["kind"], "title");
        assert_eq!(payload["result"]["value"], "Example");
    }

    #[test]
    fn typed_get_payload_uses_tagged_subcommand_dispatch() {
        let parsed: GetCommand = parse_json_args(
            &json!({
                "sub": "text",
                "selector": ".cta",
            }),
            "get",
        )
        .expect("get text payload should parse");
        assert!(matches!(parsed, GetCommand::Text(_)));
    }

    #[test]
    fn inspect_payload_rejects_unknown_fields_in_stripped_args() {
        // After cmd_inspect strips "sub", InspectReadArgs is parsed directly.
        // Verify that unknown fields are still rejected.
        let error = parse_json_args::<InspectReadArgs>(
            &json!({
                "selector": ".cta",
                "mystery": true,
            }),
            "inspect",
        )
        .expect_err("unknown inspect fields should be rejected")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn typed_exec_payload_accepts_raw_compat_flag() {
        let parsed: ExecArgs = parse_json_args(
            &json!({
                "code": "document.title",
                "raw": true,
                "wait_after": {"text": "Ready"},
                "_trigger": {"kind": "trigger_action"},
            }),
            "exec",
        )
        .expect("exec payload should accept raw compatibility flag");
        assert!(parsed._raw);
        assert!(parsed._wait_after.is_some());
        assert!(parsed._trigger.is_some());
    }

    #[tokio::test]
    async fn exec_inherits_selected_frame_authority() {
        let router = test_router();
        let state = test_state();
        state.select_frame(Some("child".to_string())).await;

        let result = cmd_exec(
            &router,
            &json!({
                "code": "document.title",
            }),
            &state,
        )
        .await
        .expect("exec should honor selected frame");

        assert_eq!(result["subject"]["frame_id"], "child");
        assert_eq!(result["result"], "child");
    }

    #[test]
    fn inspect_many_rejects_locator_selection_flags() {
        let error = reject_live_many_locator_selection(
            Some(&CanonicalLocator::Role {
                role: "button".to_string(),
                selection: Some(rub_core::locator::LocatorSelection::Nth(1)),
            }),
            "text",
        )
        .expect_err("inspect --many should reject selection modifiers");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("--many"));
    }

    #[test]
    fn inspect_snapshot_without_locator_fails_closed() {
        let error = reject_snapshot_without_locator("inspect html", Some("snap-1"), None)
            .expect_err("inspect html --snapshot without locator should fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("--snapshot"));
    }

    #[tokio::test]
    async fn inspect_text_snapshot_without_locator_uses_inspect_command_label() {
        let router = test_router();
        let state = test_state();

        let error = cmd_inspect_read(
            &router,
            &json!({
                "snapshot_id": "snap-1",
            }),
            "text",
            TransactionDeadline::new(1_000),
            &state,
        )
        .await
        .expect_err("inspect text --snapshot without locator must fail closed");
        let envelope = error.into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("inspect text --snapshot"));
    }
}
