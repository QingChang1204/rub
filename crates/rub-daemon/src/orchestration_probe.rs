use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::{
    OrchestrationSessionInfo, TabInfo, TriggerConditionKind, TriggerConditionSpec,
    TriggerEvidenceInfo,
};
use rub_core::port::BrowserPort;
use rub_core::storage::{StorageArea, StorageSnapshot};
use rub_ipc::protocol::IpcRequest;
use serde::{Deserialize, Serialize};

use crate::orchestration_executor::{
    RemoteDispatchContract, decode_orchestration_success_payload,
    dispatch_remote_orchestration_request,
};
use crate::session::SessionState;

/// Structured, bounded probe result used by orchestration workers and the
/// internal `_orchestration_probe` command surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrchestrationProbeResult {
    pub matched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TriggerEvidenceInfo>,
    pub next_network_cursor: u64,
    #[serde(default)]
    pub observed_drop_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

pub(crate) async fn evaluate_orchestration_probe_for_tab(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &TriggerConditionSpec,
    after_sequence: u64,
    last_observed_drop_count: u64,
) -> Result<OrchestrationProbeResult, RubError> {
    match condition.kind {
        TriggerConditionKind::UrlMatch => {
            let pattern = condition.url_pattern.as_deref().unwrap_or_default().trim();
            let tabs = browser.list_tabs().await?;
            let source_tab = resolve_tab(&tabs, tab_target_id)?;
            if !source_tab.url.contains(pattern) {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: after_sequence,
                    observed_drop_count: 0,
                    degraded_reason: None,
                });
            }
            Ok(OrchestrationProbeResult {
                matched: true,
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_url_matched:{pattern}"),
                    fingerprint: Some(source_tab.url.clone()),
                }),
                next_network_cursor: after_sequence,
                observed_drop_count: 0,
                degraded_reason: None,
            })
        }
        TriggerConditionKind::TextPresent => {
            let text = condition.text.as_deref().unwrap_or_default();
            if !browser.tab_has_text(tab_target_id, frame_id, text).await? {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: after_sequence,
                    observed_drop_count: 0,
                    degraded_reason: None,
                });
            }
            Ok(OrchestrationProbeResult {
                matched: true,
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_text_present:{text}"),
                    fingerprint: Some(text.to_string()),
                }),
                next_network_cursor: after_sequence,
                observed_drop_count: 0,
                degraded_reason: None,
            })
        }
        TriggerConditionKind::LocatorPresent => {
            let locator = condition.locator.as_ref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "orchestration locator_present condition is missing a locator",
                )
            })?;
            let locator = LiveLocator::try_from(locator.clone()).map_err(|invalid| {
                RubError::domain_with_context_and_suggestion(
                    ErrorCode::InvalidInput,
                    "orchestration locator_present condition requires a live DOM locator",
                    serde_json::json!({
                        "locator": invalid,
                    }),
                    "Use selector, target_text, role, label, or testid addressing for live content probes",
                )
            })?;
            let matches = browser
                .find_content_matches_in_tab(tab_target_id, frame_id, &locator)
                .await?;
            if matches.is_empty() {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: after_sequence,
                    observed_drop_count: 0,
                    degraded_reason: None,
                });
            }
            let first = &matches[0];
            Ok(OrchestrationProbeResult {
                matched: true,
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!(
                        "source_tab_locator_present:{}:{}",
                        first.tag_name,
                        matches.len()
                    ),
                    fingerprint: Some(format!(
                        "{}:{}:{}",
                        first.tag_name,
                        first.role,
                        matches.len()
                    )),
                }),
                next_network_cursor: after_sequence,
                observed_drop_count: 0,
                degraded_reason: None,
            })
        }
        TriggerConditionKind::Readiness => {
            let readiness = browser
                .probe_runtime_state_for_tab(tab_target_id, frame_id)
                .await?
                .readiness_state;
            let requested = condition.readiness_state.as_deref().unwrap_or_default();
            if !readiness_matches(&readiness, requested) {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: after_sequence,
                    observed_drop_count: 0,
                    degraded_reason: None,
                });
            }
            Ok(OrchestrationProbeResult {
                matched: true,
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_readiness_matched:{requested}"),
                    fingerprint: readiness.document_ready_state.clone().or_else(|| {
                        Some(format!("{:?}", readiness.route_stability).to_lowercase())
                    }),
                }),
                next_network_cursor: after_sequence,
                observed_drop_count: 0,
                degraded_reason: None,
            })
        }
        TriggerConditionKind::NetworkRequest => {
            let window = state
                .network_request_window_after(after_sequence, last_observed_drop_count)
                .await;
            let observed_drop_count = state.network_request_drop_count().await;
            if !window.authoritative {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: window.next_cursor,
                    observed_drop_count,
                    degraded_reason: window.degraded_reason,
                });
            }

            let Some(record) = window
                .records
                .into_iter()
                .find(|record| network_request_matches(record, tab_target_id, frame_id, condition))
            else {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: window.next_cursor,
                    observed_drop_count,
                    degraded_reason: None,
                });
            };
            Ok(OrchestrationProbeResult {
                matched: true,
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("network_request_matched:{}", record.request_id),
                    fingerprint: Some(record.request_id),
                }),
                next_network_cursor: window.next_cursor,
                observed_drop_count,
                degraded_reason: None,
            })
        }
        TriggerConditionKind::StorageValue => {
            let snapshot = browser
                .storage_snapshot_for_tab(tab_target_id, frame_id)
                .await?;
            if !storage_snapshot_matches(&snapshot, condition)? {
                return Ok(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: after_sequence,
                    observed_drop_count: 0,
                    degraded_reason: None,
                });
            }
            let key = condition.key.as_deref().unwrap_or_default();
            Ok(OrchestrationProbeResult {
                matched: true,
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_storage_matched:{key}"),
                    fingerprint: Some(format!("{}:{key}", snapshot.origin)),
                }),
                next_network_cursor: after_sequence,
                observed_drop_count: 0,
                degraded_reason: None,
            })
        }
    }
}

