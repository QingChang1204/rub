use crate::interference_classifier::{InterferenceBaseline, classify};
use crate::interference_policy::active_policies_for_mode;
use rub_core::model::{
    HumanVerificationHandoffInfo, InterferenceMode, InterferenceRecoveryAction,
    InterferenceRecoveryResult, InterferenceRuntimeInfo, ReadinessInfo, RuntimeObservatoryInfo,
    TabInfo,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterferenceRecoveryContext {
    pub baseline: InterferenceBaseline,
    pub projection: InterferenceRuntimeInfo,
}

/// Session-scoped public-web interference runtime authority.
#[derive(Debug, Default)]
pub struct InterferenceRuntimeState {
    projection: InterferenceRuntimeInfo,
    baseline: InterferenceBaseline,
}

impl InterferenceRuntimeState {
    pub fn projection(&self) -> InterferenceRuntimeInfo {
        self.projection.clone()
    }

    pub fn replace(&mut self, projection: InterferenceRuntimeInfo) {
        self.projection = projection;
    }

    pub(crate) fn set_mode(&mut self, mode: InterferenceMode) -> InterferenceRuntimeInfo {
        self.projection.mode = mode;
        self.projection.active_policies = active_policies_for_mode(mode);
        self.projection.clone()
    }

    pub(crate) fn prime_baseline_from_tabs(&mut self, tabs: &[TabInfo]) {
        if self.baseline.primary_target_id.is_some() || self.baseline.primary_url.is_some() {
            return;
        }
        let Some(active_tab) = tabs.iter().find(|tab| tab.active) else {
            return;
        };
        self.baseline = InterferenceBaseline {
            primary_target_id: Some(active_tab.target_id.clone()),
            primary_url: Some(active_tab.url.clone()),
            last_tab_count: tabs.len(),
        };
    }

    pub(crate) fn adopt_primary_context_from_tabs(&mut self, tabs: &[TabInfo]) {
        let Some(active_tab) = tabs.iter().find(|tab| tab.active) else {
            return;
        };
        self.baseline = InterferenceBaseline {
            primary_target_id: Some(active_tab.target_id.clone()),
            primary_url: Some(active_tab.url.clone()),
            last_tab_count: tabs.len(),
        };
    }

    pub fn classify(
        &mut self,
        tabs: &[TabInfo],
        observatory: &RuntimeObservatoryInfo,
        readiness: &ReadinessInfo,
        handoff: &HumanVerificationHandoffInfo,
    ) -> InterferenceRuntimeInfo {
        let classified = classify(
            &self.projection,
            &self.baseline,
            tabs,
            observatory,
            readiness,
            handoff,
        );
        self.projection = classified.projection;
        self.baseline = classified.baseline;
        self.projection.clone()
    }

    pub(crate) fn recovery_context(&self) -> InterferenceRecoveryContext {
        InterferenceRecoveryContext {
            baseline: self.baseline.clone(),
            projection: self.projection.clone(),
        }
    }

    pub(crate) fn begin_recovery(
        &mut self,
        action: InterferenceRecoveryAction,
    ) -> InterferenceRuntimeInfo {
        self.projection.recovery_in_progress = true;
        self.projection.last_recovery_action = Some(action);
        self.projection.last_recovery_result = None;
        self.projection.degraded_reason = None;
        self.projection.clone()
    }

    pub(crate) fn finish_recovery(
        &mut self,
        result: InterferenceRecoveryResult,
    ) -> InterferenceRuntimeInfo {
        self.projection.recovery_in_progress = false;
        self.projection.last_recovery_result = Some(result);
        self.projection.clone()
    }

    pub(crate) fn record_recovery_outcome(
        &mut self,
        action: Option<InterferenceRecoveryAction>,
        result: InterferenceRecoveryResult,
    ) -> InterferenceRuntimeInfo {
        self.projection.recovery_in_progress = false;
        self.projection.last_recovery_action = action;
        self.projection.last_recovery_result = Some(result);
        self.projection.clone()
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) {
        self.projection = InterferenceRuntimeInfo {
            status: rub_core::model::InterferenceRuntimeStatus::Degraded,
            recovery_in_progress: false,
            degraded_reason: Some(reason.into()),
            ..self.projection.clone()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::InterferenceRuntimeState;
    use rub_core::model::{
        HumanVerificationHandoffInfo, InterferenceKind, InterferenceMode, InterferenceObservation,
        InterferenceRecoveryAction, InterferenceRecoveryResult, InterferenceRuntimeInfo,
        InterferenceRuntimeStatus, ReadinessInfo, RuntimeObservatoryInfo, TabInfo,
    };

    #[test]
    fn interference_runtime_state_tracks_projection() {
        let mut state = InterferenceRuntimeState::default();
        let projection = InterferenceRuntimeInfo {
            mode: InterferenceMode::PublicWebStable,
            status: InterferenceRuntimeStatus::Active,
            current_interference: Some(InterferenceObservation {
                kind: InterferenceKind::UnknownNavigationDrift,
                summary: "unexpected top-level navigation drift".to_string(),
                current_url: Some("https://example.test/interstitial".to_string()),
                primary_url: Some("https://example.test/app".to_string()),
            }),
            active_policies: vec!["observe_only".to_string()],
            ..InterferenceRuntimeInfo::default()
        };

        state.replace(projection.clone());

        assert_eq!(state.projection(), projection);
    }

    #[test]
    fn interference_runtime_state_can_mark_degraded() {
        let mut state = InterferenceRuntimeState::default();
        state.mark_degraded("classifier_unavailable");

        let projection = state.projection();
        assert_eq!(projection.status, InterferenceRuntimeStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("classifier_unavailable")
        );
    }

    #[test]
    fn interference_runtime_state_classifies_and_updates_baseline() {
        let mut state = InterferenceRuntimeState::default();
        let projection = state.classify(
            &[TabInfo {
                index: 0,
                target_id: "target-1".to_string(),
                url: "https://app.example.com/home".to_string(),
                title: "Home".to_string(),
                active: true,
            }],
            &RuntimeObservatoryInfo::default(),
            &ReadinessInfo::default(),
            &HumanVerificationHandoffInfo::default(),
        );

        assert_eq!(projection.status, InterferenceRuntimeStatus::Inactive);
        assert_eq!(
            state.baseline.primary_url.as_deref(),
            Some("https://app.example.com/home")
        );
    }

    #[test]
    fn interference_runtime_state_primes_missing_baseline_from_active_tab() {
        let mut state = InterferenceRuntimeState::default();
        state.prime_baseline_from_tabs(&[TabInfo {
            index: 0,
            target_id: "target-1".to_string(),
            url: "https://app.example.com/home".to_string(),
            title: "Home".to_string(),
            active: true,
        }]);

        assert_eq!(
            state.baseline.primary_target_id.as_deref(),
            Some("target-1")
        );
        assert_eq!(
            state.baseline.primary_url.as_deref(),
            Some("https://app.example.com/home")
        );
    }

    #[test]
    fn interference_runtime_state_does_not_overwrite_existing_baseline() {
        let mut state = InterferenceRuntimeState::default();
        state.prime_baseline_from_tabs(&[TabInfo {
            index: 0,
            target_id: "target-1".to_string(),
            url: "https://app.example.com/home".to_string(),
            title: "Home".to_string(),
            active: true,
        }]);
        state.prime_baseline_from_tabs(&[TabInfo {
            index: 0,
            target_id: "target-2".to_string(),
            url: "https://app.example.com/other".to_string(),
            title: "Other".to_string(),
            active: true,
        }]);

        assert_eq!(
            state.baseline.primary_target_id.as_deref(),
            Some("target-1")
        );
        assert_eq!(
            state.baseline.primary_url.as_deref(),
            Some("https://app.example.com/home")
        );
    }

    #[test]
    fn interference_runtime_state_can_adopt_new_primary_context() {
        let mut state = InterferenceRuntimeState::default();
        state.prime_baseline_from_tabs(&[TabInfo {
            index: 0,
            target_id: "target-1".to_string(),
            url: "https://app.example.com/home".to_string(),
            title: "Home".to_string(),
            active: true,
        }]);
        state.adopt_primary_context_from_tabs(&[TabInfo {
            index: 0,
            target_id: "target-2".to_string(),
            url: "https://example.org/dashboard".to_string(),
            title: "Dashboard".to_string(),
            active: true,
        }]);

        assert_eq!(
            state.baseline.primary_target_id.as_deref(),
            Some("target-2")
        );
        assert_eq!(
            state.baseline.primary_url.as_deref(),
            Some("https://example.org/dashboard")
        );
    }

    #[test]
    fn interference_runtime_state_tracks_recovery_lifecycle() {
        let mut state = InterferenceRuntimeState::default();

        state.begin_recovery(InterferenceRecoveryAction::BackNavigate);
        let in_progress = state.projection();
        assert!(in_progress.recovery_in_progress);
        assert_eq!(
            in_progress.last_recovery_action,
            Some(InterferenceRecoveryAction::BackNavigate)
        );
        assert_eq!(in_progress.last_recovery_result, None);

        state.finish_recovery(InterferenceRecoveryResult::Succeeded);
        let finished = state.projection();
        assert!(!finished.recovery_in_progress);
        assert_eq!(
            finished.last_recovery_result,
            Some(InterferenceRecoveryResult::Succeeded)
        );
    }

    #[test]
    fn interference_runtime_state_can_record_abandoned_outcome_without_action() {
        let mut state = InterferenceRuntimeState::default();
        state.record_recovery_outcome(None, InterferenceRecoveryResult::Abandoned);

        let projection = state.projection();
        assert_eq!(projection.last_recovery_action, None);
        assert_eq!(
            projection.last_recovery_result,
            Some(InterferenceRecoveryResult::Abandoned)
        );
        assert!(!projection.recovery_in_progress);
    }

    #[test]
    fn interference_runtime_state_sets_mode_and_canonical_policies() {
        let mut state = InterferenceRuntimeState::default();
        let projection = state.set_mode(InterferenceMode::Strict);

        assert_eq!(projection.mode, InterferenceMode::Strict);
        assert_eq!(
            projection.active_policies,
            vec!["safe_recovery", "handoff_escalation", "strict_containment"]
        );
    }
}
