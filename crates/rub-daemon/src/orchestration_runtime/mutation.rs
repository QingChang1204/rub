use super::*;
use crate::session::NetworkRequestBaseline;
use rub_core::model::OrchestrationSessionInfo;
use rub_core::model::TriggerConditionKind;

impl OrchestrationRuntimeState {
    pub fn register(&mut self, rule: OrchestrationRuleInfo) -> Result<OrchestrationRuleInfo, u32> {
        self.register_with_network_baseline(rule, None)
    }

    pub(crate) fn register_with_network_baseline(
        &mut self,
        rule: OrchestrationRuleInfo,
        network_baseline: Option<NetworkRequestBaseline>,
    ) -> Result<OrchestrationRuleInfo, u32> {
        let mut rule = rule;
        if let Some(existing) = self
            .projection
            .rules
            .iter()
            .find(|existing| existing.idempotency_key == rule.idempotency_key)
        {
            return Err(existing.id);
        }
        rule.lifecycle_generation = rule.lifecycle_generation.max(1);
        self.projection.last_rule_id = Some(rule.id);
        self.projection.rules.push(rule.clone());
        self.commit_network_request_baseline(&rule, network_baseline);
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
            error_context: None,
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
        let preserved_network_baseline = self.network_request_baselines.get(&id).copied();
        let rule = {
            let rule = self
                .projection
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)?;
            rule.status = status;
            rule.lifecycle_generation = rule.lifecycle_generation.saturating_add(1);
            rule.clone()
        };
        self.commit_network_request_baseline(&rule, preserved_network_baseline);
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
                error_context: None,
                committed_steps: None,
                total_steps: None,
            });
        }
        self.refresh_counts();
        self.refresh_status();
        Some(rule)
    }

    pub(crate) fn update_status_with_network_baseline(
        &mut self,
        id: u32,
        status: OrchestrationRuleStatus,
        network_baseline: Option<NetworkRequestBaseline>,
    ) -> Option<OrchestrationRuleInfo> {
        let rule = {
            let rule = self
                .projection
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)?;
            rule.status = status;
            rule.lifecycle_generation = rule.lifecycle_generation.saturating_add(1);
            rule.clone()
        };
        self.commit_network_request_baseline(&rule, network_baseline);
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
                error_context: None,
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
        self.network_request_baselines.remove(&removed.id);
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
            error_context: None,
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
        expected_generation: Option<u64>,
        evidence: Option<TriggerEvidenceInfo>,
        result: OrchestrationResultInfo,
    ) -> OrchestrationOutcomeCommit {
        let Some(index) = self.projection.rules.iter().position(|rule| rule.id == id) else {
            return OrchestrationOutcomeCommit::Stale(None);
        };
        let current = self.projection.rules[index].clone();
        if expected_generation.is_some_and(|expected| current.lifecycle_generation != expected) {
            let rule = {
                let rule = &mut self.projection.rules[index];
                rule.last_condition_evidence = evidence.clone();
                rule.last_result = Some(result.clone());
                rule.clone()
            };
            self.projection.last_rule_id = Some(id);
            self.projection.last_rule_result = Some(result.clone());
            self.push_stale_outcome_event(&rule, evidence, &result);
            self.refresh_counts();
            self.refresh_status();
            return OrchestrationOutcomeCommit::Stale(Some(rule));
        }
        let rule = {
            let rule = &mut self.projection.rules[index];
            rule.status = result.next_status;
            rule.lifecycle_generation = rule.lifecycle_generation.saturating_add(1);
            rule.execution_policy.cooldown_until_ms = result.cooldown_until_ms;
            rule.last_condition_evidence = evidence.clone();
            rule.last_result = Some(result.clone());
            rule.clone()
        };
        if !matches!(rule.status, OrchestrationRuleStatus::Armed)
            || !matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
        {
            self.network_request_baselines.remove(&rule.id);
        }
        self.projection.last_rule_id = Some(id);
        self.projection.last_rule_result = Some(result.clone());
        self.push_outcome_event(&rule, evidence, &result);
        self.refresh_counts();
        self.refresh_status();
        OrchestrationOutcomeCommit::Applied(Some(rule))
    }

    pub fn set_condition_evidence(
        &mut self,
        id: u32,
        expected_generation: Option<u64>,
        evidence: Option<TriggerEvidenceInfo>,
    ) -> Option<OrchestrationRuleInfo> {
        let rule = {
            let rule = self
                .projection
                .rules
                .iter_mut()
                .find(|rule| rule.id == id)?;
            if expected_generation.is_some_and(|expected| rule.lifecycle_generation != expected) {
                return None;
            }
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
        expected_generation: Option<u64>,
        evidence: Option<TriggerEvidenceInfo>,
        result: OrchestrationResultInfo,
    ) -> OrchestrationOutcomeCommit {
        if self
            .projection
            .rules
            .iter()
            .any(|rule| rule.id == rule_snapshot.id)
        {
            return self.record_outcome(rule_snapshot.id, expected_generation, evidence, result);
        }
        if expected_generation.is_some() {
            self.projection.last_rule_id = Some(rule_snapshot.id);
            self.projection.last_rule_result = Some(result.clone());
            self.push_stale_outcome_event(rule_snapshot, evidence, &result);
            self.refresh_counts();
            self.refresh_status();
            return OrchestrationOutcomeCommit::Stale(None);
        }

        self.projection.last_rule_id = Some(rule_snapshot.id);
        self.projection.last_rule_result = Some(result.clone());
        self.push_outcome_event(rule_snapshot, evidence, &result);
        self.refresh_counts();
        self.refresh_status();
        OrchestrationOutcomeCommit::Applied(None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace(
        &mut self,
        sequence: u64,
        current_session_id: String,
        current_session_name: String,
        known_sessions: Vec<OrchestrationSessionInfo>,
        addressing_supported: bool,
        execution_supported: bool,
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
        self.projection.addressing_supported = addressing_supported;
        self.projection.execution_supported = execution_supported;
        self.projection.degraded_reason = degraded_reason;
        self.retain_network_request_baselines();
        self.reconcile_sessions();
        self.refresh_counts();
        self.refresh_status();
        self.projection()
    }

    pub fn mark_degraded(
        &mut self,
        sequence: u64,
        current_session: OrchestrationSessionInfo,
        reason: impl Into<String>,
    ) -> OrchestrationRuntimeInfo {
        if sequence < self.last_refresh_sequence {
            return self.projection();
        }
        self.last_refresh_sequence = sequence;
        self.projection.current_session_id = Some(current_session.session_id.clone());
        self.projection.current_session_name = Some(current_session.session_name.clone());
        self.projection.known_sessions = vec![current_session];
        self.projection.session_count = 1;
        self.projection.addressing_supported = false;
        self.projection.execution_supported = true;
        self.projection.degraded_reason = Some(reason.into());
        self.retain_network_request_baselines();
        self.reconcile_sessions();
        self.refresh_counts();
        self.refresh_status();
        self.projection()
    }

    fn commit_network_request_baseline(
        &mut self,
        rule: &OrchestrationRuleInfo,
        network_baseline: Option<NetworkRequestBaseline>,
    ) {
        if matches!(rule.status, OrchestrationRuleStatus::Armed)
            && matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
        {
            if let Some(network_baseline) = network_baseline {
                self.network_request_baselines
                    .insert(rule.id, network_baseline);
            }
            return;
        }
        self.network_request_baselines.remove(&rule.id);
    }

    fn retain_network_request_baselines(&mut self) {
        self.network_request_baselines.retain(|id, _| {
            self.projection.rules.iter().any(|rule| {
                rule.id == *id
                    && matches!(rule.status, OrchestrationRuleStatus::Armed)
                    && matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
            })
        });
    }

    fn push_outcome_event(
        &mut self,
        rule: &OrchestrationRuleInfo,
        evidence: Option<TriggerEvidenceInfo>,
        result: &OrchestrationResultInfo,
    ) {
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
                error_context: result.error_context.clone(),
                committed_steps: Some(result.committed_steps),
                total_steps: Some(result.total_steps),
            });
        }
    }

    fn push_stale_outcome_event(
        &mut self,
        rule: &OrchestrationRuleInfo,
        evidence: Option<TriggerEvidenceInfo>,
        result: &OrchestrationResultInfo,
    ) {
        self.push_event(OrchestrationEventInfo {
            sequence: 0,
            kind: OrchestrationEventKind::Degraded,
            rule_id: Some(rule.id),
            summary: format!(
                "orchestration rule {} skipped stale lifecycle outcome '{}' because newer rule authority won",
                rule.id, result.summary
            ),
            unavailable_reason: rule.unavailable_reason.clone(),
            evidence,
            correlation_key: Some(rule.correlation_key.clone()),
            idempotency_key: Some(rule.idempotency_key.clone()),
            error_code: result.error_code,
            reason: Some("orchestration_lifecycle_generation_stale".to_string()),
            error_context: None,
            committed_steps: Some(result.committed_steps),
            total_steps: Some(result.total_steps),
        });
    }
}
