use super::events::event_kind_for_result_status;
use super::*;
use crate::session::NetworkRequestBaseline;
use rub_core::model::TriggerConditionKind;

impl TriggerRuntimeState {
    pub fn replace(&mut self, triggers: Vec<TriggerInfo>) -> TriggerRuntimeInfo {
        self.projection.triggers = triggers
            .into_iter()
            .map(|mut trigger| {
                trigger.lifecycle_generation = trigger.lifecycle_generation.max(1);
                trigger
            })
            .collect();
        self.retain_network_request_baselines();
        self.refresh_status();
        self.projection()
    }

    pub fn triggers(&self) -> Vec<TriggerInfo> {
        self.projection.triggers.clone()
    }

    pub fn register(&mut self, trigger: TriggerInfo) -> TriggerInfo {
        self.register_with_network_baseline(trigger, None)
    }

    pub(crate) fn register_with_network_baseline(
        &mut self,
        trigger: TriggerInfo,
        network_baseline: Option<NetworkRequestBaseline>,
    ) -> TriggerInfo {
        let mut trigger = trigger;
        trigger.lifecycle_generation = trigger.lifecycle_generation.max(1);
        self.projection.triggers.push(trigger.clone());
        self.commit_network_request_baseline(&trigger, network_baseline);
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
        let preserved_network_baseline = self.network_request_baselines.get(&id).copied();
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.status = status;
            trigger.lifecycle_generation = trigger.lifecycle_generation.saturating_add(1);
            trigger.clone()
        };
        self.commit_network_request_baseline(&trigger, preserved_network_baseline);
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

    pub(crate) fn update_status_with_network_baseline(
        &mut self,
        id: u32,
        status: TriggerStatus,
        network_baseline: Option<NetworkRequestBaseline>,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            trigger.status = status;
            trigger.lifecycle_generation = trigger.lifecycle_generation.saturating_add(1);
            trigger.clone()
        };
        self.commit_network_request_baseline(&trigger, network_baseline);
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

    pub(crate) fn ensure_network_request_baseline(
        &mut self,
        id: u32,
        network_baseline: NetworkRequestBaseline,
    ) -> Option<TriggerInfo> {
        let trigger = self
            .projection
            .triggers
            .iter()
            .find(|trigger| trigger.id == id)?
            .clone();
        if matches!(trigger.status, TriggerStatus::Armed)
            && matches!(trigger.condition.kind, TriggerConditionKind::NetworkRequest)
        {
            self.network_request_baselines
                .entry(trigger.id)
                .or_insert(network_baseline);
        }
        Some(trigger)
    }

