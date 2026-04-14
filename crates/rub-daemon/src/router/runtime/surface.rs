use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCaptureAttachmentInfo,
    BindingCaptureAuthEvidence, BindingCaptureCandidateInfo, BindingCaptureDurabilityInfo,
    BindingCaptureFenceInfo, BindingCaptureFenceStatus, BindingCaptureLiveCorrelation,
    BindingCaptureSessionInfo, BindingCreatedVia, BindingDurabilityScope, BindingPersistencePolicy,
    BindingReattachmentMode, BindingSessionReference, BindingSessionReferenceKind,
    TakeoverTransitionKind, TakeoverTransitionResult,
};

use crate::rub_paths::{is_temp_owned_home, is_temp_root_path};
use crate::runtime_refresh::{
    refresh_live_dialog_runtime, refresh_live_frame_runtime, refresh_live_interference_state,
    refresh_live_runtime_state, refresh_live_storage_runtime, refresh_live_trigger_runtime,
    refresh_orchestration_runtime, refresh_takeover_runtime,
};
use crate::session::SessionState;

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

    pub(super) async fn refresh(self, router: &DaemonRouter, state: &Arc<SessionState>) {
        match self {
            Self::Summary => {
                refresh_live_runtime_state(&router.browser, state).await;
                refresh_live_dialog_runtime(&router.browser, state).await;
                refresh_live_frame_runtime(&router.browser, state).await;
                refresh_live_storage_runtime(&router.browser, state).await;
                refresh_takeover_runtime(&router.browser, state).await;
                refresh_orchestration_runtime(state).await;
                let _ = refresh_live_trigger_runtime(&router.browser, state).await;
                let _ = refresh_live_interference_state(&router.browser, state).await;
            }
            Self::Integration | Self::StateInspector | Self::Readiness => {
                refresh_live_runtime_state(&router.browser, state).await;
            }
            Self::Dialog => {
                refresh_live_dialog_runtime(&router.browser, state).await;
            }
            Self::Frame => {
                refresh_live_frame_runtime(&router.browser, state).await;
            }
            Self::Interference => {
                let _ = refresh_live_interference_state(&router.browser, state).await;
            }
            Self::Storage => {
                refresh_live_storage_runtime(&router.browser, state).await;
            }
            Self::Takeover => {
                refresh_takeover_runtime(&router.browser, state).await;
            }
            Self::Orchestration => {
                refresh_orchestration_runtime(state).await;
            }
            Self::Trigger => {
                let _ = refresh_live_trigger_runtime(&router.browser, state).await;
            }
            Self::Observatory | Self::Downloads | Self::Handoff => {}
            Self::BindingCaptureCandidate => {
                refresh_live_runtime_state(&router.browser, state).await;
                refresh_takeover_runtime(&router.browser, state).await;
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
                serde_json::to_value(binding_capture_candidate(state).await).map_err(RubError::from)
            }
        }
    }
}

async fn binding_capture_candidate(state: &Arc<SessionState>) -> BindingCaptureCandidateInfo {
    let launch_identity = state.launch_identity().await;
    let handoff = state.human_verification_handoff().await;
    let takeover = state.takeover_runtime().await;
    let state_inspector = state.state_inspector().await;
    let rub_home_temp_owned = is_temp_owned_home(&state.rub_home);
    let profile_directory_reference = match launch_identity.connection_target.as_ref() {
        Some(rub_core::model::ConnectionTarget::Profile { resolved_path, .. }) => {
            Some(resolved_path.clone())
        }
        _ => None,
    };

    let persistence_policy = project_binding_persistence_policy(
        &state.rub_home,
        launch_identity.connection_target.as_ref(),
        state.user_data_dir.as_deref(),
    );
    let (durability_scope, reattachment_mode, durability_reason) = project_binding_durability(
        persistence_policy,
        rub_home_temp_owned,
        state.user_data_dir.as_deref(),
    );
    let (capture_fence, auth_provenance_hint) = project_capture_fence_and_provenance(
        state,
        &handoff,
        &takeover,
        launch_identity.attachment_identity.as_deref(),
    );

    BindingCaptureCandidateInfo {
        session: BindingCaptureSessionInfo {
            session_id: state.session_id.clone(),
            session_name: state.session_name.clone(),
            rub_home_reference: state.rub_home.display().to_string(),
            rub_home_temp_owned,
        },
        attachment: BindingCaptureAttachmentInfo {
            attachment_identity: launch_identity.attachment_identity.clone(),
            connection_target: launch_identity.connection_target.clone(),
            profile_directory_reference,
            user_data_dir_reference: state.user_data_dir.clone(),
        },
        capture_fence,
        auth_evidence: BindingCaptureAuthEvidence {
            status: state_inspector.status,
            auth_state: state_inspector.auth_state,
            cookie_count: state_inspector.cookie_count,
            auth_signals: state_inspector.auth_signals,
            degraded_reason: state_inspector.degraded_reason,
        },
        durability: BindingCaptureDurabilityInfo {
            persistence_policy,
            durability_scope,
            reattachment_mode,
            status_reason: durability_reason,
        },
        live_correlation: BindingCaptureLiveCorrelation {
            session_reference: BindingSessionReference {
                kind: BindingSessionReferenceKind::LiveSessionHint,
                session_id: state.session_id.clone(),
                session_name: state.session_name.clone(),
            },
            attachment_identity: launch_identity.attachment_identity,
        },
        auth_provenance_hint,
    }
}

