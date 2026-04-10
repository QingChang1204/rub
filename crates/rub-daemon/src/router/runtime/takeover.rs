use super::projection::{runtime_projection_state, runtime_surface_payload};
use super::*;
use crate::router::request_args::subcommand_arg;
use crate::runtime_refresh::{refresh_live_runtime_and_interference, refresh_takeover_runtime};
use rub_core::model::{
    FrameContextStatus, HumanVerificationHandoffStatus, IntegrationRuntimeStatus,
    IntegrationSurface, ReadinessStatus, TakeoverRuntimeStatus, TakeoverTransitionKind,
    TakeoverTransitionResult,
};

#[derive(Clone, Copy, Debug)]
enum HandoffAction {
    Status,
    Start,
    Complete,
}

impl HandoffAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match subcommand_arg(args, "status") {
            "status" => Ok(Self::Status),
            "start" => Ok(Self::Start),
            "complete" => Ok(Self::Complete),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown handoff subcommand: '{other}'"),
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Start => "start",
            Self::Complete => "complete",
        }
    }

    async fn execute(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        match self {
            Self::Status => Ok(()),
            Self::Start => {
                ensure_handoff_available(state).await?;
                state.activate_handoff().await;
                refresh_takeover_runtime(&router.browser, state).await;
                Ok(())
            }
            Self::Complete => {
                ensure_handoff_available(state).await?;
                state.complete_handoff().await;
                refresh_takeover_runtime(&router.browser, state).await;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TakeoverAction {
    Status,
    Start,
    Elevate,
    Resume,
}

impl TakeoverAction {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match subcommand_arg(args, "status") {
            "status" => Ok(Self::Status),
            "start" => Ok(Self::Start),
            "elevate" => Ok(Self::Elevate),
            "resume" => Ok(Self::Resume),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown takeover subcommand: '{other}'"),
            )),
        }
    }

    fn kind(self) -> TakeoverTransitionKind {
        match self {
            Self::Status => TakeoverTransitionKind::Start,
            Self::Start => TakeoverTransitionKind::Start,
            Self::Elevate => TakeoverTransitionKind::Elevate,
            Self::Resume => TakeoverTransitionKind::Resume,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Start => "start",
            Self::Elevate => "elevate",
            Self::Resume => "resume",
        }
    }

    async fn execute(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        match self {
            Self::Status => Ok(()),
            Self::Start => self.execute_start(router, state).await,
            Self::Elevate => self.execute_elevate(router, state).await,
            Self::Resume => self.execute_resume(router, state).await,
        }
    }

    async fn execute_start(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        let current = state.takeover_runtime().await;
        if !matches!(
            current.status,
            TakeoverRuntimeStatus::Available | TakeoverRuntimeStatus::Active
        ) {
            return Err(self
                .reject(
                    state,
                    current.unavailable_reason.clone(),
                    "Session takeover is unavailable for this session",
                    serde_json::json!({ "takeover_runtime": state.takeover_runtime().await }),
                )
                .await);
        }
        state.activate_handoff().await;
        refresh_takeover_runtime(&router.browser, state).await;
        self.record_success(state).await;
        Ok(())
    }

    async fn execute_elevate(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        let current = state.takeover_runtime().await;
        if !current.elevate_supported {
            let reason = current
                .unavailable_reason
                .clone()
                .or_else(|| Some("elevation_not_supported".to_string()));
            return Err(self
                .reject(
                    state,
                    reason.clone(),
                    "Session takeover elevation is unavailable for this session",
                    serde_json::json!({
                        "takeover_runtime": state.takeover_runtime().await,
                        "reason": reason,
                    }),
                )
                .await);
        }
        router.browser.elevate_to_visible().await?;
        state.set_handoff_available(true).await;
        refresh_takeover_runtime(&router.browser, state).await;
        if let Err(error) = verify_takeover_continuity(router, state).await {
            self.record_rejection(state, Some("continuity_fence_failed".to_string()))
                .await;
            return Err(error);
        }
        self.record_success(state).await;
        Ok(())
    }

    async fn execute_resume(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<(), RubError> {
        let current = state.takeover_runtime().await;
        if !current.automation_paused || !current.resume_supported {
            let reason = if !current.resume_supported {
                current
                    .unavailable_reason
                    .clone()
                    .or_else(|| Some("resume_not_supported".to_string()))
            } else {
                Some("takeover_not_active".to_string())
            };
            return Err(self
                .reject(
                    state,
                    reason.clone(),
                    "Session takeover resume is unavailable for this session",
                    serde_json::json!({
                        "takeover_runtime": state.takeover_runtime().await,
                        "reason": reason,
                    }),
                )
                .await);
        }
        state.complete_handoff().await;
        refresh_takeover_runtime(&router.browser, state).await;
        if let Err(error) = verify_takeover_continuity(router, state).await {
            state.activate_handoff().await;
            refresh_takeover_runtime(&router.browser, state).await;
            self.record_rejection(state, Some("continuity_fence_failed".to_string()))
                .await;
            return Err(error);
        }
        let resumed_runtime = state.takeover_runtime().await;
        let handoff = state.human_verification_handoff().await;
        if let Some(error) = takeover_resume_repaused_error(&resumed_runtime, &handoff) {
            self.record_rejection(state, Some("automation_repaused_by_policy".to_string()))
                .await;
            return Err(error);
        }
        self.record_success(state).await;
        Ok(())
    }

    async fn reject(
        self,
        state: &Arc<SessionState>,
        reason: Option<String>,
        message: &'static str,
        context: serde_json::Value,
    ) -> RubError {
        self.record_rejection(state, reason).await;
        RubError::domain_with_context(ErrorCode::InvalidInput, message, context)
    }

    async fn record_rejection(self, state: &Arc<SessionState>, reason: Option<String>) {
        state
            .record_takeover_transition(self.kind(), TakeoverTransitionResult::Rejected, reason)
            .await;
    }

    async fn record_success(self, state: &Arc<SessionState>) {
        state
            .record_takeover_transition(self.kind(), TakeoverTransitionResult::Succeeded, None)
            .await;
    }
}

pub(super) async fn cmd_handoff(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let action = HandoffAction::parse(args)?;
    action.execute(router, state).await?;

    Ok(runtime_surface_payload(
        serde_json::json!({
            "kind": "human_verification_handoff",
            "action": action.name(),
        }),
        runtime_projection_state("handoff", "session.human_verification_handoff"),
        serde_json::to_value(state.human_verification_handoff().await).map_err(RubError::from)?,
    ))
}

pub(super) async fn cmd_takeover(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_takeover_runtime(&router.browser, state).await;

    let action = TakeoverAction::parse(args)?;
    action.execute(router, state).await?;

    Ok(runtime_surface_payload(
        serde_json::json!({
            "kind": "takeover",
            "action": action.name(),
        }),
        runtime_projection_state("takeover", "session.takeover_runtime"),
        serde_json::to_value(state.takeover_runtime().await).map_err(RubError::from)?,
    ))
}

async fn ensure_handoff_available(state: &Arc<SessionState>) -> Result<(), RubError> {
    let current = state.human_verification_handoff().await;
    if matches!(current.status, HumanVerificationHandoffStatus::Unavailable) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Human verification handoff is unavailable for this session",
        ));
    }
    Ok(())
}

