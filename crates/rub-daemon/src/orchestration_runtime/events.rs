use super::*;

pub(super) fn orchestration_outcome_event_kind(
    status: OrchestrationRuleStatus,
) -> Option<OrchestrationEventKind> {
    match status {
        OrchestrationRuleStatus::Fired => Some(OrchestrationEventKind::Fired),
        OrchestrationRuleStatus::Blocked => Some(OrchestrationEventKind::Blocked),
        OrchestrationRuleStatus::Degraded => Some(OrchestrationEventKind::Degraded),
        OrchestrationRuleStatus::Armed
        | OrchestrationRuleStatus::Paused
        | OrchestrationRuleStatus::Expired => None,
    }
}

impl OrchestrationRuntimeState {
    pub(super) fn reconcile_sessions(&mut self) {
        let mut pending_events = Vec::new();
        for rule in &mut self.projection.rules {
            let previous_unavailable_reason = rule.unavailable_reason.clone();
            let source = self
                .projection
                .known_sessions
                .iter()
                .find(|session| session.session_id == rule.source.session_id);
            let target = self
                .projection
                .known_sessions
                .iter()
                .find(|session| session.session_id == rule.target.session_id);

            if let Some(session) = source {
                rule.source.session_name = session.session_name.clone();
            }
            if let Some(session) = target {
                rule.target.session_name = session.session_name.clone();
            }

            rule.unavailable_reason = match (source.is_some(), target.is_some()) {
                (true, true) => None,
                (false, false) => Some("source_and_target_sessions_missing".to_string()),
                (false, true) => Some("source_session_missing".to_string()),
                (true, false) => Some("target_session_missing".to_string()),
            };

            if previous_unavailable_reason != rule.unavailable_reason {
                match (&previous_unavailable_reason, &rule.unavailable_reason) {
                    (_, Some(reason)) => pending_events.push(OrchestrationEventInfo {
                        sequence: 0,
                        kind: OrchestrationEventKind::Unavailable,
                        rule_id: Some(rule.id),
                        summary: format!(
                            "orchestration rule {} became unavailable: {reason}",
                            rule.id
                        ),
                        unavailable_reason: Some(reason.clone()),
                        evidence: rule.last_condition_evidence.clone(),
                        correlation_key: Some(rule.correlation_key.clone()),
                        idempotency_key: Some(rule.idempotency_key.clone()),
                        error_code: None,
                        reason: None,
                        committed_steps: None,
                        total_steps: None,
                    }),
                    (Some(_), None) => pending_events.push(OrchestrationEventInfo {
                        sequence: 0,
                        kind: OrchestrationEventKind::Recovered,
                        rule_id: Some(rule.id),
                        summary: format!(
                            "orchestration rule {} recovered source/target session availability",
                            rule.id
                        ),
                        unavailable_reason: None,
                        evidence: rule.last_condition_evidence.clone(),
                        correlation_key: Some(rule.correlation_key.clone()),
                        idempotency_key: Some(rule.idempotency_key.clone()),
                        error_code: None,
                        reason: None,
                        committed_steps: None,
                        total_steps: None,
                    }),
                    (None, None) => {}
                }
            }
        }

        for event in pending_events {
            self.push_event(event);
        }
    }

    pub(super) fn push_event(&mut self, mut event: OrchestrationEventInfo) {
        let sequence = self.next_event_sequence.max(1);
        self.next_event_sequence = sequence + 1;
        event.sequence = sequence;
        self.recent_events.push_back(event);
        while self.recent_events.len() > ORCHESTRATION_EVENT_LIMIT {
            self.recent_events.pop_front();
        }
    }
}
