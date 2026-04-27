use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::{
    NetworkRequestRecord, ReadinessInfo, RouteStability, TabInfo, TriggerConditionKind,
    TriggerEvidenceInfo, TriggerInfo, TriggerStatus,
};
use rub_core::storage::{StorageArea, StorageSnapshot};

use crate::session::NetworkRequestBaseline;
use crate::session::SessionState;

use super::action::resolve_bound_tab;
use super::{TriggerWorkerEntry, trigger_degraded_authority_error};

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
    committed_baselines: &HashMap<u32, NetworkRequestBaseline>,
) {
    let live_ids = triggers
        .iter()
        .map(|trigger| trigger.id)
        .collect::<HashSet<_>>();
    worker_state.retain(|id, _| live_ids.contains(id));

    for trigger in triggers {
        let baseline_required =
            matches!(trigger.condition.kind, TriggerConditionKind::NetworkRequest)
                && matches!(trigger.status, TriggerStatus::Armed);
        let committed_baseline = committed_baselines.get(&trigger.id).copied();
        let entry = worker_state
            .entry(trigger.id)
            .or_insert(TriggerWorkerEntry {
                last_status: trigger.status,
                network_cursor: committed_baseline
                    .map(|baseline| baseline.cursor)
                    .unwrap_or(0),
                network_cursor_primed: committed_baseline
                    .map(|baseline| baseline.primed)
                    .unwrap_or(!baseline_required),
                observatory_drop_count: committed_baseline
                    .map(|baseline| baseline.observed_ingress_drop_count)
                    .unwrap_or(0),
            });
        if !matches!(entry.last_status, TriggerStatus::Armed)
            && matches!(trigger.status, TriggerStatus::Armed)
        {
            if let Some(committed_baseline) = committed_baselines.get(&trigger.id).copied() {
                entry.network_cursor = committed_baseline.cursor;
                entry.network_cursor_primed = committed_baseline.primed;
                entry.observatory_drop_count = committed_baseline.observed_ingress_drop_count;
            } else if baseline_required {
                entry.network_cursor = 0;
                entry.network_cursor_primed = false;
                entry.observatory_drop_count = 0;
            } else {
                entry.network_cursor_primed = true;
            }
        }
        if baseline_required && committed_baseline.is_none() {
            entry.network_cursor_primed = false;
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
            if let Some(degraded_reason) = source_tab.degraded_reason.as_deref() {
                return Err(RubError::Domain(trigger_degraded_authority_error(
                    "trigger url_match condition is not authoritative because source tab page identity is degraded",
                    "trigger_source_tab_projection_degraded",
                    serde_json::json!({
                        "tab_target_id": source_tab.target_id,
                        "degraded_reason": degraded_reason,
                    }),
                )));
            }
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
                .await
                .map_err(|error| {
                    normalize_source_frame_continuity_error(error, trigger, "text_present")
                })?
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
                .await
                .map_err(|error| {
                    normalize_source_frame_continuity_error(error, trigger, "locator_present")
                })?;
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
                .await
                .map_err(|error| {
                    normalize_source_frame_continuity_error(error, trigger, "readiness")
                })?
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
            if !worker.network_cursor_primed {
                return Err(RubError::Domain(trigger_degraded_authority_error(
                    "trigger network_request evaluation is not authoritative because its committed observatory baseline is missing",
                    "trigger_network_request_baseline_missing",
                    serde_json::json!({
                        "next_network_cursor": worker.network_cursor,
                        "dropped_event_count": worker.observatory_drop_count,
                    }),
                )));
            }
            let window = state
                .network_request_window_after(worker.network_cursor, worker.observatory_drop_count)
                .await;
            let observed_drop_count = state.network_request_ingress_drop_count();
            if !window.authoritative {
                worker.network_cursor = window.next_cursor;
                worker.observatory_drop_count = observed_drop_count;
                return Err(RubError::Domain(trigger_degraded_authority_error(
                    "trigger network_request evaluation is not authoritative because observatory evidence was dropped",
                    "runtime_observatory_not_authoritative",
                    serde_json::json!({
                        "degraded_reason": window.degraded_reason,
                        "next_network_cursor": worker.network_cursor,
                        "dropped_event_count": worker.observatory_drop_count,
                    }),
                )));
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
                .await
                .map_err(|error| {
                    normalize_source_frame_continuity_error(error, trigger, "storage_value")
                })?;
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

fn normalize_source_frame_continuity_error(
    error: RubError,
    trigger: &TriggerInfo,
    condition_kind: &'static str,
) -> RubError {
    let RubError::Domain(envelope) = error else {
        return error;
    };
    let Some(source_frame_id) = trigger.source_tab.frame_id.as_deref() else {
        return RubError::Domain(envelope);
    };
    let frame_authority_reason = envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str());
    if envelope.code != ErrorCode::InvalidInput
        || !matches!(
            frame_authority_reason,
            Some(
                "frame_inventory_missing"
                    | "frame_not_same_origin_accessible"
                    | "frame_execution_context_missing"
            )
        )
    {
        return RubError::Domain(envelope);
    }

    RubError::Domain(trigger_degraded_authority_error(
        "trigger source-frame continuity fence failed before authoritative condition evaluation could complete",
        "continuity_frame_unavailable",
        serde_json::json!({
            "source_tab_target_id": trigger.source_tab.target_id,
            "source_frame_id": source_frame_id,
            "condition_kind": condition_kind,
            "frame_authority_reason": frame_authority_reason,
            "upstream_error_code": envelope.code,
            "upstream_error_message": envelope.message,
            "upstream_error_context": envelope.context,
        }),
    ))
}

pub(super) fn commit_trigger_network_progress(
    worker: &mut TriggerWorkerEntry,
    progress: Option<TriggerNetworkProgress>,
) {
    if let Some(progress) = progress {
        worker.network_cursor = progress.next_cursor;
        worker.network_cursor_primed = true;
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

    if readiness.degraded_reason.is_some() {
        return false;
    }

    if requested == "ready" {
        return matches!(readiness.route_stability, RouteStability::Stable);
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

#[cfg(test)]
mod tests {
    use super::normalize_source_frame_continuity_error;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::{
        TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerInfo, TriggerMode, TriggerStatus, TriggerTabBindingInfo,
    };

    fn trigger_with_source_frame() -> TriggerInfo {
        TriggerInfo {
            id: 7,
            status: TriggerStatus::Armed,
            lifecycle_generation: 1,
            mode: TriggerMode::Once,
            source_tab: TriggerTabBindingInfo {
                index: 0,
                target_id: "source".to_string(),
                frame_id: Some("frame-a".to_string()),
                url: "https://source.example".to_string(),
                title: "Source".to_string(),
            },
            target_tab: TriggerTabBindingInfo {
                index: 1,
                target_id: "target".to_string(),
                frame_id: None,
                url: "https://target.example".to_string(),
                title: "Target".to_string(),
            },
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::TextPresent,
                locator: None,
                text: Some("Ready".to_string()),
                url_pattern: None,
                readiness_state: None,
                method: None,
                status_code: None,
                storage_area: None,
                key: None,
                value: None,
            },
            action: TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: None,
            },
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        }
    }

    #[test]
    fn source_frame_continuity_error_uses_degraded_authority_family() {
        let trigger = trigger_with_source_frame();
        let error = normalize_source_frame_continuity_error(
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "Frame 'frame-a' is not present in the current frame inventory",
                serde_json::json!({
                    "reason": "frame_inventory_missing",
                    "frame_id": "frame-a",
                }),
            ),
            &trigger,
            "text_present",
        )
        .into_envelope();

        assert_eq!(error.code, ErrorCode::SessionBusy);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_frame_unavailable")
        );
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("frame_authority_reason"))
                .and_then(|value| value.as_str()),
            Some("frame_inventory_missing")
        );
    }
}
