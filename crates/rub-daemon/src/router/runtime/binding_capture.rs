use std::sync::Arc;

use rub_core::model::{
    BindingAuthInputMode, BindingAuthProvenance, BindingCaptureAttachmentInfo,
    BindingCaptureAuthEvidence, BindingCaptureCandidateInfo, BindingCaptureDiagnostics,
    BindingCaptureDurabilityInfo, BindingCaptureFenceInfo, BindingCaptureFenceStatus,
    BindingCaptureLiveCorrelation, BindingCaptureSessionInfo, BindingCreatedVia,
    BindingDurabilityScope, BindingPersistencePolicy, BindingReattachmentMode,
    BindingSessionReference, BindingSessionReferenceKind, StateInspectorStatus,
    TakeoverTransitionKind, TakeoverTransitionResult,
};

use crate::rub_paths::{is_temp_owned_home, is_temp_root_path};
use crate::session::SessionState;

#[derive(Debug, Clone)]
struct BindingCaptureInputs {
    session_id: String,
    session_name: String,
    rub_home_reference: String,
    rub_home_temp_owned: bool,
    attachment_identity: Option<String>,
    connection_target: Option<rub_core::model::ConnectionTarget>,
    profile_directory_reference: Option<String>,
    user_data_dir_reference: Option<String>,
    handoff: rub_core::model::HumanVerificationHandoffInfo,
    takeover: rub_core::model::TakeoverRuntimeInfo,
    auth_evidence: BindingCaptureAuthEvidence,
}

#[derive(Debug, Clone)]
struct BindingCaptureAssessment {
    capture_fence: BindingCaptureFenceInfo,
    auth_provenance_hint: BindingAuthProvenance,
    diagnostics: BindingCaptureDiagnostics,
}

