use std::collections::VecDeque;

use rub_core::model::{
    TabInfo, TriggerEventInfo, TriggerEventKind, TriggerEvidenceInfo, TriggerInfo,
    TriggerResultInfo, TriggerRuntimeInfo, TriggerRuntimeStatus, TriggerStatus,
    TriggerTraceProjection,
};

mod events;
mod mutation;
mod projection;

const TRIGGER_EVENT_LIMIT: usize = 64;

/// Session-scoped trigger registry authority.
#[derive(Debug, Default)]
pub struct TriggerRuntimeState {
    projection: TriggerRuntimeInfo,
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
}

#[cfg(test)]
mod tests {
    use super::TriggerRuntimeState;
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
            url: format!("https://example.com/{index}"),
            title: format!("Tab {index}"),
        }
    }

    fn trigger(id: u32, status: TriggerStatus) -> TriggerInfo {
        TriggerInfo {
            id,
            status,
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
                TriggerStatus::Fired,
                trigger.last_condition_evidence.clone(),
                TriggerResultInfo {
                    trigger_id: 7,
                    status: TriggerStatus::Fired,
                    summary: "trigger action executed".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: None,
                    reason: None,
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
            },
            TabInfo {
                index: 7,
                target_id: "target-target".to_string(),
                url: "https://target.example/live".to_string(),
                title: "Target Live".to_string(),
                active: false,
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
                TriggerStatus::Armed,
                Some(TriggerEvidenceInfo {
                    summary: "source_tab_text_present:Ready".to_string(),
                    fingerprint: Some("Ready".to_string()),
                }),
                TriggerResultInfo {
                    trigger_id: 11,
                    status: TriggerStatus::Degraded,
                    summary: "trigger action failed: BROWSER_CRASHED: Trigger target continuity fence failed: frame context became unavailable".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::BrowserCrashed),
                    reason: Some("continuity_frame_unavailable".to_string()),
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
            Some(rub_core::error::ErrorCode::BrowserCrashed)
        );
        assert_eq!(
            trigger
                .last_action_result
                .as_ref()
                .and_then(|result| result.reason.as_deref()),
            Some("continuity_frame_unavailable")
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
            Some(rub_core::error::ErrorCode::BrowserCrashed)
        );
        assert_eq!(
            trace.events[0]
                .result
                .as_ref()
                .and_then(|result| result.reason.as_deref()),
            Some("continuity_frame_unavailable")
        );
    }

    #[test]
    fn armed_trigger_with_degraded_last_result_keeps_lifecycle_but_marks_runtime_degraded() {
        let mut state = TriggerRuntimeState::default();
        state.replace(vec![trigger(12, TriggerStatus::Armed)]);

        let projection = state
            .record_outcome(
                12,
                TriggerStatus::Armed,
                None,
                TriggerResultInfo {
                    trigger_id: 12,
                    status: TriggerStatus::Blocked,
                    summary: "trigger action failed".to_string(),
                    command_id: None,
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::InvalidInput),
                    reason: Some("invalid_action".to_string()),
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
                TriggerStatus::Armed,
                Some(TriggerEvidenceInfo {
                    summary: "network_request_matched:req-13".to_string(),
                    fingerprint: Some("req-13".to_string()),
                }),
                TriggerResultInfo {
                    trigger_id: 13,
                    status: TriggerStatus::Blocked,
                    summary: "trigger action failed".to_string(),
                    command_id: Some("trigger:13:abcd".to_string()),
                    action: None,
                    result: None,
                    error_code: Some(rub_core::error::ErrorCode::InvalidInput),
                    reason: Some("invalid_action".to_string()),
                    consumed_evidence_fingerprint: Some("req-13".to_string()),
                },
            )
            .expect("blocked outcome");

        assert_eq!(
            trigger.consumed_evidence_fingerprint.as_deref(),
            Some("req-13")
        );

        let trigger = state
            .set_condition_evidence(13, None)
            .expect("clear evidence after condition clears");
        assert!(trigger.consumed_evidence_fingerprint.is_none());
    }
}
