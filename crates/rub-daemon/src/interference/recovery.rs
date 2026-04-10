use super::*;

impl InterferenceRuntimeState {
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
