use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::{
    NetworkRequestRecord, ReadinessInfo, RouteStability, TabInfo, TriggerConditionKind,
    TriggerEvidenceInfo, TriggerInfo, TriggerStatus,
};
use rub_core::storage::{StorageArea, StorageSnapshot};

use crate::session::SessionState;

use super::TriggerWorkerEntry;
use super::action::resolve_bound_tab;

#[derive(Debug, Clone, Copy)]
pub(super) struct TriggerNetworkProgress {
    pub(super) next_cursor: u64,
    pub(super) observed_drop_count: u64,
}

pub(super) struct TriggerConditionEvaluation {
    pub(super) evidence: Option<TriggerEvidenceInfo>,
    pub(super) network_progress: Option<TriggerNetworkProgress>,
}

pub(super) enum TriggerConditionState {
    NotTriggered {
        network_progress: Option<TriggerNetworkProgress>,
    },
    Triggered(TriggeredTriggerCondition),
}

#[derive(Clone)]
pub(super) struct TriggeredTriggerCondition {
    pub(super) evidence: TriggerEvidenceInfo,
    pub(super) evidence_fingerprint: String,
    pub(super) network_progress: Option<TriggerNetworkProgress>,
}

pub(super) async fn load_trigger_condition_state(
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    state: &Arc<SessionState>,
    tabs: &[TabInfo],
    trigger: &TriggerInfo,
    worker: &mut TriggerWorkerEntry,
) -> Result<TriggerConditionState, RubError> {
    let evaluation = evaluate_trigger_condition(browser, state, tabs, trigger, worker).await?;
    Ok(match evaluation.evidence {
        Some(evidence) => TriggerConditionState::Triggered(TriggeredTriggerCondition {
            evidence_fingerprint: trigger_evidence_consumption_key(&evidence),
            evidence,
            network_progress: evaluation.network_progress,
        }),
        None => TriggerConditionState::NotTriggered {
            network_progress: evaluation.network_progress,
        },
    })
}

pub(super) fn reconcile_worker_state(
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
    triggers: &[TriggerInfo],
    active_request_cursor: u64,
    observatory_drop_count: u64,
) {
    let live_ids = triggers
        .iter()
        .map(|trigger| trigger.id)
        .collect::<HashSet<_>>();
    worker_state.retain(|id, _| live_ids.contains(id));

    for trigger in triggers {
        let entry = worker_state
            .entry(trigger.id)
            .or_insert(TriggerWorkerEntry {
                last_status: trigger.status,
                network_cursor: active_request_cursor,
                observatory_drop_count,
            });
        if !matches!(entry.last_status, TriggerStatus::Armed)
            && matches!(trigger.status, TriggerStatus::Armed)
        {
            entry.network_cursor = active_request_cursor;
            entry.observatory_drop_count = observatory_drop_count;
        }
        entry.last_status = trigger.status;
    }
}