fn project_binding_persistence_policy(
    rub_home: &std::path::Path,
    connection_target: Option<&rub_core::model::ConnectionTarget>,
    user_data_dir: Option<&str>,
) -> BindingPersistencePolicy {
    if matches!(
        connection_target,
        Some(
            rub_core::model::ConnectionTarget::CdpUrl { .. }
                | rub_core::model::ConnectionTarget::AutoDiscovered { .. }
        )
    ) {
        return BindingPersistencePolicy::ExternalReattachmentRequired;
    }

    if is_temp_owned_home(rub_home)
        || user_data_dir
            .map(std::path::Path::new)
            .is_some_and(is_temp_root_path)
    {
        return BindingPersistencePolicy::RubHomeLocalEphemeral;
    }

    BindingPersistencePolicy::RubHomeLocalDurable
}

fn project_binding_durability(
    persistence_policy: BindingPersistencePolicy,
    rub_home_temp_owned: bool,
    user_data_dir: Option<&str>,
) -> (
    BindingDurabilityScope,
    BindingReattachmentMode,
    Option<String>,
) {
    match persistence_policy {
        BindingPersistencePolicy::ExternalReattachmentRequired => (
            BindingDurabilityScope::ExternalAttachment,
            BindingReattachmentMode::ExternalReattachRequired,
            Some("external_attachment_requires_reattachment".to_string()),
        ),
        BindingPersistencePolicy::RubHomeLocalEphemeral => (
            BindingDurabilityScope::RubHomeLocalEphemeral,
            BindingReattachmentMode::TempHomeEphemeral,
            Some(if rub_home_temp_owned {
                "temp_owned_rub_home_is_ephemeral".to_string()
            } else if user_data_dir
                .map(std::path::Path::new)
                .is_some_and(is_temp_root_path)
            {
                "temp_user_data_dir_is_ephemeral".to_string()
            } else {
                "runtime_is_ephemeral".to_string()
            }),
        ),
        BindingPersistencePolicy::RubHomeLocalDurable => (
            BindingDurabilityScope::RubHomeLocalDurable,
            BindingReattachmentMode::ManagedReacquirable,
            None,
        ),
    }
}

fn project_capture_fence_and_provenance(
    state: &Arc<SessionState>,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
    takeover: &rub_core::model::TakeoverRuntimeInfo,
    attachment_identity: Option<&str>,
) -> (BindingCaptureFenceInfo, BindingAuthProvenance) {
    let captured_from_session = Some(state.session_name.clone());
    let captured_from_attachment_identity = attachment_identity.map(ToOwned::to_owned);

    if handoff.automation_paused || takeover.automation_paused {
        return (
            BindingCaptureFenceInfo {
                status: BindingCaptureFenceStatus::CaptureUnavailable,
                capture_eligible: false,
                bind_current_eligible: false,
                capture_fence: None,
                status_reason: Some("automation_paused_for_human_control".to_string()),
            },
            BindingAuthProvenance {
                created_via: BindingCreatedVia::Unknown,
                auth_input_mode: BindingAuthInputMode::Unknown,
                capture_fence: None,
                captured_from_session,
                captured_from_attachment_identity,
            },
        );
    }

    if takeover.last_transition.as_ref().is_some_and(|transition| {
        transition.kind == TakeoverTransitionKind::Resume
            && transition.result == TakeoverTransitionResult::Succeeded
    }) {
        return (
            BindingCaptureFenceInfo {
                status: BindingCaptureFenceStatus::CaptureReady,
                capture_eligible: true,
                bind_current_eligible: true,
                capture_fence: Some("takeover_resume".to_string()),
                status_reason: Some("takeover_resume_succeeded".to_string()),
            },
            BindingAuthProvenance {
                created_via: BindingCreatedVia::TakeoverResumed,
                auth_input_mode: BindingAuthInputMode::Human,
                capture_fence: Some("takeover_resume".to_string()),
                captured_from_session,
                captured_from_attachment_identity,
            },
        );
    }

    if matches!(
        handoff.status,
        rub_core::model::HumanVerificationHandoffStatus::Completed
    ) {
        return (
            BindingCaptureFenceInfo {
                status: BindingCaptureFenceStatus::CaptureReady,
                capture_eligible: true,
                bind_current_eligible: true,
                capture_fence: Some("handoff_complete".to_string()),
                status_reason: Some("human_verification_handoff_completed".to_string()),
            },
            BindingAuthProvenance {
                created_via: BindingCreatedVia::HandoffCompleted,
                auth_input_mode: BindingAuthInputMode::Human,
                capture_fence: Some("handoff_complete".to_string()),
                captured_from_session,
                captured_from_attachment_identity,
            },
        );
    }

    (
        BindingCaptureFenceInfo {
            status: BindingCaptureFenceStatus::BindCurrentOnly,
            capture_eligible: false,
            bind_current_eligible: true,
            capture_fence: None,
            status_reason: Some("explicit_auth_completion_fence_missing".to_string()),
        },
        BindingAuthProvenance {
            created_via: BindingCreatedVia::BoundExistingRuntime,
            auth_input_mode: BindingAuthInputMode::Unknown,
            capture_fence: None,
            captured_from_session,
            captured_from_attachment_identity,
        },
    )
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
    })
}

