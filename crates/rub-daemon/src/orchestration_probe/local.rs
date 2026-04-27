use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::{TriggerConditionKind, TriggerConditionSpec, TriggerEvidenceInfo};
use rub_core::port::BrowserPort;

use super::OrchestrationProbeResult;
use super::matching::{
    network_request_matches, readiness_matches, resolve_tab, storage_snapshot_matches,
};
use crate::router::orchestration_degraded_authority_error;
use crate::session::SessionState;

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
            if let Some(degraded_reason) = source_tab.degraded_reason.as_deref() {
                return Err(RubError::Domain(orchestration_degraded_authority_error(
                    "orchestration url_match condition is not authoritative because source tab page identity is degraded",
                    "orchestration_source_tab_projection_degraded",
                    serde_json::json!({
                        "tab_target_id": source_tab.target_id,
                        "degraded_reason": degraded_reason,
                    }),
                )));
            }
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
            let observed_drop_count = state.network_request_ingress_drop_count();
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
