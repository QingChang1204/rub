use std::sync::Arc;

use crate::runtime_refresh::{
    InterferenceRefreshIntent, RefreshOutcome, refresh_live_dialog_runtime,
    refresh_live_frame_runtime, refresh_live_interference_state,
    refresh_live_runtime_and_interference, refresh_live_runtime_state,
    refresh_live_storage_runtime, refresh_live_trigger_runtime, refresh_orchestration_runtime,
    refresh_takeover_runtime,
};
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};

use super::super::DaemonRouter;
use super::super::downloads::annotate_download_runtime_path_states;
use super::projection::{runtime_projection_state, runtime_subject, runtime_surface_payload};
use crate::router::request_args::subcommand_arg;

#[derive(Clone, Copy, Debug)]
pub(super) enum RuntimeSurface {
    Summary,
    Integration,
    Dialog,
    Downloads,
    Frame,
    Interference,
    Storage,
    Takeover,
    Orchestration,
    Trigger,
    Observatory,
    StateInspector,
    Readiness,
    Handoff,
    BindingCaptureCandidate,
}

impl RuntimeSurface {
    pub(super) fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        Self::parse_name(subcommand_arg(args, "summary"))
    }

    fn parse_name(name: &str) -> Result<Self, RubError> {
        match name {
            "summary" => Ok(Self::Summary),
            "integration" => Ok(Self::Integration),
            "dialog" => Ok(Self::Dialog),
            "downloads" => Ok(Self::Downloads),
            "frame" => Ok(Self::Frame),
            "interference" => Ok(Self::Interference),
            "storage" => Ok(Self::Storage),
            "takeover" => Ok(Self::Takeover),
            "orchestration" => Ok(Self::Orchestration),
            "trigger" => Ok(Self::Trigger),
            "observatory" => Ok(Self::Observatory),
            "state-inspector" => Ok(Self::StateInspector),
            "readiness" => Ok(Self::Readiness),
            "handoff" => Ok(Self::Handoff),
            "binding-capture-candidate" => Ok(Self::BindingCaptureCandidate),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown runtime subcommand: '{other}'"),
            )),
        }
    }

    pub(super) fn subject(self) -> serde_json::Value {
        runtime_subject(self.name())
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Integration => "integration",
            Self::Dialog => "dialog",
            Self::Downloads => "downloads",
            Self::Frame => "frame",
            Self::Interference => "interference",
            Self::Storage => "storage",
            Self::Takeover => "takeover",
            Self::Orchestration => "orchestration",
            Self::Trigger => "trigger",
            Self::Observatory => "observatory",
            Self::StateInspector => "state-inspector",
            Self::Readiness => "readiness",
            Self::Handoff => "handoff",
            Self::BindingCaptureCandidate => "binding-capture-candidate",
        }
    }

    pub(super) fn projection_authority(self) -> &'static str {
        match self {
            Self::Summary => "session.runtime_summary",
            Self::Integration => "session.integration_runtime",
            Self::Dialog => "session.dialog_runtime",
            Self::Downloads => "session.download_runtime",
            Self::Frame => "session.frame_runtime",
            Self::Interference => "session.interference_runtime",
            Self::Storage => "session.storage_runtime",
            Self::Takeover => "session.takeover_runtime",
            Self::Orchestration => "session.orchestration_runtime",
            Self::Trigger => "session.trigger_runtime",
            Self::Observatory => "session.runtime_observatory",
            Self::StateInspector => "session.state_inspector",
            Self::Readiness => "session.readiness_state",
            Self::Handoff => "session.human_verification_handoff",
            Self::BindingCaptureCandidate => "session.binding_capture_candidate",
        }
    }

    pub(super) async fn refresh(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Vec<RefreshOutcome> {
        match self {
            Self::Summary => {
                let outcomes = vec![
                    refresh_live_runtime_state(&router.browser, state).await,
                    refresh_live_dialog_runtime(&router.browser, state).await,
                    refresh_live_frame_runtime(&router.browser, state).await,
                    refresh_live_storage_runtime(&router.browser, state).await,
                    refresh_takeover_runtime(&router.browser, state).await,
                ];
                refresh_orchestration_runtime(state).await;
                let _ = refresh_live_trigger_runtime(&router.browser, state).await;
                let _ = refresh_live_interference_state(
                    &router.browser,
                    state,
                    InterferenceRefreshIntent::ReadOnly,
                )
                .await;
                outcomes
            }
            Self::Integration | Self::StateInspector | Self::Readiness => {
                vec![refresh_live_runtime_state(&router.browser, state).await]
            }
            Self::Dialog => {
                vec![refresh_live_dialog_runtime(&router.browser, state).await]
            }
            Self::Frame => {
                vec![refresh_live_frame_runtime(&router.browser, state).await]
            }
            Self::Interference => {
                let _ = refresh_live_runtime_and_interference(
                    &router.browser,
                    state,
                    InterferenceRefreshIntent::ReadOnly,
                )
                .await;
                Vec::new()
            }
            Self::Storage => {
                vec![refresh_live_storage_runtime(&router.browser, state).await]
            }
            Self::Takeover => {
                vec![refresh_takeover_runtime(&router.browser, state).await]
            }
            Self::Orchestration => {
                refresh_orchestration_runtime(state).await;
                Vec::new()
            }
            Self::Trigger => {
                let _ = refresh_live_trigger_runtime(&router.browser, state).await;
                Vec::new()
            }
            Self::Observatory | Self::Downloads | Self::Handoff => Vec::new(),
            Self::BindingCaptureCandidate => {
                vec![
                    refresh_live_runtime_state(&router.browser, state).await,
                    refresh_takeover_runtime(&router.browser, state).await,
                ]
            }
        }
    }

    pub(super) async fn projection(
        self,
        state: &Arc<SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        match self {
            Self::Summary => Ok(runtime_summary(state).await),
            Self::Integration => {
                serde_json::to_value(state.integration_runtime().await).map_err(RubError::from)
            }
            Self::Dialog => {
                serde_json::to_value(state.dialog_runtime().await).map_err(RubError::from)
            }
            Self::Downloads => {
                let mut runtime =
                    serde_json::to_value(state.download_runtime().await).map_err(RubError::from)?;
                annotate_download_runtime_path_states(&mut runtime);
                Ok(runtime)
            }
            Self::Frame => {
                serde_json::to_value(state.frame_runtime().await).map_err(RubError::from)
            }
            Self::Interference => {
                serde_json::to_value(state.interference_runtime().await).map_err(RubError::from)
            }
            Self::Storage => {
                serde_json::to_value(state.storage_runtime().await).map_err(RubError::from)
            }
            Self::Takeover => {
                serde_json::to_value(state.takeover_runtime().await).map_err(RubError::from)
            }
            Self::Orchestration => {
                serde_json::to_value(state.orchestration_runtime().await).map_err(RubError::from)
            }
            Self::Trigger => {
                serde_json::to_value(state.trigger_runtime().await).map_err(RubError::from)
            }
            Self::Observatory => {
                serde_json::to_value(state.observatory().await).map_err(RubError::from)
            }
            Self::StateInspector => {
                serde_json::to_value(state.state_inspector().await).map_err(RubError::from)
            }
            Self::Readiness => {
                serde_json::to_value(state.readiness_state().await).map_err(RubError::from)
            }
            Self::Handoff => serde_json::to_value(state.human_verification_handoff().await)
                .map_err(RubError::from),
            Self::BindingCaptureCandidate => {
                serde_json::to_value(super::binding_capture::binding_capture_candidate(state).await)
                    .map_err(RubError::from)
            }
        }
    }
}

