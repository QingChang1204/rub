use super::*;
use crate::orchestration_runtime::OrchestrationOutcomeCommit;

impl SessionState {
    /// Current session-scoped trigger runtime projection.
    pub async fn trigger_runtime(&self) -> rub_core::model::TriggerRuntimeInfo {
        self.trigger_runtime.read().await.projection()
    }

    /// Current read-only orchestration foundation projection.
    pub async fn orchestration_runtime(&self) -> rub_core::model::OrchestrationRuntimeInfo {
        self.orchestration_runtime.read().await.projection()
    }

    pub fn allocate_orchestration_runtime_sequence(&self) -> u64 {
        self.next_orchestration_runtime_sequence
            .fetch_add(1, Ordering::SeqCst)
    }

    /// Replace the current orchestration foundation projection from the RUB_HOME registry authority.
    pub async fn set_orchestration_runtime(
        &self,
        sequence: u64,
        known_sessions: Vec<rub_core::model::OrchestrationSessionInfo>,
        degraded_reason: Option<String>,
    ) -> rub_core::model::OrchestrationRuntimeInfo {
        self.orchestration_runtime.write().await.replace(
            sequence,
            self.session_id.clone(),
            self.session_name.clone(),
            known_sessions,
            degraded_reason,
        )
    }

    /// Mark the orchestration foundation surface degraded when the registry cannot be projected truthfully.
    pub async fn mark_orchestration_runtime_degraded(
        &self,
        sequence: u64,
        current_session: rub_core::model::OrchestrationSessionInfo,
        reason: impl Into<String>,
    ) -> rub_core::model::OrchestrationRuntimeInfo {
        self.orchestration_runtime
            .write()
            .await
            .mark_degraded(sequence, current_session, reason)
    }

    pub async fn orchestration_trace(&self, last: usize) -> OrchestrationTraceProjection {
        self.orchestration_runtime.read().await.trace(last)
    }

    pub async fn orchestration_rules(&self) -> Vec<OrchestrationRuleInfo> {
        self.orchestration_runtime.read().await.rules()
    }

    pub async fn orchestration_rule(&self, id: u32) -> Option<OrchestrationRuleInfo> {
        self.orchestration_runtime
            .read()
            .await
            .rules()
            .into_iter()
            .find(|rule| rule.id == id)
    }

    pub async fn register_orchestration_rule(
        &self,
        mut rule: OrchestrationRuleInfo,
    ) -> Result<OrchestrationRuleInfo, u32> {
        rule.id = self.next_orchestration_id.fetch_add(1, Ordering::SeqCst);
        self.orchestration_runtime.write().await.register(rule)
    }

    pub async fn set_orchestration_rule_status(
        &self,
        id: u32,
        status: OrchestrationRuleStatus,
    ) -> Option<OrchestrationRuleInfo> {
        self.orchestration_runtime
            .write()
            .await
            .update_status(id, status)
    }

    pub async fn remove_orchestration_rule(&self, id: u32) -> Option<OrchestrationRuleInfo> {
        self.orchestration_runtime.write().await.remove(id)
    }

    pub async fn record_orchestration_outcome(
        &self,
        id: u32,
        expected_generation: Option<u64>,
        evidence: Option<rub_core::model::TriggerEvidenceInfo>,
        result: OrchestrationResultInfo,
    ) -> OrchestrationOutcomeCommit {
        self.orchestration_runtime.write().await.record_outcome(
            id,
            expected_generation,
            evidence,
            result,
        )
    }

    pub async fn set_orchestration_condition_evidence(
        &self,
        id: u32,
        evidence: Option<rub_core::model::TriggerEvidenceInfo>,
    ) -> Option<rub_core::model::OrchestrationRuleInfo> {
        self.orchestration_runtime
            .write()
            .await
            .set_condition_evidence(id, evidence)
    }

    pub async fn record_orchestration_outcome_with_fallback(
        &self,
        rule_snapshot: &rub_core::model::OrchestrationRuleInfo,
        expected_generation: Option<u64>,
        evidence: Option<rub_core::model::TriggerEvidenceInfo>,
        result: OrchestrationResultInfo,
    ) -> OrchestrationOutcomeCommit {
        self.orchestration_runtime
            .write()
            .await
            .record_outcome_with_fallback(rule_snapshot, expected_generation, evidence, result)
    }

    /// Whether the session currently owns any armed orchestration rules
    /// (including cooling-down rules) that should keep long-lived reactive
    /// orchestration alive.
    pub async fn has_active_orchestrations(&self) -> bool {
        self.resident_orchestration_count().await > 0
    }

