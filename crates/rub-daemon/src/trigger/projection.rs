use super::*;

pub(super) fn sync_binding(binding: &mut rub_core::model::TriggerTabBindingInfo, tab: &TabInfo) {
    binding.index = tab.index;
    if tab.page_identity_authoritative() {
        binding.url = tab.url.clone();
        binding.title = tab.title.clone();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerTabAuthority<'a> {
    Available,
    Missing,
    PageIdentityDegraded(&'a str),
}

fn trigger_tab_authority(tab: Option<&TabInfo>) -> TriggerTabAuthority<'_> {
    match tab {
        None => TriggerTabAuthority::Missing,
        Some(tab) => match tab.degraded_reason.as_deref() {
            Some(reason) => TriggerTabAuthority::PageIdentityDegraded(reason),
            None => TriggerTabAuthority::Available,
        },
    }
}

fn trigger_unavailable_reason(
    source: TriggerTabAuthority<'_>,
    target: TriggerTabAuthority<'_>,
) -> Option<String> {
    use TriggerTabAuthority::{Available, Missing, PageIdentityDegraded};

    match (source, target) {
        (Available, Available) => None,
        (Missing, Missing) => Some("source_and_target_tabs_missing".to_string()),
        (Missing, Available) => Some("source_tab_missing".to_string()),
        (Available, Missing) => Some("target_tab_missing".to_string()),
        (PageIdentityDegraded(_), Available) => Some("source_tab_projection_degraded".to_string()),
        (Available, PageIdentityDegraded(_)) => Some("target_tab_projection_degraded".to_string()),
        (PageIdentityDegraded(_), PageIdentityDegraded(_)) => {
            Some("source_and_target_tabs_projection_degraded".to_string())
        }
        (Missing, PageIdentityDegraded(_)) => {
            Some("source_tab_missing_and_target_projection_degraded".to_string())
        }
        (PageIdentityDegraded(_), Missing) => {
            Some("source_tab_projection_degraded_and_target_missing".to_string())
        }
    }
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

            trigger.unavailable_reason = trigger_unavailable_reason(
                trigger_tab_authority(source),
                trigger_tab_authority(target),
            );

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