async fn verify_takeover_continuity(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<(), RubError> {
    let tabs = refresh_live_runtime_and_interference(&router.browser, state)
        .await
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::BrowserCrashed,
                format!("Takeover continuity fence failed while refreshing runtime: {error}"),
                serde_json::json!({ "phase": "runtime_refresh" }),
            )
        })?;

    let active_tab = tabs.iter().any(|tab| tab.active);
    let frame_runtime = state.frame_runtime().await;
    let readiness = state.readiness_state().await;
    let integration = state.integration_runtime().await;

    let failure = takeover_continuity_failure(active_tab, &frame_runtime, &readiness, &integration);

    if let Some((reason, message)) = failure {
        state.mark_takeover_runtime_degraded(reason).await;
        return Err(RubError::domain_with_context(
            ErrorCode::BrowserCrashed,
            message,
            serde_json::json!({
                "reason": reason,
                "frame_runtime": frame_runtime,
                "readiness_state": readiness,
                "integration_runtime": integration,
                "takeover_runtime": state.takeover_runtime().await,
            }),
        ));
    }

    state.clear_takeover_runtime_degraded().await;
    refresh_takeover_runtime(&router.browser, state).await;
    Ok(())
}

pub(super) fn takeover_continuity_failure(
    active_tab: bool,
    frame_runtime: &rub_core::model::FrameRuntimeInfo,
    readiness: &rub_core::model::ReadinessInfo,
    integration: &rub_core::model::IntegrationRuntimeInfo,
) -> Option<(&'static str, &'static str)> {
    if !active_tab {
        return Some((
            "continuity_no_active_tab",
            "No active tab remained after takeover transition",
        ));
    }
    if matches!(
        frame_runtime.status,
        FrameContextStatus::Unknown | FrameContextStatus::Stale | FrameContextStatus::Degraded
    ) || frame_runtime.current_frame.is_none()
    {
        return Some((
            "continuity_frame_unavailable",
            "Frame context became unavailable after takeover transition",
        ));
    }
    if matches!(readiness.status, ReadinessStatus::Degraded) {
        return Some((
            "continuity_readiness_degraded",
            "Readiness surface degraded after takeover transition",
        ));
    }
    let takeover_required_surface_degraded = integration.degraded_surfaces.iter().any(|surface| {
        matches!(
            surface,
            IntegrationSurface::RequestRules | IntegrationSurface::RuntimeObservatory
        )
    });
    if matches!(integration.status, IntegrationRuntimeStatus::Degraded)
        && takeover_required_surface_degraded
    {
        return Some((
            "continuity_runtime_degraded",
            "Integration runtime degraded after takeover transition",
        ));
    }
    None
}

pub(super) fn takeover_resume_repaused_error(
    takeover: &rub_core::model::TakeoverRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> Option<RubError> {
    if !takeover.automation_paused && !handoff.automation_paused {
        return None;
    }

    Some(RubError::domain_with_context(
        ErrorCode::AutomationPaused,
        "Session takeover resumed briefly but policy-driven handoff immediately re-paused automation",
        serde_json::json!({
            "takeover_runtime": takeover,
            "handoff": handoff,
        }),
    ))
}