pub(super) async fn evaluate_trigger_condition(
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    state: &Arc<SessionState>,
    tabs: &[TabInfo],
    trigger: &TriggerInfo,
    worker: &mut TriggerWorkerEntry,
) -> Result<TriggerConditionEvaluation, RubError> {
    match trigger.condition.kind {
        TriggerConditionKind::UrlMatch => {
            let pattern = trigger
                .condition
                .url_pattern
                .as_deref()
                .unwrap_or_default()
                .trim();
            let source_tab = resolve_bound_tab(tabs, &trigger.source_tab.target_id)?;
            if !source_tab.url.contains(pattern) {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_url_matched:{pattern}"),
                    fingerprint: Some(source_tab.url.clone()),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::TextPresent => {
            let text = trigger.condition.text.as_deref().unwrap_or_default();
            if !browser
                .tab_has_text(
                    &trigger.source_tab.target_id,
                    trigger.source_tab.frame_id.as_deref(),
                    text,
                )
                .await?
            {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_text_present:{text}"),
                    fingerprint: Some(text.to_string()),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::LocatorPresent => {
            let locator = trigger.condition.locator.as_ref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "trigger locator_present condition is missing a locator",
                )
            })?;
            let locator = LiveLocator::try_from(locator.clone()).map_err(|invalid| {
                RubError::domain_with_context_and_suggestion(
                    ErrorCode::InvalidInput,
                    "trigger locator_present condition requires a live DOM locator",
                    serde_json::json!({
                        "locator": invalid,
                    }),
                    "Use selector, target_text, role, label, or testid addressing for live content probes",
                )
            })?;
            let matches = browser
                .find_content_matches_in_tab(
                    &trigger.source_tab.target_id,
                    trigger.source_tab.frame_id.as_deref(),
                    &locator,
                )
                .await?;
            if matches.is_empty() {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            let first = &matches[0];
            Ok(TriggerConditionEvaluation {
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
                network_progress: None,
            })
        }
        TriggerConditionKind::Readiness => {
            let readiness = browser
                .probe_runtime_state_for_tab(
                    &trigger.source_tab.target_id,
                    trigger.source_tab.frame_id.as_deref(),
                )
                .await?
                .readiness_state;
            let requested = trigger
                .condition
                .readiness_state
                .as_deref()
                .unwrap_or_default();
            if !readiness_matches(&readiness, requested) {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_readiness_matched:{requested}"),
                    fingerprint: readiness.document_ready_state.clone().or_else(|| {
                        Some(format!("{:?}", readiness.route_stability).to_lowercase())
                    }),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::NetworkRequest => {
            let window = state
                .network_request_window_after(worker.network_cursor, worker.observatory_drop_count)
                .await;
            let observed_drop_count = state.network_request_drop_count().await;
            if !window.authoritative {
                worker.network_cursor = window.next_cursor;
                worker.observatory_drop_count = observed_drop_count;
                return Err(RubError::domain_with_context(
                    ErrorCode::BrowserCrashed,
                    "trigger network_request evaluation is not authoritative because observatory evidence was dropped",
                    serde_json::json!({
                        "reason": "runtime_observatory_not_authoritative",
                        "degraded_reason": window.degraded_reason,
                        "next_network_cursor": worker.network_cursor,
                        "dropped_event_count": worker.observatory_drop_count,
                    }),
                ));
            }
            let network_progress = Some(TriggerNetworkProgress {
                next_cursor: window.next_cursor,
                observed_drop_count,
            });

            let Some(record) = window
                .records
                .into_iter()
                .find(|record| network_request_matches(record, trigger))
            else {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress,
                });
            };
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("network_request_matched:{}", record.request_id),
                    fingerprint: Some(record.request_id),
                }),
                network_progress,
            })
        }
        TriggerConditionKind::StorageValue => {
            let snapshot = browser
                .storage_snapshot_for_tab(
                    &trigger.source_tab.target_id,
                    trigger.source_tab.frame_id.as_deref(),
                )
                .await?;
            if !storage_snapshot_matches(&snapshot, trigger)? {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            let key = trigger.condition.key.as_deref().unwrap_or_default();
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_storage_matched:{key}"),
                    fingerprint: Some(format!("{}:{key}", snapshot.origin)),
                }),
                network_progress: None,
            })
        }
    }
}

pub(super) fn commit_trigger_network_progress(
    worker: &mut TriggerWorkerEntry,
    progress: Option<TriggerNetworkProgress>,
) {
    if let Some(progress) = progress {
        worker.network_cursor = progress.next_cursor;
        worker.observatory_drop_count = progress.observed_drop_count;
    }
}

pub(super) fn trigger_evidence_consumption_key(evidence: &TriggerEvidenceInfo) -> String {
    evidence
        .fingerprint
        .clone()
        .unwrap_or_else(|| evidence.summary.clone())
}

pub(super) fn readiness_matches(readiness: &ReadinessInfo, requested: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() {
        return false;
    }

    if requested == "ready" {
        return matches!(readiness.route_stability, RouteStability::Stable)
            && readiness.degraded_reason.is_none();
    }

    requested == format!("{:?}", readiness.status).to_ascii_lowercase()
        || requested == format!("{:?}", readiness.route_stability).to_ascii_lowercase()
        || readiness
            .document_ready_state
            .as_deref()
            .map(|state| state.eq_ignore_ascii_case(&requested))
            .unwrap_or(false)
}

pub(super) fn network_request_matches(
    record: &NetworkRequestRecord,
    trigger: &TriggerInfo,
) -> bool {
    if record.tab_target_id.as_deref() != Some(trigger.source_tab.target_id.as_str()) {
        return false;
    }
    if let Some(frame_id) = trigger.source_tab.frame_id.as_deref()
        && record.frame_id.as_deref() != Some(frame_id)
    {
        return false;
    }

    let url_pattern = trigger.condition.url_pattern.as_deref().unwrap_or_default();
    if !record.url.contains(url_pattern) {
        return false;
    }

    if let Some(method) = trigger.condition.method.as_deref()
        && !record.method.eq_ignore_ascii_case(method)
    {
        return false;
    }

    if let Some(status_code) = trigger.condition.status_code
        && record.status != Some(status_code)
    {
        return false;
    }

    true
}

pub(super) fn storage_snapshot_matches(
    snapshot: &StorageSnapshot,
    trigger: &TriggerInfo,
) -> Result<bool, RubError> {
    let key = trigger.condition.key.as_deref().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "trigger storage_value condition is missing condition.key",
        )
    })?;
    let expected_value = trigger.condition.value.as_deref();
    let area = trigger.condition.storage_area;

    let areas = match area {
        Some(StorageArea::Local) => [&snapshot.local_storage, &std::collections::BTreeMap::new()],
        Some(StorageArea::Session) => [
            &std::collections::BTreeMap::new(),
            &snapshot.session_storage,
        ],
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
