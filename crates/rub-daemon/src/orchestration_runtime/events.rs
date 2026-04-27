use super::*;
use rub_core::model::OrchestrationSessionInfo;

#[derive(Clone, Copy)]
enum RuleSessionState {
    Addressable,
    Missing,
    NotAddressable,
}

fn rule_session_state(session: Option<&OrchestrationSessionInfo>) -> RuleSessionState {
    match session {
        None => RuleSessionState::Missing,
        Some(session) if super::orchestration_session_addressability_reason(session).is_some() => {
            RuleSessionState::NotAddressable
        }
        Some(_) => RuleSessionState::Addressable,
    }
}

fn unavailable_reason_for_rule_sessions(
    source: RuleSessionState,
    target: RuleSessionState,
) -> Option<String> {
    match (source, target) {
        (RuleSessionState::Addressable, RuleSessionState::Addressable) => None,
        (RuleSessionState::Missing, RuleSessionState::Missing) => {
            Some("source_and_target_sessions_missing".to_string())
        }
        (RuleSessionState::Missing, RuleSessionState::Addressable) => {
            Some("source_session_missing".to_string())
        }
        (RuleSessionState::Addressable, RuleSessionState::Missing) => {
            Some("target_session_missing".to_string())
        }
        (RuleSessionState::NotAddressable, RuleSessionState::Addressable) => {
            Some("source_session_not_addressable".to_string())
        }
        (RuleSessionState::Addressable, RuleSessionState::NotAddressable) => {
            Some("target_session_not_addressable".to_string())
        }
        (RuleSessionState::NotAddressable, RuleSessionState::NotAddressable) => {
            Some("source_and_target_sessions_not_addressable".to_string())
        }
        (RuleSessionState::Missing, RuleSessionState::NotAddressable) => {
            Some("source_session_missing_target_not_addressable".to_string())
        }
        (RuleSessionState::NotAddressable, RuleSessionState::Missing) => {
            Some("source_session_not_addressable_target_missing".to_string())
        }
    }
}

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

            rule.unavailable_reason = unavailable_reason_for_rule_sessions(
                rule_session_state(source),
                rule_session_state(target),
            );

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
                        error_context: None,
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
                        error_context: None,
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
