use super::events::event_kind_for_result_status;
use super::*;

impl TriggerRuntimeState {
    pub fn replace(&mut self, triggers: Vec<TriggerInfo>) -> TriggerRuntimeInfo {
        self.projection.triggers = triggers;
        self.refresh_status();
        self.projection()
    }

    pub fn triggers(&self) -> Vec<TriggerInfo> {
        self.projection.triggers.clone()
    }

    pub fn register(&mut self, trigger: TriggerInfo) -> TriggerInfo {
        self.projection.triggers.push(trigger.clone());
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: TriggerEventKind::Registered,
            trigger_id: Some(trigger.id),
            summary: format!("trigger {} registered", trigger.id),
            unavailable_reason: None,
            evidence: None,
            result: None,
        });
        self.refresh_status();
        trigger
    }

    pub fn update_status(&mut self, id: u32, status: TriggerStatus) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.status = status;
            trigger.clone()
        };
        let kind = match status {
            TriggerStatus::Paused => Some(TriggerEventKind::Paused),
            TriggerStatus::Armed => Some(TriggerEventKind::Resumed),
            _ => None,
        };
        if let Some(kind) = kind {
            self.push_event(TriggerEventInfo {
                sequence: 0,
                kind,
                trigger_id: Some(trigger.id),
                summary: format!(
                    "trigger {} {}",
                    trigger.id,
                    match kind {
                        TriggerEventKind::Paused => "paused",
                        TriggerEventKind::Resumed => "resumed",
                        _ => "updated",
                    }
                ),
                unavailable_reason: trigger.unavailable_reason.clone(),
                evidence: trigger.last_condition_evidence.clone(),
                result: trigger.last_action_result.clone(),
            });
        }
        self.refresh_status();
        Some(trigger)
    }

    pub fn set_condition_evidence(
        &mut self,
        id: u32,
        evidence: Option<TriggerEvidenceInfo>,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.last_condition_evidence = evidence;
            if trigger.last_condition_evidence.is_none() {
                trigger.consumed_evidence_fingerprint = None;
            }
            trigger.clone()
        };
        self.refresh_status();
        Some(trigger)
    }

    pub fn record_outcome(
        &mut self,
        id: u32,
        status: TriggerStatus,
        evidence: Option<TriggerEvidenceInfo>,
        result: TriggerResultInfo,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.status = status;
            trigger.last_condition_evidence = evidence;
            trigger.consumed_evidence_fingerprint = result.consumed_evidence_fingerprint.clone();
            trigger.last_action_result = Some(result.clone());
            trigger.clone()
        };
        self.set_last_result(result);
        let event_kind = event_kind_for_result_status(
            trigger
                .last_action_result
                .as_ref()
                .map(|result| result.status)
                .unwrap_or(status),
        );
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: event_kind,
            trigger_id: Some(trigger.id),
            summary: trigger
                .last_action_result
                .as_ref()
                .map(|result| result.summary.clone())
                .unwrap_or_else(|| format!("trigger {} {:?}", trigger.id, status).to_lowercase()),
            unavailable_reason: trigger.unavailable_reason.clone(),
            evidence: trigger.last_condition_evidence.clone(),
            result: trigger.last_action_result.clone(),
        });
        self.refresh_status();
        Some(trigger)
    }

    pub fn remove(&mut self, id: u32) -> Option<TriggerInfo> {
        let index = self
            .projection
            .triggers
            .iter()
            .position(|trigger| trigger.id == id)?;
        let removed = self.projection.triggers.remove(index);
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: TriggerEventKind::Removed,
            trigger_id: Some(removed.id),
            summary: format!("trigger {} removed", removed.id),
            unavailable_reason: removed.unavailable_reason.clone(),
            evidence: removed.last_condition_evidence.clone(),
            result: removed.last_action_result.clone(),
        });
        self.refresh_status();
        Some(removed)
    }

    pub fn reconcile_tabs(&mut self, tabs: &[TabInfo]) -> TriggerRuntimeInfo {
        self.reconcile_tabs_projection(tabs);
        self.projection()
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) -> TriggerRuntimeInfo {
        self.projection.status = TriggerRuntimeStatus::Degraded;
        self.projection.degraded_reason = Some(reason.into());
        self.projection()
    }

    pub fn clear_degraded(&mut self) {
        self.projection.degraded_reason = None;
        self.refresh_status();
    }

    pub fn set_last_result(&mut self, result: TriggerResultInfo) -> TriggerRuntimeInfo {
        self.projection.last_trigger_id = Some(result.trigger_id);
        self.projection.last_trigger_result = Some(result);
        self.projection()
    }
}