    pub async fn active_orchestration_count(&self) -> u32 {
        u32::try_from(
            self.orchestration_runtime
                .read()
                .await
                .projection()
                .active_rule_count,
        )
        .unwrap_or(u32::MAX)
    }

    pub async fn resident_orchestration_count(&self) -> u32 {
        let projection = self.orchestration_runtime.read().await.projection();
        let resident = projection
            .active_rule_count
            .saturating_add(projection.cooldown_rule_count);
        u32::try_from(resident).unwrap_or(u32::MAX)
    }

    /// Whether the session currently owns any armed + available triggers that
    /// should keep long-lived trigger automation active.
    pub async fn has_active_triggers(&self) -> bool {
        self.active_trigger_count().await > 0
    }

    pub async fn active_trigger_count(&self) -> u32 {
        u32::try_from(self.trigger_runtime.read().await.projection().active_count)
            .unwrap_or(u32::MAX)
    }

    /// Replace the current session-scoped trigger registry projection.
    pub async fn set_trigger_runtime(&self, runtime: rub_core::model::TriggerRuntimeInfo) {
        self.trigger_runtime.write().await.replace(runtime.triggers);
    }

    /// Mark the trigger runtime surface as degraded when the evaluator cannot
    /// run reliably.
    pub async fn mark_trigger_runtime_degraded(&self, reason: impl Into<String>) {
        self.trigger_runtime.write().await.mark_degraded(reason);
    }

    /// Clear any trigger-runtime degradation override so the registry
    /// projection can publish its live state again.
    pub async fn clear_trigger_runtime_degraded(&self) {
        self.trigger_runtime.write().await.clear_degraded();
    }

    /// Register a new session-scoped trigger rule.
    pub async fn register_trigger(
        &self,
        mut trigger: rub_core::model::TriggerInfo,
    ) -> rub_core::model::TriggerInfo {
        trigger.id = self.next_trigger_id.fetch_add(1, Ordering::SeqCst);
        self.trigger_runtime.write().await.register(trigger.clone());
        trigger
    }

    /// List configured trigger rules in stable registration order.
    pub async fn triggers(&self) -> Vec<rub_core::model::TriggerInfo> {
        self.trigger_runtime.read().await.triggers()
    }

    pub async fn trigger_rule(&self, id: u32) -> Option<rub_core::model::TriggerInfo> {
        self.trigger_runtime
            .read()
            .await
            .triggers()
            .into_iter()
            .find(|trigger| trigger.id == id)
    }

    /// Read the dedicated bounded trigger trace/history stream.
    pub async fn trigger_trace(&self, last: usize) -> rub_core::model::TriggerTraceProjection {
        self.trigger_runtime.read().await.trace(last)
    }

    /// Update one trigger lifecycle status.
    pub async fn set_trigger_status(
        &self,
        id: u32,
        status: rub_core::model::TriggerStatus,
    ) -> Option<rub_core::model::TriggerInfo> {
        self.trigger_runtime.write().await.update_status(id, status)
    }

    /// Record the most recent condition evidence for one trigger entry while
    /// keeping its current lifecycle status intact.
    pub async fn set_trigger_condition_evidence(
        &self,
        id: u32,
        evidence: Option<rub_core::model::TriggerEvidenceInfo>,
    ) -> Option<rub_core::model::TriggerInfo> {
        self.trigger_runtime
            .write()
            .await
            .set_condition_evidence(id, evidence)
    }

    /// Record the most recent trigger execution outcome and update the
    /// canonical runtime-wide last-result projection.
    pub async fn record_trigger_outcome(
        &self,
        id: u32,
        evidence: Option<rub_core::model::TriggerEvidenceInfo>,
        result: rub_core::model::TriggerResultInfo,
    ) -> Option<rub_core::model::TriggerInfo> {
        self.trigger_runtime
            .write()
            .await
            .record_outcome(id, evidence, result)
    }

    /// Remove one configured trigger rule.
    pub async fn remove_trigger(&self, id: u32) -> Option<rub_core::model::TriggerInfo> {
        self.trigger_runtime.write().await.remove(id)
    }

    /// Reconcile stable trigger tab bindings against the current live tab inventory.
    pub async fn reconcile_trigger_runtime(
        &self,
        tabs: &[TabInfo],
    ) -> rub_core::model::TriggerRuntimeInfo {
        self.trigger_runtime.write().await.reconcile_tabs(tabs)
    }
}
