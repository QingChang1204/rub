use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use rub_core::model::{
    OrchestrationEventInfo, OrchestrationEventKind, OrchestrationGroupInfo,
    OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
    OrchestrationRuntimeInfo, OrchestrationRuntimeStatus, OrchestrationTraceProjection,
    TriggerEvidenceInfo,
};

use crate::session::NetworkRequestBaseline;

mod events;
mod mutation;
mod projection;
mod sessions;

use events::orchestration_outcome_event_kind;
pub(crate) use sessions::{
    extend_orchestration_session_path_context, orchestration_session_addressability_reason,
    orchestration_session_not_addressable_error, projected_orchestration_session,
};

const ORCHESTRATION_EVENT_LIMIT: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestrationOutcomeCommit {
    Applied(Option<OrchestrationRuleInfo>),
    Stale(Option<OrchestrationRuleInfo>),
}

/// Session-local orchestration authority built on top of the RUB_HOME registry foundation.
#[derive(Debug, Default)]
pub struct OrchestrationRuntimeState {
    projection: OrchestrationRuntimeInfo,
    network_request_baselines: HashMap<u32, NetworkRequestBaseline>,
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

    pub(crate) fn network_request_baselines(&self) -> HashMap<u32, NetworkRequestBaseline> {
        self.network_request_baselines.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::projection::current_time_ms;
    use super::{
        OrchestrationOutcomeCommit, OrchestrationRuntimeState,
        extend_orchestration_session_path_context, projected_orchestration_session,
    };
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationEventKind, OrchestrationExecutionPolicyInfo,
        OrchestrationMode, OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
        OrchestrationRuntimeStatus, OrchestrationSessionAvailability, OrchestrationSessionInfo,
        TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
    };

    fn session(id: &str, name: &str, current: bool) -> OrchestrationSessionInfo {
        projected_orchestration_session(
            id.to_string(),
            name.to_string(),
            1234,
            format!("/tmp/{name}.sock"),
            current,
            "1.0".to_string(),
            OrchestrationSessionAvailability::Addressable,
            None,
        )
    }

    fn applied_rule(commit: OrchestrationOutcomeCommit) -> Option<OrchestrationRuleInfo> {
        match commit {
            OrchestrationOutcomeCommit::Applied(rule) => rule,
            OrchestrationOutcomeCommit::Stale(rule) => {
                panic!("expected applied orchestration outcome, got stale: {rule:?}")
            }
        }
    }

