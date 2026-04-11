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
    let area = parse_storage_area(condition.storage_area.as_deref())?;

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

fn parse_storage_area(area: Option<&str>) -> Result<Option<StorageArea>, RubError> {
    match area.map(|value| value.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(value) if value == "local" => Ok(Some(StorageArea::Local)),
        Some(value) if value == "session" => Ok(Some(StorageArea::Session)),
        Some(other) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unsupported orchestration storage area '{other}'; use 'local' or 'session'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::network_request_matches;
    use rub_core::model::{
        NetworkRequestLifecycle, NetworkRequestRecord, TriggerConditionKind, TriggerConditionSpec,
    };
    use std::collections::BTreeMap;

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
}
