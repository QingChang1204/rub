use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterferenceRefreshIntent {
    ReadOnly,
    PolicyDriven,
}

#[derive(Debug, Clone)]
pub(crate) struct InterferenceRefreshSnapshot {
    pub(crate) tabs: Vec<TabInfo>,
    pub(crate) interference: InterferenceRuntimeInfo,
    pub(crate) readiness: ReadinessInfo,
}

pub(crate) async fn refresh_live_interference_state(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
    intent: InterferenceRefreshIntent,
) -> Result<Vec<TabInfo>, RubError> {
    let launch_policy = browser.launch_policy();
    match browser.list_tabs().await {
        Ok(tabs) => {
            let runtime = state.classify_interference_runtime(&tabs).await;
            apply_policy_driven_handoff(intent, state, &runtime, &launch_policy).await;
            Ok(tabs)
        }
        Err(error) => {
            state
                .mark_interference_runtime_degraded(format!("tab_probe_failed:{error}"))
                .await;
            Err(error)
        }
    }
}

pub(crate) async fn refresh_live_runtime_and_interference_snapshot(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
    intent: InterferenceRefreshIntent,
) -> Result<InterferenceRefreshSnapshot, RubError> {
    refresh_live_runtime_state(browser, state).await;
    refresh_live_frame_runtime(browser, state).await;
    refresh_live_storage_runtime(browser, state).await;
    refresh_takeover_runtime(browser, state).await;
    let tabs = refresh_live_interference_state(browser, state, intent).await?;
    Ok(InterferenceRefreshSnapshot {
        tabs,
        interference: state.interference_runtime().await,
        readiness: state.readiness_state().await,
    })
}

pub(crate) async fn refresh_live_runtime_and_interference(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
    intent: InterferenceRefreshIntent,
) -> Result<Vec<TabInfo>, RubError> {
    refresh_live_runtime_and_interference_snapshot(browser, state, intent)
        .await
        .map(|snapshot| snapshot.tabs)
}

pub(super) async fn apply_policy_driven_handoff(
    intent: InterferenceRefreshIntent,
    state: &Arc<SessionState>,
    runtime: &InterferenceRuntimeInfo,
    launch_policy: &LaunchPolicyInfo,
) {
    if matches!(intent, InterferenceRefreshIntent::ReadOnly) {
        return;
    }

    let should_escalate = matches!(
        runtime
            .current_interference
            .as_ref()
            .map(|current| current.kind),
        Some(InterferenceKind::HumanVerificationRequired)
    ) && runtime
        .active_policies
        .iter()
        .any(|policy| policy == "handoff_escalation");
    if !should_escalate {
        return;
    }

    let handoff = state.human_verification_handoff().await;
    if matches!(
        handoff.status,
        HumanVerificationHandoffStatus::Available | HumanVerificationHandoffStatus::Completed
    ) {
        state.activate_handoff().await;
        state.refresh_takeover_runtime(launch_policy).await;
    }
}
