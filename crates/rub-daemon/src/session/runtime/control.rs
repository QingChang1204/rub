use super::*;

impl SessionState {
    /// Current session-scoped human verification handoff projection.
    pub async fn human_verification_handoff(&self) -> HumanVerificationHandoffInfo {
        self.handoff.read().await.projection()
    }

    /// Current session-scoped accessibility/takeover runtime projection.
    pub async fn takeover_runtime(&self) -> TakeoverRuntimeInfo {
        self.takeover.read().await.projection()
    }

    /// Recompute the canonical takeover runtime from launch policy + handoff authority.
    pub async fn refresh_takeover_runtime(
        &self,
        launch_policy: &rub_core::model::LaunchPolicyInfo,
    ) -> TakeoverRuntimeInfo {
        let handoff = self.handoff.read().await.projection();
        self.takeover.write().await.refresh(launch_policy, &handoff)
    }

    /// Record the last takeover transition outcome without mutating the
    /// underlying launch-policy or handoff authorities.
    pub async fn record_takeover_transition(
        &self,
        kind: TakeoverTransitionKind,
        result: TakeoverTransitionResult,
        reason: Option<String>,
    ) -> TakeoverRuntimeInfo {
        self.takeover
            .write()
            .await
            .record_transition(kind, result, reason)
    }

    /// Mark the takeover runtime surface as degraded when relaunch/resume
    /// continuity fences fail.
    pub async fn mark_takeover_runtime_degraded(&self, reason: impl Into<String>) {
        self.takeover.write().await.mark_degraded(reason);
    }

    /// Clear any takeover-runtime degradation override so the canonical
    /// launch-policy + handoff authority can project the live status again.
    pub async fn clear_takeover_runtime_degraded(&self) {
        self.takeover.write().await.clear_degraded();
    }

    /// Current session-scoped public-web interference runtime projection.
    pub async fn interference_runtime(&self) -> InterferenceRuntimeInfo {
        self.interference.read().await.projection()
    }

    /// Replace the current public-web interference runtime projection.
    pub async fn set_interference_runtime(&self, runtime: InterferenceRuntimeInfo) {
        self.interference.write().await.replace(runtime);
    }

    /// Set the canonical public-web interference mode for this session.
    pub async fn set_interference_mode(
        &self,
        mode: rub_core::model::InterferenceMode,
    ) -> InterferenceRuntimeInfo {
        self.interference.write().await.set_mode(mode)
    }

    /// Prime the canonical interference baseline from the current active tab
    /// when the session has not yet established a primary context.
    pub async fn prime_interference_baseline(&self, tabs: &[TabInfo]) {
        self.interference
            .write()
            .await
            .prime_baseline_from_tabs(tabs);
    }

    /// Adopt the current active tab as the canonical primary context after an
    /// explicit user-driven navigation fence.
    pub async fn adopt_interference_primary_context(&self, tabs: &[TabInfo]) {
        self.interference
            .write()
            .await
            .adopt_primary_context_from_tabs(tabs);
    }

    /// Mark the interference runtime surface as degraded.
    pub async fn mark_interference_runtime_degraded(&self, reason: impl Into<String>) {
        self.interference.write().await.mark_degraded(reason);
    }

    /// Recompute the canonical public-web interference runtime projection from
    /// the current session-scoped runtime surfaces and live tab context.
    pub async fn classify_interference_runtime(&self, tabs: &[TabInfo]) -> InterferenceRuntimeInfo {
        let observatory_guard = self.observatory.read().await;
        let observatory = self.projected_observatory(&observatory_guard);
        let readiness = self.runtime_state.read().await.readiness();
        let handoff = self.handoff.read().await.projection();
        self.interference
            .write()
            .await
            .classify(tabs, &observatory, &readiness, &handoff)
    }

    /// Snapshot the current recovery context used by the safe recovery coordinator.
    pub(crate) async fn interference_recovery_context(&self) -> InterferenceRecoveryContext {
        self.interference.read().await.recovery_context()
    }

    /// Mark an interference recovery attempt as in progress.
    pub(crate) async fn begin_interference_recovery(
        &self,
        action: InterferenceRecoveryAction,
    ) -> InterferenceRuntimeInfo {
        self.interference.write().await.begin_recovery(action)
    }

    /// Mark an interference recovery attempt as completed.
    pub(crate) async fn finish_interference_recovery(
        &self,
        result: InterferenceRecoveryResult,
    ) -> InterferenceRuntimeInfo {
        self.interference.write().await.finish_recovery(result)
    }

    /// Record a completed recovery outcome even when no browser mutation was attempted.
    pub(crate) async fn record_interference_recovery_outcome(
        &self,
        action: Option<InterferenceRecoveryAction>,
        result: InterferenceRecoveryResult,
    ) -> InterferenceRuntimeInfo {
        self.interference
            .write()
            .await
            .record_recovery_outcome(action, result)
    }

    /// Replace the current human verification handoff projection.
    pub async fn set_human_verification_handoff(&self, handoff: HumanVerificationHandoffInfo) {
        self.handoff.write().await.replace(handoff);
    }

    /// Mark the session as capable of human verification handoff.
    pub async fn set_handoff_available(&self, resume_supported: bool) {
        self.handoff.write().await.set_available(resume_supported);
    }

    /// Activate human verification handoff and pause automation.
    pub async fn activate_handoff(&self) {
        self.handoff.write().await.activate();
    }

    /// Complete the current human verification handoff and resume automation.
    pub async fn complete_handoff(&self) {
        self.handoff.write().await.complete();
    }

    /// Whether automation is currently paused for human verification handoff.
    pub async fn is_handoff_active(&self) -> bool {
        self.handoff.read().await.projection().automation_paused
    }

    /// Whether the session is currently held by explicit human control.
    pub async fn has_active_human_control(&self) -> bool {
        if self.is_handoff_active().await {
            return true;
        }

        let takeover = self.takeover.read().await.projection();
        takeover.automation_paused
            || matches!(
                takeover.status,
                rub_core::model::TakeoverRuntimeStatus::Active
            )
    }

    /// Whether the session is idle enough for upgrade/restart coordination.
    pub async fn is_idle_for_upgrade(&self) -> bool {
        self.is_base_idle_for_upgrade() && !self.has_active_human_control().await
    }
}