pub(super) async fn runtime_summary(state: &Arc<SessionState>) -> serde_json::Value {
    let runtime_state = state.runtime_state_snapshot().await;
    let mut download_runtime =
        serde_json::to_value(state.download_runtime().await).unwrap_or(serde_json::Value::Null);
    annotate_download_runtime_path_states(&mut download_runtime);
    serde_json::json!({
        "integration_runtime": state.integration_runtime().await,
        "dialog_runtime": state.dialog_runtime().await,
        "download_runtime": download_runtime,
        "frame_runtime": state.frame_runtime().await,
        "interference_runtime": state.interference_runtime().await,
        "storage_runtime": state.storage_runtime().await,
        "takeover_runtime": state.takeover_runtime().await,
        "orchestration_runtime": state.orchestration_runtime().await,
        "trigger_runtime": state.trigger_runtime().await,
        "runtime_observatory": state.observatory().await,
        "state_inspector": runtime_state.state_inspector,
        "readiness_state": runtime_state.readiness_state,
        "human_verification_handoff": state.human_verification_handoff().await,
        "post_commit_journal": state.post_commit_journal_projection(),
    })
}

pub(super) async fn cmd_runtime(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    super::super::request_args::reject_unknown_fields(args, &["sub"], "runtime")?;
    let surface = RuntimeSurface::parse(args)?;
    let refresh_outcomes = surface.refresh(router, state).await;
    let mut payload = runtime_surface_payload(
        surface.subject(),
        runtime_projection_state(surface.name(), surface.projection_authority()),
        surface.projection(state).await?,
    );
    if let Some(object) = payload.as_object_mut()
        && !refresh_outcomes.is_empty()
    {
        object.insert(
            "refresh_outcomes".to_string(),
            serde_json::to_value(refresh_outcomes).map_err(RubError::from)?,
        );
    }
    Ok(payload)
}
