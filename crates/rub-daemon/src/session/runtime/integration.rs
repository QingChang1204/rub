use super::*;

impl SessionState {
    /// Current session-scoped developer integration runtime projection.
    pub async fn integration_runtime(&self) -> IntegrationRuntimeInfo {
        let mut integration = self.integration_runtime.read().await.clone();
        integration.sync_request_rule_count();
        let observatory_guard = self.observatory.read().await;
        let observatory = self.projected_observatory(&observatory_guard);
        let runtime_state = self.runtime_state.read().await.snapshot();
        let state_inspector = runtime_state.state_inspector.clone();
        let readiness = runtime_state.readiness_state.clone();
        let handoff = self.handoff.read().await.projection();
        let (active_surfaces, degraded_surfaces) = derive_integration_runtime_surfaces(
            &integration,
            &observatory,
            &state_inspector,
            &readiness,
            &handoff,
        );

        integration.status = derive_integration_runtime_status(
            integration.status,
            &integration.request_rules,
            observatory.status,
            state_inspector.status,
            readiness.status,
        );
        integration.active_surfaces = active_surfaces;
        integration.degraded_surfaces = degraded_surfaces;
        integration.observatory_ready =
            !matches!(observatory.status, RuntimeObservatoryStatus::Inactive);
        integration.state_inspector_ready =
            !matches!(state_inspector.status, StateInspectorStatus::Inactive);
        integration.readiness_ready = !matches!(readiness.status, ReadinessStatus::Inactive);
        integration.handoff_ready = !matches!(
            handoff.status,
            rub_core::model::HumanVerificationHandoffStatus::Unavailable
        );
        integration
    }

    /// Replace the current developer integration runtime projection.
    pub async fn set_integration_runtime(&self, mut integration: IntegrationRuntimeInfo) {
        integration.sync_request_rule_count();
        *self.integration_runtime.write().await = integration;
    }
}
