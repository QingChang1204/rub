use rub_core::model::{
    HumanVerificationHandoffInfo, HumanVerificationHandoffStatus, IntegrationRuntimeInfo,
    IntegrationRuntimeStatus, IntegrationSurface, NetworkRule, NetworkRuleStatus, ReadinessInfo,
    ReadinessStatus, RuntimeObservatoryInfo, RuntimeObservatoryStatus, StateInspectorInfo,
    StateInspectorStatus,
};

pub(super) fn derive_integration_runtime_status(
    current: IntegrationRuntimeStatus,
    request_rules: &[NetworkRule],
    observatory_status: RuntimeObservatoryStatus,
    state_inspector_status: StateInspectorStatus,
    readiness_status: ReadinessStatus,
) -> IntegrationRuntimeStatus {
    if matches!(current, IntegrationRuntimeStatus::Unsupported) {
        return IntegrationRuntimeStatus::Unsupported;
    }

    if matches!(current, IntegrationRuntimeStatus::Degraded)
        || request_rules
            .iter()
            .any(|rule| matches!(rule.status, NetworkRuleStatus::Degraded))
        || matches!(observatory_status, RuntimeObservatoryStatus::Degraded)
        || matches!(state_inspector_status, StateInspectorStatus::Degraded)
        || matches!(readiness_status, ReadinessStatus::Degraded)
    {
        return IntegrationRuntimeStatus::Degraded;
    }

    if !request_rules.is_empty()
        || matches!(observatory_status, RuntimeObservatoryStatus::Active)
        || matches!(state_inspector_status, StateInspectorStatus::Active)
        || matches!(readiness_status, ReadinessStatus::Active)
    {
        return IntegrationRuntimeStatus::Active;
    }

    IntegrationRuntimeStatus::Inactive
}

pub(super) fn derive_integration_runtime_surfaces(
    integration: &IntegrationRuntimeInfo,
    observatory: &RuntimeObservatoryInfo,
    state_inspector: &StateInspectorInfo,
    readiness: &ReadinessInfo,
    handoff: &HumanVerificationHandoffInfo,
) -> (Vec<IntegrationSurface>, Vec<IntegrationSurface>) {
    let mut active = Vec::new();
    let mut degraded = Vec::new();

    if !integration.request_rules.is_empty() {
        active.push(IntegrationSurface::RequestRules);
    }
    if integration
        .request_rules
        .iter()
        .any(|rule| matches!(rule.status, NetworkRuleStatus::Degraded))
    {
        degraded.push(IntegrationSurface::RequestRules);
    }

    match observatory.status {
        RuntimeObservatoryStatus::Active => active.push(IntegrationSurface::RuntimeObservatory),
        RuntimeObservatoryStatus::Degraded => degraded.push(IntegrationSurface::RuntimeObservatory),
        RuntimeObservatoryStatus::Inactive => {}
    }

    match state_inspector.status {
        StateInspectorStatus::Active => active.push(IntegrationSurface::StateInspector),
        StateInspectorStatus::Degraded => degraded.push(IntegrationSurface::StateInspector),
        StateInspectorStatus::Inactive => {}
    }

    match readiness.status {
        ReadinessStatus::Active => active.push(IntegrationSurface::Readiness),
        ReadinessStatus::Degraded => degraded.push(IntegrationSurface::Readiness),
        ReadinessStatus::Inactive => {}
    }

    if !matches!(handoff.status, HumanVerificationHandoffStatus::Unavailable) {
        active.push(IntegrationSurface::HumanVerificationHandoff);
    }

    (active, degraded)
}