    pub fn set_condition_evidence(
        &mut self,
        id: u32,
        expected_generation: u64,
        evidence: Option<TriggerEvidenceInfo>,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            if trigger.lifecycle_generation != expected_generation {
                return None;
            }
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
        expected_generation: u64,
        evidence: Option<TriggerEvidenceInfo>,
        result: TriggerResultInfo,
    ) -> Option<TriggerInfo> {
        let trigger = {
            let trigger = self
                .projection
                .triggers
                .iter_mut()
                .find(|trigger| trigger.id == id)?;
            if trigger.lifecycle_generation != expected_generation {
                return None;
            }
            trigger.status = result.next_status;
            trigger.lifecycle_generation = trigger.lifecycle_generation.saturating_add(1);
            trigger.last_condition_evidence = evidence;
            trigger.consumed_evidence_fingerprint = result.consumed_evidence_fingerprint.clone();
            trigger.last_action_result = Some(result.clone());
            trigger.clone()
        };
        if !matches!(trigger.status, TriggerStatus::Armed)
            || !matches!(trigger.condition.kind, TriggerConditionKind::NetworkRequest)
        {
            self.network_request_baselines.remove(&trigger.id);
        }
        self.set_last_result(result);
        let event_kind = event_kind_for_result_status(
            trigger
                .last_action_result
                .as_ref()
                .map(|result| result.status)
                .unwrap_or(trigger.status),
        );
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: event_kind,
            trigger_id: Some(trigger.id),
            summary: trigger
                .last_action_result
                .as_ref()
                .map(|result| result.summary.clone())
                .unwrap_or_else(|| {
                    format!("trigger {} {:?}", trigger.id, trigger.status).to_lowercase()
                }),
            unavailable_reason: trigger.unavailable_reason.clone(),
            evidence: trigger.last_condition_evidence.clone(),
            result: trigger.last_action_result.clone(),
        });
        self.refresh_status();
        Some(trigger)
    }

    pub fn record_outcome_with_fallback(
        &mut self,
        trigger_snapshot: &TriggerInfo,
        expected_generation: u64,
        evidence: Option<TriggerEvidenceInfo>,
        result: TriggerResultInfo,
    ) -> TriggerOutcomeCommit {
        let Some(index) = self
            .projection
            .triggers
            .iter()
            .position(|trigger| trigger.id == trigger_snapshot.id)
        else {
            self.set_last_result(result.clone());
            self.push_outcome_event(trigger_snapshot, evidence, &result);
            self.refresh_status();
            return TriggerOutcomeCommit::Applied(None);
        };

        let current = self.projection.triggers[index].clone();
        if current.lifecycle_generation != expected_generation {
            let trigger = {
                let trigger = &mut self.projection.triggers[index];
                trigger.last_condition_evidence = evidence.clone();
                trigger.last_action_result = Some(result.clone());
                trigger.clone()
            };
            self.set_last_result(result.clone());
            self.push_stale_outcome_event(&trigger, evidence, &result);
            self.refresh_status();
            return TriggerOutcomeCommit::Stale(Some(trigger));
        }

        TriggerOutcomeCommit::Applied(self.record_outcome(
            trigger_snapshot.id,
            expected_generation,
            evidence,
            result,
        ))
    }

    pub fn remove(&mut self, id: u32) -> Option<TriggerInfo> {
        let index = self
            .projection
            .triggers
            .iter()
            .position(|trigger| trigger.id == id)?;
        let removed = self.projection.triggers.remove(index);
        self.network_request_baselines.remove(&removed.id);
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

    fn commit_network_request_baseline(
        &mut self,
        trigger: &TriggerInfo,
        network_baseline: Option<NetworkRequestBaseline>,
    ) {
        if matches!(trigger.status, TriggerStatus::Armed)
            && matches!(trigger.condition.kind, TriggerConditionKind::NetworkRequest)
        {
            if let Some(network_baseline) = network_baseline {
                self.network_request_baselines
                    .insert(trigger.id, network_baseline);
            }
            return;
        }
        self.network_request_baselines.remove(&trigger.id);
    }

    fn retain_network_request_baselines(&mut self) {
        self.network_request_baselines.retain(|id, _| {
            self.projection.triggers.iter().any(|trigger| {
                trigger.id == *id
                    && matches!(trigger.status, TriggerStatus::Armed)
                    && matches!(trigger.condition.kind, TriggerConditionKind::NetworkRequest)
            })
        });
    }

    fn push_outcome_event(
        &mut self,
        trigger: &TriggerInfo,
        evidence: Option<TriggerEvidenceInfo>,
        result: &TriggerResultInfo,
    ) {
        let event_kind = event_kind_for_result_status(result.status);
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: event_kind,
            trigger_id: Some(trigger.id),
            summary: result.summary.clone(),
            unavailable_reason: trigger.unavailable_reason.clone(),
            evidence,
            result: Some(result.clone()),
        });
    }

    fn push_stale_outcome_event(
        &mut self,
        trigger: &TriggerInfo,
        evidence: Option<TriggerEvidenceInfo>,
        result: &TriggerResultInfo,
    ) {
        let mut stale_result = result.clone();
        stale_result.reason = Some("trigger_lifecycle_generation_stale".to_string());
        self.push_event(TriggerEventInfo {
            sequence: 0,
            kind: TriggerEventKind::Degraded,
            trigger_id: Some(trigger.id),
            summary: format!(
                "trigger {} preserved committed outcome '{}' after newer trigger authority won",
                trigger.id, result.summary
            ),
            unavailable_reason: trigger.unavailable_reason.clone(),
            evidence,
            result: Some(stale_result),
        });
    }
}
