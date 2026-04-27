use super::*;
use rub_core::model::PathReferenceState;

pub(super) fn orchestration_session_path_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "operator_path_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(super) fn build_groups(rules: &[OrchestrationRuleInfo]) -> Vec<OrchestrationGroupInfo> {
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

pub(super) fn rule_in_cooldown(rule: &OrchestrationRuleInfo, now_ms: u64) -> bool {
    rule.execution_policy
        .cooldown_until_ms
        .map(|until| until > now_ms)
        .unwrap_or(false)
}

pub(super) fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl OrchestrationRuntimeState {
    pub(super) fn refresh_counts(&mut self) {
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

    pub(super) fn refresh_status(&mut self) {
        self.projection.status = if self.projection.degraded_reason.is_some()
            || all_rules_unavailable(&self.projection)
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
}

fn all_rules_unavailable(projection: &OrchestrationRuntimeInfo) -> bool {
    !projection.rules.is_empty() && projection.unavailable_rule_count == projection.rules.len()
}
