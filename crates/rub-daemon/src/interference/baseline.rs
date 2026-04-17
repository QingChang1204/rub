use super::*;
use rub_core::model::{
    HumanVerificationHandoffInfo, InterferenceMode, ReadinessInfo, RuntimeObservatoryInfo, TabInfo,
};

impl InterferenceRuntimeState {
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
        if self.baseline.primary_target_id.is_none()
            && self.baseline.primary_url.is_none()
            && matches!(
                self.projection.status,
                rub_core::model::InterferenceRuntimeStatus::Inactive
            )
        {
            self.prime_baseline_from_tabs(tabs);
        }
        self.projection.clone()
    }
}
