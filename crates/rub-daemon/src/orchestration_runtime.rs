use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use rub_core::model::{
    OrchestrationEventInfo, OrchestrationEventKind, OrchestrationGroupInfo,
    OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
    OrchestrationRuntimeInfo, OrchestrationRuntimeStatus, OrchestrationSessionInfo,
    OrchestrationTraceProjection, TriggerEvidenceInfo,
};

const ORCHESTRATION_EVENT_LIMIT: usize = 64;

/// Session-local orchestration authority built on top of the RUB_HOME registry foundation.
#[derive(Debug, Default)]
pub struct OrchestrationRuntimeState {
    projection: OrchestrationRuntimeInfo,
    last_refresh_sequence: u64,
    next_event_sequence: u64,
    recent_events: VecDeque<OrchestrationEventInfo>,
}

impl OrchestrationRuntimeState {
    pub fn projection(&self) -> OrchestrationRuntimeInfo {
        self.projection.clone()
    }

    pub fn trace(&self, last: usize) -> OrchestrationTraceProjection {
        let take = last.min(self.recent_events.len());
        let mut events = self
            .recent_events
            .iter()
            .rev()
            .take(take)
            .cloned()
            .collect::<Vec<_>>();
        events.reverse();
        OrchestrationTraceProjection { events }
    }

    pub fn rules(&self) -> Vec<OrchestrationRuleInfo> {
        self.projection.rules.clone()
    }

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
        self.projection.addressing_supported = false;
        self.projection.execution_supported = false;
        self.projection.degraded_reason = Some(reason.into());
        self.refresh_counts();
        self.refresh_status();
        self.projection()
    }

    fn reconcile_sessions(&mut self) {
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

    fn refresh_counts(&mut self) {
        let now_ms = current_time_ms();
        self.projection.groups = build_groups(&self.projection.rules);
        self.projection.group_count = self.projection.groups.len();
        self.projection.active_rule_count = self
            .projection
            .rules
            .iter()
            .filter(|rule| {
                matches!(rule.status, OrchestrationRuleStatus::Armed)
                    && rule.unavailable_reason.is_none()
                    && !rule_in_cooldown(rule, now_ms)
            })
            .count();
        self.projection.cooldown_rule_count = self
            .projection
            .rules
            .iter()
            .filter(|rule| {
                matches!(rule.status, OrchestrationRuleStatus::Armed)
                    && rule.unavailable_reason.is_none()
                    && rule_in_cooldown(rule, now_ms)
            })
            .count();
        self.projection.paused_rule_count = self
            .projection
            .rules
            .iter()
            .filter(|rule| matches!(rule.status, OrchestrationRuleStatus::Paused))
            .count();
        self.projection.unavailable_rule_count = self
            .projection
            .rules
            .iter()
            .filter(|rule| rule.unavailable_reason.is_some())
            .count();
    }

    fn refresh_status(&mut self) {
        self.projection.status = if self.projection.degraded_reason.is_some()
            || self.projection.rules.iter().any(|rule| {
                rule.last_result.as_ref().is_some_and(|result| {
                    matches!(
                        result.status,
                        OrchestrationRuleStatus::Blocked | OrchestrationRuleStatus::Degraded
                    )
                })
            }) {
            OrchestrationRuntimeStatus::Degraded
        } else if self.projection.session_count > 0 || !self.projection.rules.is_empty() {
            OrchestrationRuntimeStatus::Active
        } else {
            OrchestrationRuntimeStatus::Inactive
        };
    }

    fn push_event(&mut self, mut event: OrchestrationEventInfo) {
        let sequence = self.next_event_sequence.max(1);
        self.next_event_sequence = sequence + 1;
        event.sequence = sequence;
        self.recent_events.push_back(event);
        while self.recent_events.len() > ORCHESTRATION_EVENT_LIMIT {
            self.recent_events.pop_front();
        }
    }
}

