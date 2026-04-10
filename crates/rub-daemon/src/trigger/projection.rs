use super::*;

pub(super) fn sync_binding(binding: &mut rub_core::model::TriggerTabBindingInfo, tab: &TabInfo) {
    binding.index = tab.index;
    binding.url = tab.url.clone();
    binding.title = tab.title.clone();
}

impl TriggerRuntimeState {
    pub(super) fn reconcile_tabs_projection(&mut self, tabs: &[TabInfo]) {
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
    }

    pub(super) fn refresh_status(&mut self) {
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
