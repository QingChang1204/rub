use super::*;
use rub_core::model::OrchestrationSessionInfo;

impl OrchestrationRuntimeState {
    pub fn register(&mut self, rule: OrchestrationRuleInfo) -> Result<OrchestrationRuleInfo, u32> {
        if let Some(existing) = self
            .projection
            .rules
            .iter()
            .find(|existing| existing.idempotency_key == rule.idempotency_key)
        {
            return Err(existing.id);
        }
        self.projection.last_rule_id = Some(rule.id);
        self.projection.rules.push(rule.clone());
        self.push_event(OrchestrationEventInfo {
            sequence: 0,
            kind: OrchestrationEventKind::Registered,
            rule_id: Some(rule.id),
            summary: format!("orchestration rule {} registered", rule.id),
            unavailable_reason: rule.unavailable_reason.clone(),
            evidence: rule.last_condition_evidence.clone(),
            correlation_key: Some(rule.correlation_key.clone()),
            idempotency_key: Some(rule.idempotency_key.clone()),
            error_code: None,
            reason: None,
            committed_steps: None,
            total_steps: None,
        });
        self.refresh_counts();
        self.refresh_status();
        Ok(rule)
    }

    pub fn update_status(
        &mut self,
        id: u32,
        status: OrchestrationRuleStatus,
    ) -> Option<OrchestrationRuleInfo> {
        let rule = {
            let rule = self
                .projection
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)?;
            rule.status = status;
            rule.clone()
        };
        let kind = match status {
            OrchestrationRuleStatus::Paused => Some(OrchestrationEventKind::Paused),
            OrchestrationRuleStatus::Armed => Some(OrchestrationEventKind::Resumed),
            _ => None,
        };
        if let Some(kind) = kind {
            self.push_event(OrchestrationEventInfo {
                sequence: 0,
                kind,
                rule_id: Some(rule.id),
                summary: format!(
                    "orchestration rule {} {}",
                    rule.id,
                    match kind {
                        OrchestrationEventKind::Paused => "paused",
                        OrchestrationEventKind::Resumed => "resumed",
                        _ => "updated",
                    }
                ),
                unavailable_reason: rule.unavailable_reason.clone(),
                evidence: rule.last_condition_evidence.clone(),
                correlation_key: Some(rule.correlation_key.clone()),
                idempotency_key: Some(rule.idempotency_key.clone()),
                error_code: None,
                reason: None,
                committed_steps: None,
                total_steps: None,
            });
        }
        self.refresh_counts();
        self.refresh_status();
        Some(rule)
    }

    pub fn remove(&mut self, id: u32) -> Option<OrchestrationRuleInfo> {
        let index = self
            .projection
            .rules
            .iter()
            .position(|rule| rule.id == id)?;
        let removed = self.projection.rules.remove(index);
        self.push_event(OrchestrationEventInfo {
            sequence: 0,
            kind: OrchestrationEventKind::Removed,
            rule_id: Some(removed.id),
            summary: format!("orchestration rule {} removed", removed.id),
            unavailable_reason: removed.unavailable_reason.clone(),
            evidence: removed.last_condition_evidence.clone(),
            correlation_key: Some(removed.correlation_key.clone()),
            idempotency_key: Some(removed.idempotency_key.clone()),
            error_code: None,
            reason: None,
            committed_steps: None,
            total_steps: None,
        });
        self.refresh_counts();
        self.refresh_status();
        Some(removed)
    }

    pub fn record_outcome(
        &mut self,
        id: u32,
        evidence: Option<TriggerEvidenceInfo>,
        result: OrchestrationResultInfo,
    ) -> Option<OrchestrationRuleInfo> {
        let rule = {
            let rule = self
                .projection
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)?;
            rule.status = result.next_status;
            rule.execution_policy.cooldown_until_ms = result.cooldown_until_ms;
            rule.last_condition_evidence = evidence.clone();
            rule.last_result = Some(result.clone());
            rule.clone()
        };
        self.projection.last_rule_id = Some(id);
        self.projection.last_rule_result = Some(result.clone());
        if let Some(kind) = orchestration_outcome_event_kind(result.status) {
            self.push_event(OrchestrationEventInfo {
                sequence: 0,
                kind,
                rule_id: Some(rule.id),
                summary: result.summary.clone(),
                unavailable_reason: rule.unavailable_reason.clone(),
                evidence,
                correlation_key: Some(rule.correlation_key.clone()),
                idempotency_key: Some(rule.idempotency_key.clone()),
                error_code: result.error_code,
                reason: result.reason.clone(),
                committed_steps: Some(result.committed_steps),
                total_steps: Some(result.total_steps),
            });
        }
        self.refresh_counts();
        self.refresh_status();
        Some(rule)
    }

    pub fn set_condition_evidence(
        &mut self,
        id: u32,
        evidence: Option<TriggerEvidenceInfo>,
    ) -> Option<OrchestrationRuleInfo> {
        let rule = {
            let rule = self
                .projection
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)?;
            rule.last_condition_evidence = evidence;
            rule.clone()
        };
        self.refresh_counts();
        self.refresh_status();
        Some(rule)
    }

    pub fn record_outcome_with_fallback(
        &mut self,
        rule_snapshot: &OrchestrationRuleInfo,
        evidence: Option<TriggerEvidenceInfo>,
        result: OrchestrationResultInfo,
    ) -> Option<OrchestrationRuleInfo> {
        if self
            .projection
            .rules
            .iter()
            .any(|rule| rule.id == rule_snapshot.id)
        {
            return self.record_outcome(rule_snapshot.id, evidence, result);
        }

        self.projection.last_rule_id = Some(rule_snapshot.id);
        self.projection.last_rule_result = Some(result.clone());
        if let Some(kind) = orchestration_outcome_event_kind(result.status) {
            self.push_event(OrchestrationEventInfo {
                sequence: 0,
                kind,
                rule_id: Some(rule_snapshot.id),
                summary: result.summary.clone(),
                unavailable_reason: rule_snapshot.unavailable_reason.clone(),
                evidence,
                correlation_key: Some(rule_snapshot.correlation_key.clone()),
                idempotency_key: Some(rule_snapshot.idempotency_key.clone()),
                error_code: result.error_code,
                reason: result.reason.clone(),
                committed_steps: Some(result.committed_steps),
                total_steps: Some(result.total_steps),
            });
        }
        self.refresh_counts();
        self.refresh_status();
        None
    }

    pub fn replace(
        &mut self,
        sequence: u64,
        current_session_id: String,
        current_session_name: String,
        known_sessions: Vec<OrchestrationSessionInfo>,
        degraded_reason: Option<String>,
    ) -> OrchestrationRuntimeInfo {
        if sequence < self.last_refresh_sequence {
            return self.projection();
        }
        self.last_refresh_sequence = sequence;
        self.projection.current_session_id = Some(current_session_id);
        self.projection.current_session_name = Some(current_session_name);
        self.projection.known_sessions = known_sessions;
        self.projection.session_count = self.projection.known_sessions.len();
        let supported = degraded_reason.is_none();
        self.projection.addressing_supported = supported;
        self.projection.execution_supported = supported;
        self.projection.degraded_reason = degraded_reason;
        self.reconcile_sessions();
        self.refresh_counts();
        self.refresh_status();
        self.projection()
    }

    pub fn mark_degraded(
        &mut self,
        sequence: u64,
        reason: impl Into<String>,
    ) -> OrchestrationRuntimeInfo {
        if sequence < self.last_refresh_sequence {
            return self.projection();
        }
        self.last_refresh_sequence = sequence;
        self.projection.known_sessions.clear();
        self.projection.session_count = 0;
        self.projection.addressing_supported = false;
        self.projection.execution_supported = false;
        self.projection.degraded_reason = Some(reason.into());
        self.reconcile_sessions();
        self.refresh_counts();
        self.refresh_status();
        self.projection()
    }
}