fn orchestration_outcome_event_kind(
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

fn build_groups(rules: &[OrchestrationRuleInfo]) -> Vec<OrchestrationGroupInfo> {
    let now_ms = current_time_ms();
    let mut grouped = BTreeMap::<String, OrchestrationGroupInfo>::new();
    for rule in rules {
        let entry = grouped
            .entry(rule.correlation_key.clone())
            .or_insert_with(|| OrchestrationGroupInfo {
                correlation_key: rule.correlation_key.clone(),
                rule_ids: Vec::new(),
                active_rule_count: 0,
                cooldown_rule_count: 0,
                paused_rule_count: 0,
                unavailable_rule_count: 0,
            });
        entry.rule_ids.push(rule.id);
        if matches!(rule.status, OrchestrationRuleStatus::Armed)
            && rule.unavailable_reason.is_none()
        {
            if rule_in_cooldown(rule, now_ms) {
                entry.cooldown_rule_count += 1;
            } else {
                entry.active_rule_count += 1;
            }
        }
        if matches!(rule.status, OrchestrationRuleStatus::Paused) {
            entry.paused_rule_count += 1;
        }
        if rule.unavailable_reason.is_some() {
            entry.unavailable_rule_count += 1;
        }
    }

    let mut groups = grouped.into_values().collect::<Vec<_>>();
    for group in &mut groups {
        group.rule_ids.sort_unstable();
    }
    groups
}

fn rule_in_cooldown(rule: &OrchestrationRuleInfo, now_ms: u64) -> bool {
    rule.execution_policy
        .cooldown_until_ms
        .map(|until| until > now_ms)
        .unwrap_or(false)
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::OrchestrationRuntimeState;
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationEventKind, OrchestrationExecutionPolicyInfo,
        OrchestrationMode, OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
        OrchestrationRuntimeStatus, OrchestrationSessionInfo, TriggerActionKind, TriggerActionSpec,
        TriggerConditionKind, TriggerConditionSpec,
    };

    fn session(id: &str, name: &str, current: bool) -> OrchestrationSessionInfo {
        OrchestrationSessionInfo {
            session_id: id.to_string(),
            session_name: name.to_string(),
            pid: 1234,
            socket_path: format!("/tmp/{name}.sock"),
            current,
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: None,
        }
    }

    fn rule(source_session_id: &str, target_session_id: &str) -> OrchestrationRuleInfo {
        OrchestrationRuleInfo {
            id: 1,
            status: OrchestrationRuleStatus::Armed,
            source: OrchestrationAddressInfo {
                session_id: source_session_id.to_string(),
                session_name: "source".to_string(),
                tab_index: None,
                tab_target_id: None,
                frame_id: None,
            },
            target: OrchestrationAddressInfo {
                session_id: target_session_id.to_string(),
                session_name: "target".to_string(),
                tab_index: None,
                tab_target_id: None,
                frame_id: None,
            },
            mode: OrchestrationMode::Once,
            execution_policy: OrchestrationExecutionPolicyInfo::default(),
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
            actions: vec![TriggerActionSpec {
                kind: TriggerActionKind::Workflow,
                command: None,
                payload: Some(serde_json::json!({ "workflow_name": "reply_flow" })),
            }],
            correlation_key: "corr-1".to_string(),
            idempotency_key: "idem-1".to_string(),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        }
    }

    #[test]
    fn orchestration_runtime_defaults_to_inactive() {
        let state = OrchestrationRuntimeState::default();
        let runtime = state.projection();
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Inactive);
        assert!(!runtime.addressing_supported);
        assert!(!runtime.execution_supported);
        assert!(runtime.known_sessions.is_empty());
        assert!(runtime.rules.is_empty());
        assert!(runtime.groups.is_empty());
    }

    #[test]
    fn orchestration_runtime_projects_registry_backed_sessions() {
        let mut state = OrchestrationRuntimeState::default();
        let runtime = state.replace(
            1,
            "sess-current".to_string(),
            "default".to_string(),
            vec![session("sess-current", "default", true)],
            None,
        );
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Active);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(runtime.session_count, 1);
        assert_eq!(runtime.current_session_id.as_deref(), Some("sess-current"));
    }

    #[test]
    fn orchestration_runtime_preserves_degraded_foundation_reason() {
        let mut state = OrchestrationRuntimeState::default();
        let runtime = state.replace(
            1,
            "sess-current".to_string(),
            "default".to_string(),
            vec![session("sess-other", "other", false)],
            Some("current_session_missing_from_registry".to_string()),
        );
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert!(!runtime.addressing_supported);
        assert!(!runtime.execution_supported);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("current_session_missing_from_registry")
        );
    }

    #[test]
    fn orchestration_runtime_tracks_registered_rules_and_trace() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");

        let runtime = state.projection();
        assert_eq!(runtime.rules.len(), 1);
        assert_eq!(runtime.group_count, 1);
        assert_eq!(runtime.groups.len(), 1);
        assert_eq!(runtime.groups[0].correlation_key, "corr-1");
        assert_eq!(runtime.groups[0].rule_ids, vec![1]);
        assert_eq!(runtime.active_rule_count, 1);
        assert_eq!(runtime.cooldown_rule_count, 0);
        assert_eq!(runtime.last_rule_id, Some(1));
        let trace = state.trace(5);
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0].kind, OrchestrationEventKind::Registered);
    }

    #[test]
    fn orchestration_runtime_records_execution_outcome_and_trace() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");
        let rule = state
            .record_outcome(
                1,
                None,
                OrchestrationResultInfo {
                    rule_id: 1,
                    status: OrchestrationRuleStatus::Fired,
                    next_status: OrchestrationRuleStatus::Fired,
                    summary: "orchestration rule 1 committed 1/1 action(s)".to_string(),
                    committed_steps: 1,
                    total_steps: 1,
                    steps: Vec::new(),
                    cooldown_until_ms: None,
                    error_code: None,
                    reason: None,
                },
            )
            .expect("outcome should record");
        assert_eq!(rule.status, OrchestrationRuleStatus::Fired);
        assert_eq!(
            rule.last_result
                .as_ref()
                .map(|result| result.summary.as_str()),
            Some("orchestration rule 1 committed 1/1 action(s)")
        );
        let runtime = state.projection();
        assert_eq!(runtime.last_rule_id, Some(1));
        assert_eq!(
            runtime
                .last_rule_result
                .as_ref()
                .map(|result| result.status),
            Some(OrchestrationRuleStatus::Fired)
        );
        let trace = state.trace(5);
        assert!(
            trace
                .events
                .iter()
                .any(|event| event.kind == OrchestrationEventKind::Fired
                    && event.committed_steps == Some(1)
                    && event.total_steps == Some(1)),
            "{trace:?}"
        );
    }

    #[test]
    fn orchestration_runtime_preserves_armed_lifecycle_for_degraded_last_result() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");

        let rule = state
            .record_outcome(
                1,
                None,
                OrchestrationResultInfo {
                    rule_id: 1,
                    status: OrchestrationRuleStatus::Degraded,
                    next_status: OrchestrationRuleStatus::Armed,
                    summary: "orchestration condition evaluation failed".to_string(),
                    committed_steps: 0,
                    total_steps: 1,
                    steps: Vec::new(),
                    cooldown_until_ms: None,
                    error_code: Some(rub_core::error::ErrorCode::BrowserCrashed),
                    reason: Some("probe_failed".to_string()),
                },
            )
            .expect("outcome should record");

        assert_eq!(rule.status, OrchestrationRuleStatus::Armed);
        let runtime = state.projection();
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert_eq!(
            runtime
                .last_rule_result
                .as_ref()
                .map(|result| result.status),
            Some(OrchestrationRuleStatus::Degraded)
        );
        let trace = state.trace(5);
        assert!(
            trace
                .events
                .iter()
                .any(|event| event.kind == OrchestrationEventKind::Degraded)
        );
    }

    #[test]
    fn orchestration_runtime_records_orphan_outcome_trace_when_rule_is_missing() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        let rule = rule("sess-source", "sess-target");
        state
            .register(rule.clone())
            .expect("orchestration rule should register");
        state.remove(rule.id).expect("rule should remove cleanly");

        let live_rule = state.record_outcome_with_fallback(
            &rule,
            None,
            OrchestrationResultInfo {
                rule_id: rule.id,
                status: OrchestrationRuleStatus::Fired,
                next_status: OrchestrationRuleStatus::Fired,
                summary: "orchestration rule 1 committed 1/1 action(s)".to_string(),
                committed_steps: 1,
                total_steps: 1,
                steps: Vec::new(),
                cooldown_until_ms: None,
                error_code: None,
                reason: None,
            },
        );

        assert!(live_rule.is_none());
        let runtime = state.projection();
        assert_eq!(runtime.last_rule_id, Some(rule.id));
        assert_eq!(
            runtime
                .last_rule_result
                .as_ref()
                .map(|result| result.status),
            Some(OrchestrationRuleStatus::Fired)
        );
        let trace = state.trace(5);
        assert!(
            trace.events.iter().any(|event| {
                event.rule_id == Some(rule.id) && event.kind == OrchestrationEventKind::Fired
            }),
            "{trace:?}"
        );
    }

    #[test]
    fn orchestration_runtime_rearms_repeat_rules_during_cooldown() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        let mut repeat_rule = rule("sess-source", "sess-target");
        repeat_rule.mode = OrchestrationMode::Repeat;
        repeat_rule.execution_policy.cooldown_ms = 5_000;
        state
            .register(repeat_rule)
            .expect("repeat orchestration rule should register");

        let cooldown_until_ms = super::current_time_ms() + 5_000;
        let rule = state
            .record_outcome(
                1,
                None,
                OrchestrationResultInfo {
                    rule_id: 1,
                    status: OrchestrationRuleStatus::Fired,
                    next_status: OrchestrationRuleStatus::Armed,
                    summary: "repeat orchestration rule 1 committed 1/1 action(s)".to_string(),
                    committed_steps: 1,
                    total_steps: 1,
                    steps: Vec::new(),
                    cooldown_until_ms: Some(cooldown_until_ms),
                    error_code: None,
                    reason: None,
                },
            )
            .expect("repeat outcome should record");
        assert_eq!(rule.status, OrchestrationRuleStatus::Armed);
        assert_eq!(
            rule.execution_policy.cooldown_until_ms,
            Some(cooldown_until_ms)
        );

        let runtime = state.projection();
        assert_eq!(runtime.active_rule_count, 0);
        assert_eq!(runtime.cooldown_rule_count, 1);
        assert_eq!(runtime.groups[0].active_rule_count, 0);
        assert_eq!(runtime.groups[0].cooldown_rule_count, 1);
        assert_eq!(
            runtime
                .last_rule_result
                .as_ref()
                .map(|result| result.next_status),
            Some(OrchestrationRuleStatus::Armed)
        );
    }

    #[test]
    fn orchestration_runtime_marks_missing_sessions_unavailable() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");
        let runtime = state.replace(
            2,
            "sess-source".to_string(),
            "source".to_string(),
            vec![session("sess-source", "source", true)],
            None,
        );
        assert_eq!(runtime.unavailable_rule_count, 1);
        assert_eq!(
            runtime.rules[0].unavailable_reason.as_deref(),
            Some("target_session_missing")
        );
        let trace = state.trace(8);
        assert!(
            trace.events.iter().any(|event| {
                event.kind == OrchestrationEventKind::Unavailable
                    && event.unavailable_reason.as_deref() == Some("target_session_missing")
            }),
            "{trace:?}"
        );
    }

    #[test]
    fn orchestration_runtime_rejects_duplicate_idempotency_key() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("first orchestration rule should register");
        let duplicate = state
            .register(rule("sess-source", "sess-target"))
            .expect_err("duplicate idempotency key should be rejected");
        assert_eq!(duplicate, 1);
        assert_eq!(state.projection().rules.len(), 1);
    }

    #[test]
    fn orchestration_runtime_groups_rules_by_correlation_key() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
                session("sess-other", "other", false),
            ],
            None,
        );
        let mut first = rule("sess-source", "sess-target");
        first.id = 3;
        first.idempotency_key = "idem-3".to_string();
        let mut second = rule("sess-source", "sess-other");
        second.id = 7;
        second.idempotency_key = "idem-7".to_string();
        second.status = OrchestrationRuleStatus::Paused;
        let mut third = rule("sess-other", "sess-target");
        third.id = 9;
        third.correlation_key = "corr-9".to_string();
        third.idempotency_key = "idem-9".to_string();
        third.unavailable_reason = Some("target_session_missing".to_string());

        state
            .register(first)
            .expect("first orchestration rule should register");
        state
            .register(second)
            .expect("second orchestration rule should register");
        state
            .register(third)
            .expect("third orchestration rule should register");

        let runtime = state.projection();
        assert_eq!(runtime.group_count, 2);
        assert_eq!(runtime.groups[0].correlation_key, "corr-1");
        assert_eq!(runtime.groups[0].rule_ids, vec![3, 7]);
        assert_eq!(runtime.groups[0].active_rule_count, 1);
        assert_eq!(runtime.groups[0].paused_rule_count, 1);
        assert_eq!(runtime.groups[0].unavailable_rule_count, 0);
        assert_eq!(runtime.groups[1].correlation_key, "corr-9");
        assert_eq!(runtime.groups[1].rule_ids, vec![9]);
        assert_eq!(runtime.groups[1].active_rule_count, 0);
        assert_eq!(runtime.groups[1].paused_rule_count, 0);
        assert_eq!(runtime.groups[1].unavailable_rule_count, 1);
    }
}