pub(crate) async fn dispatch_remote_orchestration_probe(
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &TriggerConditionSpec,
    after_sequence: u64,
    last_observed_drop_count: u64,
) -> Result<OrchestrationProbeResult, ErrorEnvelope> {
    let response = dispatch_remote_orchestration_request(
        session,
        "source",
        IpcRequest::new(
            "_orchestration_probe",
            serde_json::json!({
                "tab_target_id": tab_target_id,
                "frame_id": frame_id,
                "condition": condition,
                "after_sequence": after_sequence,
                "last_observed_drop_count": last_observed_drop_count,
            }),
            30_000,
        ),
        RemoteDispatchContract {
            dispatch_subject: "probe",
            unreachable_reason: "orchestration_source_session_unreachable",
            transport_failure_reason: "orchestration_source_probe_dispatch_transport_failed",
            protocol_failure_reason: "orchestration_source_probe_dispatch_protocol_failed",
            missing_error_message:
                "remote orchestration probe returned an error without an envelope",
        },
    )
    .await?;

    decode_orchestration_success_payload(
        response,
        session,
        "orchestration_source_probe_payload_missing",
        "orchestration probe returned success without a payload",
        "orchestration_source_probe_payload_invalid",
        "orchestration probe payload",
    )
}

fn resolve_tab<'a>(tabs: &'a [TabInfo], tab_target_id: &str) -> Result<&'a TabInfo, RubError> {
    tabs.iter()
        .find(|tab| tab.target_id == tab_target_id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::TabNotFound,
                format!(
                    "Source tab target '{}' is not present in the current session",
                    tab_target_id
                ),
            )
        })
}

fn readiness_matches(readiness: &rub_core::model::ReadinessInfo, requested: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() {
        return false;
    }

    if requested == "ready" {
        return matches!(
            readiness.route_stability,
            rub_core::model::RouteStability::Stable
        ) && readiness.degraded_reason.is_none();
    }

    requested == format!("{:?}", readiness.status).to_ascii_lowercase()
        || requested == format!("{:?}", readiness.route_stability).to_ascii_lowercase()
        || readiness
            .document_ready_state
            .as_deref()
            .map(|state| state.eq_ignore_ascii_case(&requested))
            .unwrap_or(false)
}

fn network_request_matches(
    record: &rub_core::model::NetworkRequestRecord,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &TriggerConditionSpec,
) -> bool {
    if record.tab_target_id.as_deref() != Some(tab_target_id) {
        return false;
    }

    if let Some(frame_id) = frame_id
        && record.frame_id.as_deref() != Some(frame_id)
    {
        return false;
    }

    let url_pattern = condition.url_pattern.as_deref().unwrap_or_default();
    if !record.url.contains(url_pattern) {
        return false;
    }

    if let Some(method) = condition.method.as_deref()
        && !record.method.eq_ignore_ascii_case(method)
    {
        return false;
    }

    if let Some(status_code) = condition.status_code
        && record.status != Some(status_code)
    {
        return false;
    }

    true
}

fn storage_snapshot_matches(
    snapshot: &StorageSnapshot,
    condition: &TriggerConditionSpec,
) -> Result<bool, RubError> {
    let key = condition.key.as_deref().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "orchestration storage_value condition is missing condition.key",
        )
    })?;
    let expected_value = condition.value.as_deref();
    let area = condition.storage_area;

    let empty = std::collections::BTreeMap::new();
    let areas = match area {
        Some(StorageArea::Local) => [&snapshot.local_storage, &empty],
        Some(StorageArea::Session) => [&empty, &snapshot.session_storage],
        None => [&snapshot.local_storage, &snapshot.session_storage],
    };

    for entries in areas {
        if let Some(value) = entries.get(key)
            && expected_value
                .map(|expected| expected == value)
                .unwrap_or(true)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{evaluate_orchestration_probe_for_tab, network_request_matches};
    use async_trait::async_trait;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::locator::LiveLocator;
    use rub_core::model::{
        BoundingBox, ContentFindMatch, Cookie, DialogInterceptPolicy, DialogRuntimeInfo, Element,
        FrameInventoryEntry, FrameRuntimeInfo, InteractionOutcome, KeyCombo, LaunchPolicyInfo,
        LoadStrategy, NetworkRequestLifecycle, NetworkRequestRecord, NetworkRule, OverlayState,
        Page, ReadinessInfo, ReadinessStatus, RouteStability, RuntimeStateSnapshot,
        ScrollDirection, ScrollPosition, SelectOutcome, Snapshot, StateInspectorInfo, TabInfo,
        TriggerConditionKind, TriggerConditionSpec, WaitCondition,
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

    struct ReadinessProbeBrowser;

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

        async fn input(
            &self,
            _element: &Element,
            _text: &str,
            _clear: bool,
        ) -> Result<InteractionOutcome, RubError> {
            unexpected_browser_call!("input")
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
            frame_id: Option<&str>,
        ) -> Result<RuntimeStateSnapshot, RubError> {
            match frame_id {
                Some(frame_id) => Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("frame '{frame_id}' is unavailable"),
                )),
                None => Ok(ready_snapshot()),
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
        let browser: Arc<dyn BrowserPort> = Arc::new(ReadinessProbeBrowser);
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
}
