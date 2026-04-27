use std::collections::{HashMap, VecDeque};

use rub_core::model::{
    TabInfo, TriggerEventInfo, TriggerEventKind, TriggerEvidenceInfo, TriggerInfo,
    TriggerResultInfo, TriggerRuntimeInfo, TriggerRuntimeStatus, TriggerStatus,
    TriggerTraceProjection,
};

use crate::session::NetworkRequestBaseline;

mod events;
mod mutation;
mod projection;

const TRIGGER_EVENT_LIMIT: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum TriggerOutcomeCommit {
    Applied(Option<TriggerInfo>),
    Stale(Option<TriggerInfo>),
}

/// Session-scoped trigger registry authority.
#[derive(Debug, Default)]
pub struct TriggerRuntimeState {
    projection: TriggerRuntimeInfo,
    network_request_baselines: HashMap<u32, NetworkRequestBaseline>,
    next_event_sequence: u64,
    recent_events: VecDeque<TriggerEventInfo>,
}

impl TriggerRuntimeState {
    pub fn projection(&self) -> TriggerRuntimeInfo {
        self.projection.clone()
    }

    pub fn trace(&self, last: usize) -> TriggerTraceProjection {
        let take = last.min(self.recent_events.len());
        let mut events = self
            .recent_events
            .iter()
            .rev()
            .take(take)
            .cloned()
            .collect::<Vec<_>>();
        events.reverse();
        TriggerTraceProjection { events }
    }

    pub(crate) fn network_request_baselines(&self) -> HashMap<u32, NetworkRequestBaseline> {
        self.network_request_baselines.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{TriggerOutcomeCommit, TriggerRuntimeState};
    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        TabInfo, TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerEventKind, TriggerEvidenceInfo, TriggerInfo, TriggerMode, TriggerResultInfo,
        TriggerRuntimeStatus, TriggerStatus, TriggerTabBindingInfo,
    };
    use serde_json::json;

    fn binding(index: u32, target_id: &str) -> TriggerTabBindingInfo {
        TriggerTabBindingInfo {
            index,
            target_id: target_id.to_string(),
            frame_id: None,
            url: format!("https://example.com/{index}"),
            title: format!("Tab {index}"),
        }
    }