    fn rule(source_session_id: &str, target_session_id: &str) -> OrchestrationRuleInfo {
        OrchestrationRuleInfo {
            id: 1,
            status: OrchestrationRuleStatus::Armed,
            lifecycle_generation: 1,
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
            true,
            true,
            None,
        );
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Active);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(runtime.session_count, 1);
        assert_eq!(runtime.current_session_id.as_deref(), Some("sess-current"));
        assert_eq!(
            runtime.known_sessions[0]
                .socket_path_state
                .as_ref()
                .map(|state| state.path_kind.as_str()),
            Some("session_socket_reference")
        );
    }

    #[test]
    fn projected_orchestration_session_marks_registry_backed_path_references() {
        let session = projected_orchestration_session(
            "sess-current".to_string(),
            "default".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            true,
            "1.0".to_string(),
            OrchestrationSessionAvailability::Addressable,
            Some("/tmp/rub-profile".to_string()),
        );

        assert_eq!(
            session
                .socket_path_state
                .as_ref()
                .map(|state| state.truth_level.as_str()),
            Some("operator_path_reference")
        );
        assert_eq!(
            session
                .socket_path_state
                .as_ref()
                .map(|state| state.path_authority.as_str()),
            Some("session.orchestration_runtime.known_sessions.socket_path")
        );
        assert_eq!(
            session
                .user_data_dir_state
                .as_ref()
                .map(|state| state.path_kind.as_str()),
            Some("managed_user_data_directory")
        );
        assert_eq!(
            session
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("registry_authority_snapshot")
        );
    }

    #[test]
    fn projected_orchestration_session_marks_current_fallback_path_references_from_runtime_authority()
     {
        let session = projected_orchestration_session(
            "sess-current".to_string(),
            "default".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            true,
            "1.0".to_string(),
            OrchestrationSessionAvailability::CurrentFallback,
            Some("/tmp/rub-profile".to_string()),
        );

        assert_eq!(
            session
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("current_session_runtime_authority")
        );
        assert_eq!(
            session
                .user_data_dir_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("current_session_runtime_authority")
        );
    }

    #[test]
    fn extend_orchestration_session_path_context_projects_transport_references() {
        let session = projected_orchestration_session(
            "sess-current".to_string(),
            "default".to_string(),
            42,
            "/tmp/rub.sock".to_string(),
            true,
            "1.0".to_string(),
            OrchestrationSessionAvailability::Addressable,
            Some("/tmp/rub-profile".to_string()),
        );
        let mut context = serde_json::json!({
            "reason": "orchestration_target_session_unreachable",
        });

        extend_orchestration_session_path_context(&mut context, &session);

        assert_eq!(context["socket_path"], "/tmp/rub.sock");
        assert_eq!(
            context["socket_path_state"]["path_kind"],
            "session_socket_reference"
        );
        assert_eq!(context["user_data_dir"], "/tmp/rub-profile");
        assert_eq!(
            context["user_data_dir_state"]["path_kind"],
            "managed_user_data_directory"
        );
    }

    #[test]
    fn orchestration_runtime_preserves_degraded_foundation_reason() {
        let mut state = OrchestrationRuntimeState::default();
        let runtime = state.replace(
            1,
            "sess-current".to_string(),
            "default".to_string(),
            vec![session("sess-other", "other", false)],
            false,
            false,
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
            true,
            true,
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
            true,
            true,
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");
        let rule = state.record_outcome(
            1,
            Some(1),
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
                error_context: None,
            },
        );
        let rule = applied_rule(rule).expect("outcome should record");
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
            true,
            true,
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");

        let rule = state.record_outcome(
            1,
            Some(1),
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
                error_context: None,
            },
        );
        let rule = applied_rule(rule).expect("outcome should record");

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
    fn orchestration_runtime_preserves_structured_error_context_in_result_and_trace() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");

        let rule = state.record_outcome(
            1,
            Some(1),
            None,
            OrchestrationResultInfo {
                rule_id: 1,
                status: OrchestrationRuleStatus::Degraded,
                next_status: OrchestrationRuleStatus::Armed,
                summary: "orchestration continuity failed".to_string(),
                committed_steps: 0,
                total_steps: 1,
                steps: Vec::new(),
                cooldown_until_ms: None,
                error_code: Some(rub_core::error::ErrorCode::SessionBusy),
                reason: Some("continuity_runtime_degraded".to_string()),
                error_context: Some(serde_json::json!({
                    "reason": "continuity_runtime_degraded",
                    "target_tab_target_id": "tab-target",
                    "phase": "target_continuity",
                })),
            },
        );
        let rule = applied_rule(rule).expect("outcome should record");

        assert_eq!(
            rule.last_result
                .as_ref()
                .and_then(|result| result.error_context.as_ref())
                .and_then(|context| context.get("phase"))
                .and_then(|value| value.as_str()),
            Some("target_continuity")
        );

        let trace = state.trace(5);
        let event = trace
            .events
            .iter()
            .find(|event| event.kind == OrchestrationEventKind::Degraded)
            .expect("degraded trace event");
        assert_eq!(
            event
                .error_context
                .as_ref()
                .and_then(|context| context.get("target_tab_target_id"))
                .and_then(|value| value.as_str()),
            Some("tab-target")
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
            true,
            true,
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
                error_context: None,
            },
        );

        assert!(matches!(
            live_rule,
            OrchestrationOutcomeCommit::Applied(None)
        ));
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
    fn orchestration_runtime_preserves_newer_lifecycle_when_outcome_generation_is_stale() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");
        let paused = state
            .update_status(1, OrchestrationRuleStatus::Paused)
            .expect("pause should update rule");
        assert_eq!(paused.lifecycle_generation, 2);

        let outcome = state.record_outcome(
            1,
            Some(1),
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
                error_context: None,
            },
        );

        assert!(matches!(
            outcome,
            OrchestrationOutcomeCommit::Stale(Some(ref rule))
                if rule.status == OrchestrationRuleStatus::Paused
        ));
        let runtime = state.projection();
        assert_eq!(runtime.rules[0].status, OrchestrationRuleStatus::Paused);
        assert_eq!(runtime.rules[0].lifecycle_generation, 2);
        assert_eq!(
            runtime.rules[0]
                .last_result
                .as_ref()
                .map(|result| result.summary.as_str()),
            Some("orchestration rule 1 committed 1/1 action(s)")
        );
        assert_eq!(
            runtime
                .last_rule_result
                .as_ref()
                .map(|result| result.summary.as_str()),
            Some("orchestration rule 1 committed 1/1 action(s)")
        );
        let trace = state.trace(8);
        assert!(
            trace.events.iter().any(|event| {
                event.kind == OrchestrationEventKind::Degraded
                    && event.reason.as_deref() == Some("orchestration_lifecycle_generation_stale")
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
            true,
            true,
            None,
        );
        let mut repeat_rule = rule("sess-source", "sess-target");
        repeat_rule.mode = OrchestrationMode::Repeat;
        repeat_rule.execution_policy.cooldown_ms = 5_000;
        state
            .register(repeat_rule)
            .expect("repeat orchestration rule should register");

        let cooldown_until_ms = current_time_ms() + 5_000;
        let rule = state.record_outcome(
            1,
            Some(1),
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
                error_context: None,
            },
        );
        let rule = applied_rule(rule).expect("repeat outcome should record");
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
            true,
            true,
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
            true,
            true,
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
    fn orchestration_runtime_marks_non_addressable_sessions_unavailable() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                projected_orchestration_session(
                    "sess-target".to_string(),
                    "target".to_string(),
                    42,
                    "/tmp/rub-target.sock".to_string(),
                    false,
                    "1.0".to_string(),
                    OrchestrationSessionAvailability::ProtocolIncompatible,
                    None,
                ),
            ],
            true,
            true,
            Some("registry_contains_non_addressable_sessions".to_string()),
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");
        let runtime = state.replace(
            2,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                projected_orchestration_session(
                    "sess-target".to_string(),
                    "target".to_string(),
                    42,
                    "/tmp/rub-target.sock".to_string(),
                    false,
                    "1.0".to_string(),
                    OrchestrationSessionAvailability::ProtocolIncompatible,
                    None,
                ),
            ],
            true,
            true,
            Some("registry_contains_non_addressable_sessions".to_string()),
        );
        assert_eq!(runtime.unavailable_rule_count, 1);
        assert_eq!(
            runtime.rules[0].unavailable_reason.as_deref(),
            Some("target_session_not_addressable")
        );
        let trace = state.trace(8);
        assert!(
            trace.events.iter().any(|event| {
                event.kind == OrchestrationEventKind::Unavailable
                    && event.unavailable_reason.as_deref() == Some("target_session_not_addressable")
            }),
            "{trace:?}"
        );
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
    }

    #[test]
    fn orchestration_runtime_degrades_when_all_rules_are_unavailable() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        let mut first = rule("sess-source", "sess-target");
        first.id = 3;
        first.idempotency_key = "idem-3".to_string();
        first.unavailable_reason = Some("target_session_missing".to_string());
        let mut second = rule("sess-source", "sess-target");
        second.id = 7;
        second.idempotency_key = "idem-7".to_string();
        second.unavailable_reason = Some("target_session_not_addressable".to_string());

        state
            .register(first)
            .expect("first orchestration rule should register");
        state
            .register(second)
            .expect("second orchestration rule should register");

        let runtime = state.projection();
        assert_eq!(runtime.unavailable_rule_count, 2);
        assert_eq!(runtime.active_rule_count, 0);
        assert_eq!(runtime.cooldown_rule_count, 0);
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
    }

    #[test]
    fn orchestration_runtime_rejects_stale_condition_evidence_generation_after_pause() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        state
            .register(rule("sess-source", "sess-target"))
            .expect("orchestration rule should register");
        let paused = state
            .update_status(1, OrchestrationRuleStatus::Paused)
            .expect("pause should update rule");
        assert_eq!(paused.lifecycle_generation, 2);

        let stale = state.set_condition_evidence(
            1,
            Some(1),
            Some(rub_core::model::TriggerEvidenceInfo {
                summary: "text_present:Ready".to_string(),
                fingerprint: Some("Ready".to_string()),
            }),
        );

        assert!(stale.is_none());
        let runtime = state.projection();
        assert_eq!(runtime.rules[0].status, OrchestrationRuleStatus::Paused);
        assert_eq!(runtime.rules[0].lifecycle_generation, 2);
        assert!(runtime.rules[0].last_condition_evidence.is_none());
    }

    #[test]
    fn orchestration_runtime_rejects_removed_rule_probe_failure_with_expected_generation() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        let rule = rule("sess-source", "sess-target");
        state
            .register(rule.clone())
            .expect("orchestration rule should register");
        state.remove(rule.id).expect("rule should remove cleanly");

        let stale = state.record_outcome_with_fallback(
            &rule,
            Some(rule.lifecycle_generation),
            None,
            OrchestrationResultInfo {
                rule_id: rule.id,
                status: OrchestrationRuleStatus::Degraded,
                next_status: OrchestrationRuleStatus::Armed,
                summary: "orchestration condition evaluation failed".to_string(),
                committed_steps: 0,
                total_steps: rule.actions.len() as u32,
                steps: Vec::new(),
                cooldown_until_ms: None,
                error_code: Some(rub_core::error::ErrorCode::BrowserCrashed),
                reason: Some("probe_failed".to_string()),
                error_context: None,
            },
        );

        assert!(matches!(stale, OrchestrationOutcomeCommit::Stale(None)));
        let runtime = state.projection();
        assert_eq!(runtime.last_rule_id, Some(rule.id));
        assert_eq!(
            runtime
                .last_rule_result
                .as_ref()
                .and_then(|result| result.reason.as_deref()),
            Some("probe_failed")
        );
        let trace = state.trace(8);
        assert!(
            trace.events.iter().any(|event| {
                event.kind == OrchestrationEventKind::Degraded
                    && event.reason.as_deref() == Some("orchestration_lifecycle_generation_stale")
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
            true,
            true,
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
            true,
            true,
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

    #[test]
    fn orchestration_runtime_commits_network_request_baseline_on_register_and_resume() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        let mut network_rule = rule("sess-source", "sess-target");
        network_rule.condition.kind = TriggerConditionKind::NetworkRequest;

        state
            .register_with_network_baseline(
                network_rule.clone(),
                Some(crate::session::NetworkRequestBaseline {
                    cursor: 5,
                    observed_ingress_drop_count: 1,
                    primed: true,
                }),
            )
            .expect("rule should register");
        assert_eq!(
            state
                .network_request_baselines()
                .get(&1)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((5, 1))
        );

        state
            .update_status(1, OrchestrationRuleStatus::Paused)
            .expect("pause should update rule");
        assert!(
            !state.network_request_baselines().contains_key(&1),
            "paused network rule must not keep an armed baseline"
        );

        state
            .update_status_with_network_baseline(
                1,
                OrchestrationRuleStatus::Armed,
                Some(crate::session::NetworkRequestBaseline {
                    cursor: 9,
                    observed_ingress_drop_count: 3,
                    primed: true,
                }),
            )
            .expect("resume should update rule");
        assert_eq!(
            state
                .network_request_baselines()
                .get(&1)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((9, 3))
        );
    }

    #[test]
    fn orchestration_runtime_preserves_existing_armed_network_request_baseline() {
        let mut state = OrchestrationRuntimeState::default();
        state.replace(
            1,
            "sess-source".to_string(),
            "source".to_string(),
            vec![
                session("sess-source", "source", true),
                session("sess-target", "target", false),
            ],
            true,
            true,
            None,
        );
        let mut network_rule = rule("sess-source", "sess-target");
        network_rule.condition.kind = TriggerConditionKind::NetworkRequest;

        state
            .register_with_network_baseline(
                network_rule,
                Some(crate::session::NetworkRequestBaseline {
                    cursor: 21,
                    observed_ingress_drop_count: 8,
                    primed: true,
                }),
            )
            .expect("rule should register");
        state
            .update_status(1, OrchestrationRuleStatus::Armed)
            .expect("generic armed update should preserve baseline");
        assert_eq!(
            state
                .network_request_baselines()
                .get(&1)
                .copied()
                .map(|baseline| (baseline.cursor, baseline.observed_ingress_drop_count)),
            Some((21, 8))
        );
    }
}