#[derive(Debug, Clone)]
struct BindingCaptureDurabilityProjection {
    persistence_policy: BindingPersistencePolicy,
    durability_scope: BindingDurabilityScope,
    reattachment_mode: BindingReattachmentMode,
    status_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct BindingCaptureProjection {
    session: BindingCaptureSessionInfo,
    attachment: BindingCaptureAttachmentInfo,
    durability: BindingCaptureDurabilityProjection,
    live_correlation: BindingCaptureLiveCorrelation,
}

pub(super) async fn binding_capture_candidate(
    state: &Arc<SessionState>,
) -> BindingCaptureCandidateInfo {
    let inputs = read_binding_capture_inputs(state).await;
    let projection = project_binding_capture(&inputs);
    let assessment = assess_binding_capture(&inputs);

    BindingCaptureCandidateInfo {
        session: projection.session,
        attachment: projection.attachment,
        capture_fence: assessment.capture_fence,
        auth_evidence: inputs.auth_evidence,
        durability: BindingCaptureDurabilityInfo {
            persistence_policy: projection.durability.persistence_policy,
            durability_scope: projection.durability.durability_scope,
            reattachment_mode: projection.durability.reattachment_mode,
            status_reason: projection.durability.status_reason,
        },
        live_correlation: projection.live_correlation,
        auth_provenance_hint: assessment.auth_provenance_hint,
        diagnostics: assessment.diagnostics,
    }
}

async fn read_binding_capture_inputs(state: &Arc<SessionState>) -> BindingCaptureInputs {
    // Read the live surfaces together to minimize interleaving across this
    // evidence-only composite without pretending it is a single-epoch snapshot.
    let (launch_identity, handoff, takeover, state_inspector) = tokio::join!(
        state.launch_identity(),
        state.human_verification_handoff(),
        state.takeover_runtime(),
        state.state_inspector(),
    );
    let rub_home_temp_owned = is_temp_owned_home(&state.rub_home);
    let profile_directory_reference = match launch_identity.connection_target.as_ref() {
        Some(rub_core::model::ConnectionTarget::Profile { resolved_path, .. }) => {
            Some(resolved_path.clone())
        }
        _ => None,
    };

    BindingCaptureInputs {
        session_id: state.session_id.clone(),
        session_name: state.session_name.clone(),
        rub_home_reference: state.rub_home.display().to_string(),
        rub_home_temp_owned,
        attachment_identity: launch_identity.attachment_identity,
        connection_target: launch_identity.connection_target,
        profile_directory_reference,
        user_data_dir_reference: state.user_data_dir.clone(),
        handoff,
        takeover,
        auth_evidence: BindingCaptureAuthEvidence {
            status: state_inspector.status,
            auth_state: state_inspector.auth_state,
            cookie_count: state_inspector.cookie_count,
            auth_signals: state_inspector.auth_signals,
            degraded_reason: state_inspector.degraded_reason,
        },
    }
}

fn project_binding_persistence_policy(inputs: &BindingCaptureInputs) -> BindingPersistencePolicy {
    if matches!(
        inputs.connection_target.as_ref(),
        Some(
            rub_core::model::ConnectionTarget::CdpUrl { .. }
                | rub_core::model::ConnectionTarget::AutoDiscovered { .. }
        )
    ) {
        return BindingPersistencePolicy::ExternalReattachmentRequired;
    }

    if inputs.rub_home_temp_owned
        || inputs
            .user_data_dir_reference
            .as_deref()
            .map(std::path::Path::new)
            .is_some_and(is_temp_root_path)
    {
        return BindingPersistencePolicy::RubHomeLocalEphemeral;
    }

    BindingPersistencePolicy::RubHomeLocalDurable
}

fn project_binding_capture(inputs: &BindingCaptureInputs) -> BindingCaptureProjection {
    let durability = project_binding_durability(inputs);

    BindingCaptureProjection {
        session: BindingCaptureSessionInfo {
            session_id: inputs.session_id.clone(),
            session_name: inputs.session_name.clone(),
            rub_home_reference: inputs.rub_home_reference.clone(),
            rub_home_temp_owned: inputs.rub_home_temp_owned,
        },
        attachment: BindingCaptureAttachmentInfo {
            attachment_identity: inputs.attachment_identity.clone(),
            connection_target: inputs.connection_target.clone(),
            profile_directory_reference: inputs.profile_directory_reference.clone(),
            user_data_dir_reference: inputs.user_data_dir_reference.clone(),
        },
        durability,
        live_correlation: BindingCaptureLiveCorrelation {
            session_reference: BindingSessionReference {
                kind: BindingSessionReferenceKind::LiveSessionHint,
                session_id: inputs.session_id.clone(),
                session_name: inputs.session_name.clone(),
            },
            attachment_identity: inputs.attachment_identity.clone(),
        },
    }
}

fn project_binding_durability(inputs: &BindingCaptureInputs) -> BindingCaptureDurabilityProjection {
    let persistence_policy = project_binding_persistence_policy(inputs);

    match persistence_policy {
        BindingPersistencePolicy::ExternalReattachmentRequired => {
            BindingCaptureDurabilityProjection {
                persistence_policy,
                durability_scope: BindingDurabilityScope::ExternalAttachment,
                reattachment_mode: BindingReattachmentMode::ExternalReattachRequired,
                status_reason: Some("external_attachment_requires_reattachment".to_string()),
            }
        }
        BindingPersistencePolicy::RubHomeLocalEphemeral => BindingCaptureDurabilityProjection {
            persistence_policy,
            durability_scope: BindingDurabilityScope::RubHomeLocalEphemeral,
            reattachment_mode: BindingReattachmentMode::TempHomeEphemeral,
            status_reason: Some(if inputs.rub_home_temp_owned {
                "temp_owned_rub_home_is_ephemeral".to_string()
            } else if inputs
                .user_data_dir_reference
                .as_deref()
                .map(std::path::Path::new)
                .is_some_and(is_temp_root_path)
            {
                "temp_user_data_dir_is_ephemeral".to_string()
            } else {
                "runtime_is_ephemeral".to_string()
            }),
        },
        BindingPersistencePolicy::RubHomeLocalDurable => BindingCaptureDurabilityProjection {
            persistence_policy,
            durability_scope: BindingDurabilityScope::RubHomeLocalDurable,
            reattachment_mode: BindingReattachmentMode::ManagedReacquirable,
            status_reason: None,
        },
    }
}

fn project_capture_fence_and_provenance(
    inputs: &BindingCaptureInputs,
) -> (BindingCaptureFenceInfo, BindingAuthProvenance) {
    let captured_from_session = Some(inputs.session_name.clone());
    let captured_from_attachment_identity = inputs.attachment_identity.clone();

    if inputs.handoff.automation_paused || inputs.takeover.automation_paused {
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

    if takeover_resume_succeeded(&inputs.takeover) {
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
        inputs.handoff.status,
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

fn assess_binding_capture(inputs: &BindingCaptureInputs) -> BindingCaptureAssessment {
    let (capture_fence, auth_provenance_hint) = project_capture_fence_and_provenance(inputs);
    let diagnostics = project_capture_diagnostics(inputs, &capture_fence);
    BindingCaptureAssessment {
        capture_fence,
        auth_provenance_hint,
        diagnostics,
    }
}

fn project_capture_diagnostics(
    inputs: &BindingCaptureInputs,
    capture_fence: &BindingCaptureFenceInfo,
) -> BindingCaptureDiagnostics {
    let mut consistency_warnings = Vec::new();
    let resume_succeeded = takeover_resume_succeeded(&inputs.takeover);
    let handoff_completed = matches!(
        inputs.handoff.status,
        rub_core::model::HumanVerificationHandoffStatus::Completed
    );

    if inputs.handoff.automation_paused && resume_succeeded {
        consistency_warnings
            .push("active_human_control_conflicts_with_stale_takeover_resume".to_string());
    }

    if handoff_completed && resume_succeeded {
        consistency_warnings.push("takeover_resume_overrides_stale_handoff_completed".to_string());
    }

    if capture_fence.status == BindingCaptureFenceStatus::BindCurrentOnly
        && inputs.auth_evidence.auth_state == rub_core::model::AuthState::Authenticated
    {
        consistency_warnings
            .push("authenticated_evidence_without_explicit_capture_fence".to_string());
    }

    if capture_fence.capture_eligible
        && inputs.auth_evidence.auth_state != rub_core::model::AuthState::Authenticated
    {
        consistency_warnings.push("capture_ready_with_non_authenticated_auth_evidence".to_string());
    }

    if capture_fence.capture_eligible && inputs.auth_evidence.status != StateInspectorStatus::Active
    {
        consistency_warnings.push("capture_ready_with_non_active_state_inspector".to_string());
    }

    BindingCaptureDiagnostics {
        consistency_warnings,
    }
}

fn takeover_resume_succeeded(takeover: &rub_core::model::TakeoverRuntimeInfo) -> bool {
    takeover.last_transition.as_ref().is_some_and(|transition| {
        transition.kind == TakeoverTransitionKind::Resume
            && transition.result == TakeoverTransitionResult::Succeeded
    })
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
        assert!(
            candidate
                .diagnostics
                .consistency_warnings
                .contains(&"authenticated_evidence_without_explicit_capture_fence".to_string())
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
        assert!(
            candidate
                .diagnostics
                .consistency_warnings
                .contains(&"takeover_resume_overrides_stale_handoff_completed".to_string())
        );
    }

    #[tokio::test]
    async fn binding_capture_candidate_blocks_capture_when_active_handoff_conflicts_with_stale_resume()
     {
        let home = PathBuf::from("/tmp/rub-binding-candidate-active-handoff-stale-resume");
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
        state.activate_handoff().await;

        let candidate = binding_capture_candidate(&state).await;
        assert_eq!(
            candidate.capture_fence.status,
            rub_core::model::BindingCaptureFenceStatus::CaptureUnavailable
        );
        assert_eq!(
            candidate.capture_fence.status_reason.as_deref(),
            Some("automation_paused_for_human_control")
        );
        assert_eq!(
            candidate.auth_provenance_hint.created_via,
            rub_core::model::BindingCreatedVia::Unknown
        );
        assert!(
            candidate
                .diagnostics
                .consistency_warnings
                .contains(&"active_human_control_conflicts_with_stale_takeover_resume".to_string())
        );
    }

    #[tokio::test]
    async fn binding_capture_candidate_flags_capture_ready_with_non_authenticated_evidence() {
        let home = PathBuf::from("/tmp/rub-binding-candidate-ready-non-authenticated");
        let state = Arc::new(SessionState::new(
            "default",
            home,
            Some("/Users/test/work".into()),
        ));
        state
            .publish_runtime_state_snapshot(1, sample_runtime_state(AuthState::Anonymous))
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
        assert!(
            candidate
                .diagnostics
                .consistency_warnings
                .contains(&"capture_ready_with_non_authenticated_auth_evidence".to_string())
        );
    }
}