    fn trigger(id: u32, status: TriggerStatus) -> TriggerInfo {
        TriggerInfo {
            id,
            status,
            lifecycle_generation: 1,
            mode: TriggerMode::Once,
            source_tab: binding(0, "source-target"),
            target_tab: binding(1, "target-target"),
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::LocatorPresent,
                locator: Some(CanonicalLocator::Selector {
                    css: "#ready".to_string(),
                    selection: None,
                }),
                text: None,
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
                payload: Some(json!({ "selector": "#continue" })),
            },
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        }
    }

    #[test]
    fn trigger_runtime_tracks_counts_and_status() {
        let mut state = TriggerRuntimeState::default();
        let projection = state.replace(vec![
            trigger(1, TriggerStatus::Armed),
            trigger(2, TriggerStatus::Paused),
            trigger(3, TriggerStatus::Degraded),
        ]);

        assert_eq!(projection.active_count, 1);
        assert_eq!(projection.degraded_count, 1);
        assert_eq!(projection.status, TriggerRuntimeStatus::Degraded);
        assert_eq!(projection.triggers[0].target_tab.target_id, "target-target");
    }

    #[test]
    fn trigger_runtime_marks_empty_registry_inactive() {
        let mut state = TriggerRuntimeState::default();
        let projection = state.replace(Vec::new());

        assert_eq!(projection.status, TriggerRuntimeStatus::Inactive);
        assert_eq!(projection.active_count, 0);
        assert_eq!(projection.degraded_count, 0);
    }

    #[test]
    fn trigger_runtime_records_condition_evidence_and_outcome() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(7, TriggerStatus::Armed)]);

        let trigger = state
            .set_condition_evidence(
                7,
                1,
                Some(TriggerEvidenceInfo {
                    summary: "source_tab_url_match".to_string(),
                    fingerprint: Some("https://example.com/source".to_string()),
                }),
            )
            .expect("trigger");
        assert_eq!(
            trigger
                .last_condition_evidence
                .as_ref()
                .map(|evidence| evidence.summary.as_str()),
            Some("source_tab_url_match")
        );

        let trigger = state
            .record_outcome(
                7,
                trigger.lifecycle_generation,
                trigger.last_condition_evidence.clone(),
                TriggerResultInfo {
                    trigger_id: 7,
                    status: TriggerStatus::Fired,
                    next_status: TriggerStatus::Fired,
                    summary: "trigger action executed".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: None,
                    reason: None,
                    error_context: None,
                    consumed_evidence_fingerprint: None,
                },
            )
            .expect("outcome");
        assert_eq!(trigger.status, TriggerStatus::Fired);
        assert_eq!(
            state
                .projection()
                .last_trigger_result
                .as_ref()
                .map(|result| result.summary.as_str()),
            Some("trigger action executed")
        );
        let trace = state.trace(10);
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].kind, TriggerEventKind::Fired);
        assert_eq!(trace.events[0].trigger_id, Some(7));
    }

    #[test]
    fn trigger_failure_outcome_preserves_armed_lifecycle_while_recording_blocked_result() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(14, TriggerStatus::Armed)]);

        let trigger = state
            .record_outcome(
                14,
                1,
                None,
                TriggerResultInfo {
                    trigger_id: 14,
                    status: TriggerStatus::Blocked,
                    next_status: TriggerStatus::Armed,
                    summary: "trigger action blocked".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::InvalidInput),
                    reason: Some("invalid_action".to_string()),
                    error_context: None,
                    consumed_evidence_fingerprint: None,
                },
            )
            .expect("outcome");

        assert_eq!(trigger.status, TriggerStatus::Armed);
        assert_eq!(
            trigger
                .last_action_result
                .as_ref()
                .map(|result| result.status),
            Some(TriggerStatus::Blocked)
        );
        assert_eq!(
            trigger
                .last_action_result
                .as_ref()
                .map(|result| result.next_status),
            Some(TriggerStatus::Armed)
        );
        let trace = state.trace(10);
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].kind, TriggerEventKind::Blocked);
    }

    #[test]
    fn trigger_runtime_degraded_override_is_truthful() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(1, TriggerStatus::Armed)]);
        let degraded = state.mark_degraded("evaluator_unavailable");

        assert_eq!(degraded.status, TriggerRuntimeStatus::Degraded);
        assert_eq!(
            degraded.degraded_reason.as_deref(),
            Some("evaluator_unavailable")
        );
    }

    #[test]
    fn trigger_runtime_reconciles_live_tab_bindings_and_missing_targets() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![
            trigger(1, TriggerStatus::Armed),
            trigger(2, TriggerStatus::Paused),
        ]);

        let projection = state.reconcile_tabs(&[
            TabInfo {
                index: 3,
                target_id: "source-target".to_string(),
                url: "https://source.example/live".to_string(),
                title: "Source Live".to_string(),
                active: true,
                active_authority: None,
                degraded_reason: None,
            },
            TabInfo {
                index: 7,
                target_id: "target-target".to_string(),
                url: "https://target.example/live".to_string(),
                title: "Target Live".to_string(),
                active: false,
                active_authority: None,
                degraded_reason: None,
            },
        ]);

        assert_eq!(projection.active_count, 1);
        assert_eq!(projection.degraded_count, 0);
        assert_eq!(projection.triggers[0].source_tab.index, 3);
        assert_eq!(
            projection.triggers[0].target_tab.url,
            "https://target.example/live"
        );
        assert!(projection.triggers[0].unavailable_reason.is_none());

        let degraded = state.reconcile_tabs(&[TabInfo {
            index: 3,
            target_id: "source-target".to_string(),
            url: "https://source.example/live".to_string(),
            title: "Source Live".to_string(),
            active: true,
            active_authority: None,
            degraded_reason: None,
        }]);
        assert_eq!(degraded.active_count, 0);
        assert_eq!(degraded.degraded_count, 2);
        assert_eq!(degraded.status, TriggerRuntimeStatus::Degraded);
        assert_eq!(
            degraded.triggers[0].unavailable_reason.as_deref(),
            Some("target_tab_missing")
        );
        let trace = state.trace(10);
        assert_eq!(trace.events.len(), 2);
        assert_eq!(trace.events[0].kind, TriggerEventKind::Unavailable);
        assert_eq!(trace.events[1].kind, TriggerEventKind::Unavailable);
    }

    #[test]
    fn trigger_runtime_preserves_tab_page_identity_when_live_tab_projection_is_degraded() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(1, TriggerStatus::Armed)]);

        let projection = state.reconcile_tabs(&[
            TabInfo {
                index: 5,
                target_id: "source-target".to_string(),
                url: String::new(),
                title: String::new(),
                active: true,
                active_authority: None,
                degraded_reason: Some("tab_url_and_title_probe_failed".to_string()),
            },
            TabInfo {
                index: 7,
                target_id: "target-target".to_string(),
                url: "https://target.example/live".to_string(),
                title: "Target Live".to_string(),
                active: false,
                active_authority: None,
                degraded_reason: None,
            },
        ]);

        assert_eq!(projection.triggers[0].source_tab.index, 5);
        assert_eq!(
            projection.triggers[0].source_tab.url,
            "https://example.com/0"
        );
        assert_eq!(projection.triggers[0].source_tab.title, "Tab 0");
        assert_eq!(
            projection.triggers[0].unavailable_reason.as_deref(),
            Some("source_tab_projection_degraded")
        );
        assert_eq!(projection.active_count, 0);
        assert_eq!(projection.degraded_count, 1);
        assert_eq!(projection.status, TriggerRuntimeStatus::Degraded);
    }

    #[test]
    fn trigger_runtime_marks_mixed_missing_and_degraded_tab_authority_truthfully() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(1, TriggerStatus::Armed)]);

        let projection = state.reconcile_tabs(&[TabInfo {
            index: 5,
            target_id: "target-target".to_string(),
            url: String::new(),
            title: String::new(),
            active: true,
            active_authority: None,
            degraded_reason: Some("tab_url_and_title_probe_failed".to_string()),
        }]);

        assert_eq!(
            projection.triggers[0].unavailable_reason.as_deref(),
            Some("source_tab_missing_and_target_projection_degraded")
        );
        assert_eq!(projection.active_count, 0);
        assert_eq!(projection.degraded_count, 1);
    }

    #[test]
    fn trigger_runtime_records_registry_lifecycle_events() {
        let mut state = TriggerRuntimeState::default();
        let registered = state.register(trigger(9, TriggerStatus::Armed));
        assert_eq!(registered.id, 9);

        let paused = state
            .update_status(9, TriggerStatus::Paused)
            .expect("paused");
        assert_eq!(paused.status, TriggerStatus::Paused);

        let removed = state.remove(9).expect("removed");
        assert_eq!(removed.id, 9);

        let trace = state.trace(10);
        assert_eq!(trace.events.len(), 3);
        assert_eq!(trace.events[0].kind, TriggerEventKind::Registered);
        assert_eq!(trace.events[1].kind, TriggerEventKind::Paused);
        assert_eq!(trace.events[2].kind, TriggerEventKind::Removed);
    }

    #[test]
    fn trigger_runtime_preserves_structured_result_reason_in_trace() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(11, TriggerStatus::Armed)]);

        let trigger = state
            .record_outcome(
                11,
                1,
                Some(TriggerEvidenceInfo {
                    summary: "source_tab_text_present:Ready".to_string(),
                    fingerprint: Some("Ready".to_string()),
                }),
                TriggerResultInfo {
                    trigger_id: 11,
                    status: TriggerStatus::Degraded,
                    next_status: TriggerStatus::Armed,
                    summary: "trigger action failed: SESSION_BUSY: Trigger target continuity fence failed: frame context became unavailable".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::SessionBusy),
                    reason: Some("continuity_frame_unavailable".to_string()),
                    error_context: Some(serde_json::json!({
                        "reason": "continuity_frame_unavailable",
                        "target_tab_target_id": "target",
                        "phase": "action",
                    })),
                    consumed_evidence_fingerprint: None,
                },
            )
            .expect("outcome");

        assert_eq!(trigger.status, TriggerStatus::Armed);
        assert_eq!(
            trigger
                .last_action_result
                .as_ref()
                .and_then(|result| result.error_code),
            Some(rub_core::error::ErrorCode::SessionBusy)
        );
        assert_eq!(
            trigger
                .last_action_result
                .as_ref()
                .and_then(|result| result.reason.as_deref()),
            Some("continuity_frame_unavailable")
        );
        assert_eq!(
            trigger
                .last_action_result
                .as_ref()
                .and_then(|result| result.error_context.as_ref())
                .and_then(|context| context.get("target_tab_target_id"))
                .and_then(|value| value.as_str()),
            Some("target")
        );
        assert_eq!(state.projection().degraded_count, 1);

        let trace = state.trace(10);
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].kind, TriggerEventKind::Degraded);
        assert_eq!(
            trace.events[0]
                .result
                .as_ref()
                .and_then(|result| result.error_code),
            Some(rub_core::error::ErrorCode::SessionBusy)
        );
        assert_eq!(
            trace.events[0]
                .result
                .as_ref()
                .and_then(|result| result.reason.as_deref()),
            Some("continuity_frame_unavailable")
        );
        assert_eq!(
            trace.events[0]
                .result
                .as_ref()
                .and_then(|result| result.error_context.as_ref())
                .and_then(|context| context.get("phase"))
                .and_then(|value| value.as_str()),
            Some("action")
        );
    }

    #[test]
    fn trigger_runtime_records_orphan_outcome_trace_when_trigger_is_missing() {
        let mut state = TriggerRuntimeState::default();
        let trigger = trigger(21, TriggerStatus::Armed);
        state.register(trigger.clone());
        state
            .remove(trigger.id)
            .expect("trigger should remove cleanly");

        let commit = state.record_outcome_with_fallback(
            &trigger,
            trigger.lifecycle_generation,
            Some(TriggerEvidenceInfo {
                summary: "source_tab_text_present:Ready".to_string(),
                fingerprint: Some("Ready".to_string()),
            }),
            TriggerResultInfo {
                trigger_id: trigger.id,
                status: TriggerStatus::Fired,
                next_status: TriggerStatus::Fired,
                summary: "trigger action executed".to_string(),
                command_id: Some("cmd-1".to_string()),
                action: None,
                result: Some(serde_json::json!({"ok": true})),
                error_code: None,
                reason: None,
                error_context: None,
                consumed_evidence_fingerprint: Some("Ready".to_string()),
            },
        );

        assert!(matches!(commit, TriggerOutcomeCommit::Applied(None)));
        let projection = state.projection();
        assert_eq!(projection.last_trigger_id, Some(trigger.id));
        assert_eq!(
            projection
                .last_trigger_result
                .as_ref()
                .map(|result| result.status),
            Some(TriggerStatus::Fired)
        );
        let trace = state.trace(10);
        assert!(
            trace.events.iter().any(|event| {
                event.trigger_id == Some(trigger.id) && event.kind == TriggerEventKind::Fired
            }),
            "{trace:?}"
        );
    }

    #[test]
    fn trigger_runtime_preserves_committed_outcome_when_generation_is_stale() {
        let mut state = TriggerRuntimeState::default();
        let trigger = trigger(22, TriggerStatus::Armed);
        state.register(trigger.clone());
        let paused = state
            .update_status(trigger.id, TriggerStatus::Paused)
            .expect("pause should update trigger");
        assert_eq!(paused.lifecycle_generation, 2);

        let commit = state.record_outcome_with_fallback(
            &trigger,
            trigger.lifecycle_generation,
            Some(TriggerEvidenceInfo {
                summary: "source_tab_text_present:Ready".to_string(),
                fingerprint: Some("Ready".to_string()),
            }),
            TriggerResultInfo {
                trigger_id: trigger.id,
                status: TriggerStatus::Fired,
                next_status: TriggerStatus::Fired,
                summary: "trigger action executed".to_string(),
                command_id: Some("cmd-2".to_string()),
                action: None,
                result: Some(serde_json::json!({"ok": true})),
                error_code: None,
                reason: None,
                error_context: None,
                consumed_evidence_fingerprint: Some("Ready".to_string()),
            },
        );

        assert!(matches!(commit, TriggerOutcomeCommit::Stale(Some(_))));
        let live_trigger = state
            .triggers()
            .into_iter()
            .find(|candidate| candidate.id == trigger.id)
            .expect("trigger should still exist");
        assert_eq!(live_trigger.status, TriggerStatus::Paused);
        assert_eq!(live_trigger.lifecycle_generation, 2);
        assert_eq!(
            live_trigger
                .last_action_result
                .as_ref()
                .map(|result| result.status),
            Some(TriggerStatus::Fired)
        );
        assert_eq!(
            live_trigger
                .last_condition_evidence
                .as_ref()
                .map(|evidence| evidence.summary.as_str()),
            Some("source_tab_text_present:Ready")
        );
        assert_eq!(live_trigger.consumed_evidence_fingerprint, None);
        let projection = state.projection();
        assert_eq!(projection.last_trigger_id, Some(trigger.id));
        assert_eq!(
            projection
                .last_trigger_result
                .as_ref()
                .map(|result| result.status),
            Some(TriggerStatus::Fired)
        );
        let trace = state.trace(10);
        assert!(
            trace.events.iter().any(|event| {
                event.kind == TriggerEventKind::Degraded
                    && event.trigger_id == Some(trigger.id)
                    && event
                        .result
                        .as_ref()
                        .and_then(|result| result.reason.as_deref())
                        == Some("trigger_lifecycle_generation_stale")
            }),
            "{trace:?}"
        );
    }

    #[test]
    fn armed_trigger_with_degraded_last_result_keeps_lifecycle_but_marks_runtime_degraded() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(12, TriggerStatus::Armed)]);

        let projection = state
            .record_outcome(
                12,
                1,
                None,
                TriggerResultInfo {
                    trigger_id: 12,
                    status: TriggerStatus::Blocked,
                    next_status: TriggerStatus::Armed,
                    summary: "trigger action failed".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::InvalidInput),
                    reason: Some("invalid_action".to_string()),
                    error_context: None,
                    consumed_evidence_fingerprint: None,
                },
            )
            .expect("outcome");

        assert_eq!(projection.status, TriggerStatus::Armed);
        let runtime = state.projection();
        assert_eq!(runtime.active_count, 1);
        assert_eq!(runtime.degraded_count, 1);
        assert_eq!(runtime.status, TriggerRuntimeStatus::Degraded);
        assert_eq!(
            runtime
                .triggers
                .first()
                .and_then(|trigger| trigger.last_action_result.as_ref())
                .map(|result| result.status),
            Some(TriggerStatus::Blocked)
        );
    }

    #[test]
    fn blocked_trigger_outcome_consumes_evidence_until_condition_clears() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(13, TriggerStatus::Armed)]);

        let trigger = state
            .record_outcome(
                13,
                1,
                Some(TriggerEvidenceInfo {
                    summary: "network_request_matched:req-13".to_string(),
                    fingerprint: Some("req-13".to_string()),
                }),
                TriggerResultInfo {
                    trigger_id: 13,
                    status: TriggerStatus::Blocked,
                    next_status: TriggerStatus::Armed,
                    summary: "trigger action failed".to_string(),
                    command_id: Some("trigger:13:abcd".to_string()),
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::InvalidInput),
                    reason: Some("invalid_action".to_string()),
                    error_context: None,
                    consumed_evidence_fingerprint: Some("req-13".to_string()),
                },
            )
            .expect("blocked outcome");

        assert_eq!(
            trigger.consumed_evidence_fingerprint.as_deref(),
            Some("req-13")
        );

        let trigger = state
            .set_condition_evidence(13, trigger.lifecycle_generation, None)
            .expect("clear evidence after condition clears");
        assert!(trigger.consumed_evidence_fingerprint.is_none());
    }

    #[test]
    fn trigger_runtime_rejects_stale_outcome_generation_after_pause() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(21, TriggerStatus::Armed)]);

        let paused = state
            .update_status(21, TriggerStatus::Paused)
            .expect("pause should update trigger");
        assert_eq!(paused.lifecycle_generation, 2);

        let stale = state.record_outcome(
            21,
            1,
            None,
            TriggerResultInfo {
                trigger_id: 21,
                status: TriggerStatus::Fired,
                next_status: TriggerStatus::Fired,
                summary: "trigger action executed".to_string(),
                command_id: None,
                action: None,
                result: None,
                error_code: None,
                reason: None,
                error_context: None,
                consumed_evidence_fingerprint: None,
            },
        );

        assert!(stale.is_none());
        let runtime = state.projection();
        assert_eq!(runtime.triggers[0].status, TriggerStatus::Paused);
        assert_eq!(runtime.triggers[0].lifecycle_generation, 2);
        assert!(runtime.triggers[0].last_action_result.is_none());
    }

    #[test]
    fn trigger_runtime_rejects_stale_condition_evidence_generation_after_pause() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(22, TriggerStatus::Armed)]);

        let paused = state
            .update_status(22, TriggerStatus::Paused)
            .expect("pause should update trigger");
        assert_eq!(paused.lifecycle_generation, 2);

        let stale = state.set_condition_evidence(
            22,
            1,
            Some(TriggerEvidenceInfo {
                summary: "source_tab_url_match".to_string(),
                fingerprint: Some("https://example.com/source".to_string()),
            }),
        );

        assert!(stale.is_none());
        let runtime = state.projection();
        assert_eq!(runtime.triggers[0].status, TriggerStatus::Paused);
        assert_eq!(runtime.triggers[0].lifecycle_generation, 2);
        assert!(runtime.triggers[0].last_condition_evidence.is_none());
    }

    #[test]
    fn trigger_runtime_commits_network_request_baseline_on_register_and_resume() {
        let mut state = TriggerRuntimeState::default();
        let mut network_trigger = trigger(30, TriggerStatus::Armed);
        network_trigger.condition.kind = TriggerConditionKind::NetworkRequest;

        state.register_with_network_baseline(
            network_trigger.clone(),
            Some(crate::session::NetworkRequestBaseline {
                cursor: 7,
                observed_ingress_drop_count: 2,
                primed: true,
            }),
        );
        assert_eq!(
            state
                .network_request_baselines()
                .get(&30)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((7, 2))
        );

        state
            .update_status(30, TriggerStatus::Paused)
            .expect("pause should update trigger");
        assert!(
            !state.network_request_baselines().contains_key(&30),
            "paused network trigger must not keep an armed baseline"
        );

        state
            .update_status_with_network_baseline(
                30,
                TriggerStatus::Armed,
                Some(crate::session::NetworkRequestBaseline {
                    cursor: 11,
                    observed_ingress_drop_count: 4,
                    primed: true,
                }),
            )
            .expect("resume should update trigger");
        assert_eq!(
            state
                .network_request_baselines()
                .get(&30)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((11, 4))
        );
    }

    #[test]
    fn trigger_runtime_preserves_and_backfills_armed_network_request_baselines() {
        let mut state = TriggerRuntimeState::default();
        let mut network_trigger = trigger(31, TriggerStatus::Armed);
        network_trigger.condition.kind = TriggerConditionKind::NetworkRequest;

        state.register(network_trigger.clone());
        assert!(
            !state.network_request_baselines().contains_key(&31),
            "generic register must not invent a baseline for armed network triggers"
        );

        let ensured = state.ensure_network_request_baseline(
            31,
            crate::session::NetworkRequestBaseline {
                cursor: 19,
                observed_ingress_drop_count: 6,
                primed: true,
            },
        );
        assert!(
            ensured.is_some(),
            "armed trigger should accept baseline backfill"
        );
        assert_eq!(
            state
                .network_request_baselines()
                .get(&31)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((19, 6))
        );

        state
            .update_status(31, TriggerStatus::Armed)
            .expect("generic armed update should preserve existing baseline");
        assert_eq!(
            state
                .network_request_baselines()
                .get(&31)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((19, 6))
        );
    }
}
