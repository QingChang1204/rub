use std::collections::VecDeque;

use rub_core::model::{
    TabInfo, TriggerEventInfo, TriggerEventKind, TriggerEvidenceInfo, TriggerInfo,
    TriggerResultInfo, TriggerRuntimeInfo, TriggerRuntimeStatus, TriggerStatus,
    TriggerTraceProjection,
};

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

    pub fn replace(&mut self, triggers: Vec<TriggerInfo>) -> TriggerRuntimeInfo {
        self.projection.triggers = triggers;
        self.refresh_status();
        self.projection()
    }

    pub fn triggers(&self) -> Vec<TriggerInfo> {
        self.projection.triggers.clone()
    }

    pub fn register(&mut self, trigger: TriggerInfo) -> TriggerInfo {
        self.projection.triggers.push(trigger.clone());
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: TriggerEventKind::Registered,
            trigger_id: Some(trigger.id),
            summary: format!("trigger {} registered", trigger.id),
            unavailable_reason: None,
            evidence: None,
            result: None,
        });
        self.refresh_status();
        trigger
    }

    pub fn update_status(&mut self, id: u32, status: TriggerStatus) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.status = status;
            trigger.clone()
        };
        let kind = match status {
            TriggerStatus::Paused => Some(TriggerEventKind::Paused),
            TriggerStatus::Armed => Some(TriggerEventKind::Resumed),
            _ => None,
        };
        if let Some(kind) = kind {
            self.push_event(TriggerEventInfo {
                sequence: 0,
                kind,
                trigger_id: Some(trigger.id),
                summary: format!(
                    "trigger {} {}",
                    trigger.id,
                    match kind {
                        TriggerEventKind::Paused => "paused",
                        TriggerEventKind::Resumed => "resumed",
                        _ => "updated",
                    }
                ),
                unavailable_reason: trigger.unavailable_reason.clone(),
                evidence: trigger.last_condition_evidence.clone(),
                result: trigger.last_action_result.clone(),
            });
        }
        self.refresh_status();
        Some(trigger)
    }

    pub fn set_condition_evidence(
        &mut self,
        id: u32,
        evidence: Option<TriggerEvidenceInfo>,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.last_condition_evidence = evidence;
            if trigger.last_condition_evidence.is_none() {
                trigger.consumed_evidence_fingerprint = None;
            }
            trigger.clone()
        };
        self.refresh_status();
        Some(trigger)
    }

    pub fn record_outcome(
        &mut self,
        id: u32,
        status: TriggerStatus,
        evidence: Option<TriggerEvidenceInfo>,
        result: TriggerResultInfo,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.status = status;
            trigger.last_condition_evidence = evidence;
            trigger.consumed_evidence_fingerprint = result.consumed_evidence_fingerprint.clone();
            trigger.last_action_result = Some(result.clone());
            trigger.clone()
        };
        self.set_last_result(result);
        let event_kind = event_kind_for_result_status(
            trigger
                .last_action_result
                .as_ref()
                .map(|result| result.status)
                .unwrap_or(status),
        );
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: event_kind,
            trigger_id: Some(trigger.id),
            summary: trigger
                .last_action_result
                .as_ref()
                .map(|result| result.summary.clone())
                .unwrap_or_else(|| format!("trigger {} {:?}", trigger.id, status).to_lowercase()),
            unavailable_reason: trigger.unavailable_reason.clone(),
            evidence: trigger.last_condition_evidence.clone(),
            result: trigger.last_action_result.clone(),
        });
        self.refresh_status();
        Some(trigger)
    }

    pub fn remove(&mut self, id: u32) -> Option<TriggerInfo> {
        let index = self
            .projection
            .triggers
            .iter()
            .position(|trigger| trigger.id == id)?;
        let removed = self.projection.triggers.remove(index);
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: TriggerEventKind::Removed,
            trigger_id: Some(removed.id),
            summary: format!("trigger {} removed", removed.id),
            unavailable_reason: removed.unavailable_reason.clone(),
            evidence: removed.last_condition_evidence.clone(),
            result: removed.last_action_result.clone(),
        });
        self.refresh_status();
        Some(removed)
    }

    pub fn reconcile_tabs(&mut self, tabs: &[TabInfo]) -> TriggerRuntimeInfo {
        let mut pending_events = Vec::new();
        for trigger in &mut self.projection.triggers {
            let previous_unavailable_reason = trigger.unavailable_reason.clone();
            let source = tabs
                .iter()
                .find(|tab| tab.target_id == trigger.source_tab.target_id);
            let target = tabs
                .iter()
                .find(|tab| tab.target_id == trigger.target_tab.target_id);

            if let Some(tab) = source {
                sync_binding(&mut trigger.source_tab, tab);
            }
            if let Some(tab) = target {
                sync_binding(&mut trigger.target_tab, tab);
            }

            trigger.unavailable_reason = match (source.is_some(), target.is_some()) {
                (true, true) => None,
                (false, false) => Some("source_and_target_tabs_missing".to_string()),
                (false, true) => Some("source_tab_missing".to_string()),
                (true, false) => Some("target_tab_missing".to_string()),
            };

            if previous_unavailable_reason != trigger.unavailable_reason {
                match (&previous_unavailable_reason, &trigger.unavailable_reason) {
                    (_, Some(reason)) => pending_events.push(TriggerEventInfo {
                        sequence: 0,
                        kind: TriggerEventKind::Unavailable,
                        trigger_id: Some(trigger.id),
                        summary: format!("trigger {} became unavailable: {reason}", trigger.id),
                        unavailable_reason: Some(reason.clone()),
                        evidence: trigger.last_condition_evidence.clone(),
                        result: trigger.last_action_result.clone(),
                    }),
                    (Some(_), None) => pending_events.push(TriggerEventInfo {
                        sequence: 0,
                        kind: TriggerEventKind::Recovered,
                        trigger_id: Some(trigger.id),
                        summary: format!(
                            "trigger {} recovered target/source availability",
                            trigger.id
                        ),
                        unavailable_reason: None,
                        evidence: trigger.last_condition_evidence.clone(),
                        result: trigger.last_action_result.clone(),
                    }),
                    (None, None) => {}
                }
            }
        }

        for event in pending_events {
            self.push_event(event);
        }

        self.refresh_status();
        self.projection()
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) -> TriggerRuntimeInfo {
        self.projection.status = TriggerRuntimeStatus::Degraded;
        self.projection.degraded_reason = Some(reason.into());
        self.projection()
    }

    pub fn clear_degraded(&mut self) {
        self.projection.degraded_reason = None;
        self.refresh_status();
    }

    pub fn set_last_result(&mut self, result: TriggerResultInfo) -> TriggerRuntimeInfo {
        self.projection.last_trigger_id = Some(result.trigger_id);
        self.projection.last_trigger_result = Some(result);
        self.projection()
    }

    fn push_event(&mut self, mut event: TriggerEventInfo) {
        let sequence = self.next_event_sequence.max(1);
        self.next_event_sequence = sequence + 1;
        event.sequence = sequence;
        self.recent_events.push_back(event);
        while self.recent_events.len() > TRIGGER_EVENT_LIMIT {
            self.recent_events.pop_front();
        }
    }

    fn refresh_status(&mut self) {
        self.projection.active_count = self
            .projection
            .triggers
            .iter()
            .filter(|trigger| {
                matches!(trigger.status, TriggerStatus::Armed)
                    && trigger.unavailable_reason.is_none()
            })
            .count();
        self.projection.degraded_count = self
            .projection
            .triggers
            .iter()
            .filter(|trigger| {
                matches!(trigger.status, TriggerStatus::Degraded)
                    || trigger.last_action_result.as_ref().is_some_and(|result| {
                        matches!(
                            result.status,
                            TriggerStatus::Blocked | TriggerStatus::Degraded
                        )
                    })
                    || trigger.unavailable_reason.is_some()
            })
            .count();

        self.projection.status =
            if self.projection.degraded_reason.is_some() || self.projection.degraded_count > 0 {
                TriggerRuntimeStatus::Degraded
            } else if self.projection.triggers.is_empty() {
                TriggerRuntimeStatus::Inactive
            } else {
                TriggerRuntimeStatus::Active
            };
    }
}

fn event_kind_for_result_status(status: TriggerStatus) -> TriggerEventKind {
    match status {
        TriggerStatus::Fired => TriggerEventKind::Fired,
        TriggerStatus::Blocked => TriggerEventKind::Blocked,
        TriggerStatus::Degraded => TriggerEventKind::Degraded,
        TriggerStatus::Armed => TriggerEventKind::Resumed,
        TriggerStatus::Paused => TriggerEventKind::Paused,
        TriggerStatus::Expired => TriggerEventKind::Degraded,
    }
}

fn sync_binding(binding: &mut rub_core::model::TriggerTabBindingInfo, tab: &TabInfo) {
    binding.index = tab.index;
    binding.url = tab.url.clone();
    binding.title = tab.title.clone();
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