pub(super) async fn cmd_runtime(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    super::super::request_args::reject_unknown_fields(args, &["sub"], "runtime")?;
    let surface = RuntimeSurface::parse(args)?;
    surface.refresh(router, state).await;
    Ok(runtime_surface_payload(
        surface.subject(),
        runtime_projection_state(surface.name(), surface.projection_authority()),
        surface.projection(state).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::binding_capture_candidate;
    use crate::session::SessionState;
    use rub_core::model::{
        AuthState, ConnectionTarget, HumanVerificationHandoffInfo, HumanVerificationHandoffStatus,
        LaunchPolicyInfo, ReadinessInfo, ReadinessStatus, RouteStability, RuntimeStateSnapshot,
        StateInspectorInfo, StateInspectorStatus, TakeoverTransitionKind, TakeoverTransitionResult,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

    fn sample_runtime_state(auth_state: AuthState) -> RuntimeStateSnapshot {
        RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Active,
                auth_state,
                cookie_count: 3,
                local_storage_keys: vec!["token".to_string()],
                session_storage_keys: vec!["csrf".to_string()],
                auth_signals: vec!["cookies_present".to_string()],
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability: RouteStability::Stable,
                loading_present: false,
                skeleton_present: false,
                overlay_state: rub_core::model::OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: Vec::new(),
                degraded_reason: None,
            },
        }
    }

    fn managed_launch_policy(headless: bool, user_data_dir: Option<&str>) -> LaunchPolicyInfo {
        LaunchPolicyInfo {
            headless,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: user_data_dir.map(str::to_string),
            connection_target: Some(ConnectionTarget::Managed),
            stealth_level: None,
            stealth_patches: None,
            stealth_default_enabled: None,
            humanize_enabled: None,
            humanize_speed: None,
            stealth_coverage: None,
        }
    }

    #[tokio::test]
    async fn binding_capture_candidate_projects_handoff_completed_capture_ready() {
        let home = PathBuf::from("/tmp/rub-binding-candidate-handoff");
        let state = Arc::new(SessionState::new(
            "default",
            home,
            Some("/Users/test/work".into()),
        ));
        state
            .publish_runtime_state_snapshot(1, sample_runtime_state(AuthState::Authenticated))
            .await;
        state
            .set_connection_target(Some(ConnectionTarget::Profile {
                name: "Work".to_string(),
                resolved_path: "/Users/test/Chrome/Profile 2".to_string(),
            }))
            .await;
        state
            .set_attachment_identity(Some("profile:Work".to_string()))
            .await;
        state
            .set_human_verification_handoff(HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Completed,
                automation_paused: false,
                resume_supported: true,
                unavailable_reason: None,
            })
            .await;
        state
            .refresh_takeover_runtime(&managed_launch_policy(false, Some("/Users/test/work")))
            .await;

        let candidate = binding_capture_candidate(&state).await;
        assert!(candidate.capture_fence.capture_eligible);
        assert_eq!(
            candidate.capture_fence.capture_fence.as_deref(),
            Some("handoff_complete")
        );
        assert_eq!(
            candidate.auth_provenance_hint.created_via,
            rub_core::model::BindingCreatedVia::HandoffCompleted
        );
        assert_eq!(
            candidate.attachment.profile_directory_reference.as_deref(),
            Some("/Users/test/Chrome/Profile 2")
        );
        assert_eq!(
            candidate.durability.persistence_policy,
            rub_core::model::BindingPersistencePolicy::RubHomeLocalDurable
        );
    }

    #[tokio::test]
    async fn binding_capture_candidate_marks_external_sessions_for_reattachment() {
        let home = PathBuf::from("/tmp/rub-binding-candidate-external");
        let state = Arc::new(SessionState::new("default", home, None));
        state
            .publish_runtime_state_snapshot(1, sample_runtime_state(AuthState::Authenticated))
            .await;
        state
            .set_connection_target(Some(ConnectionTarget::CdpUrl {
                url: "http://127.0.0.1:9222".to_string(),
            }))
            .await;
        state
            .set_attachment_identity(Some("cdp:http://127.0.0.1:9222".to_string()))
            .await;
        state.set_handoff_available(true).await;
        state
            .refresh_takeover_runtime(&LaunchPolicyInfo {
                headless: true,
                ignore_cert_errors: false,
                hide_infobars: true,
                user_data_dir: None,
                connection_target: Some(ConnectionTarget::CdpUrl {
                    url: "http://127.0.0.1:9222".to_string(),
                }),
                stealth_level: None,
                stealth_patches: None,
                stealth_default_enabled: None,
                humanize_enabled: None,
                humanize_speed: None,
                stealth_coverage: None,
            })
            .await;

        let candidate = binding_capture_candidate(&state).await;
        assert_eq!(
            candidate.durability.persistence_policy,
            rub_core::model::BindingPersistencePolicy::ExternalReattachmentRequired
        );
        assert_eq!(
            candidate.durability.reattachment_mode,
            rub_core::model::BindingReattachmentMode::ExternalReattachRequired
        );
        assert_eq!(
            candidate.capture_fence.status,
            rub_core::model::BindingCaptureFenceStatus::BindCurrentOnly
        );
    }

    #[tokio::test]
    async fn binding_capture_candidate_blocks_capture_while_handoff_is_active() {
        let home = PathBuf::from("/tmp/rub-binding-candidate-active-handoff");
        let state = Arc::new(SessionState::new("default", home, None));
        state
            .publish_runtime_state_snapshot(1, sample_runtime_state(AuthState::Authenticated))
            .await;
        state.set_handoff_available(true).await;
        state.activate_handoff().await;
        state
            .refresh_takeover_runtime(&managed_launch_policy(false, None))
            .await;

        let candidate = binding_capture_candidate(&state).await;
        assert_eq!(
            candidate.capture_fence.status,
            rub_core::model::BindingCaptureFenceStatus::CaptureUnavailable
        );
        assert!(!candidate.capture_fence.bind_current_eligible);
        assert_eq!(
            candidate.capture_fence.status_reason.as_deref(),
            Some("automation_paused_for_human_control")
        );
    }

    #[tokio::test]
    async fn binding_capture_candidate_prefers_takeover_resume_over_stale_handoff_completed() {
        let home = PathBuf::from("/tmp/rub-binding-candidate-takeover-resume");
        let state = Arc::new(SessionState::new(
            "default",
            home,
            Some("/Users/test/work".into()),
        ));
        state
            .publish_runtime_state_snapshot(1, sample_runtime_state(AuthState::Authenticated))
            .await;
        state
            .set_connection_target(Some(ConnectionTarget::Profile {
                name: "Work".to_string(),
                resolved_path: "/Users/test/Chrome/Profile 2".to_string(),
            }))
            .await;
        state
            .set_attachment_identity(Some("profile:Work".to_string()))
            .await;
        state
            .set_human_verification_handoff(HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Completed,
                automation_paused: false,
                resume_supported: true,
                unavailable_reason: None,
            })
            .await;
        state
            .refresh_takeover_runtime(&managed_launch_policy(false, Some("/Users/test/work")))
            .await;
        state
            .record_takeover_transition(
                TakeoverTransitionKind::Resume,
                TakeoverTransitionResult::Succeeded,
                None,
            )
            .await;

        let candidate = binding_capture_candidate(&state).await;
        assert!(candidate.capture_fence.capture_eligible);
        assert_eq!(
            candidate.capture_fence.capture_fence.as_deref(),
            Some("takeover_resume")
        );
        assert_eq!(
            candidate.auth_provenance_hint.created_via,
            rub_core::model::BindingCreatedVia::TakeoverResumed
        );
    }
}
