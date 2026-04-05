use rub_core::model::{
    ConnectionTarget, HumanVerificationHandoffInfo, HumanVerificationHandoffStatus,
    LaunchPolicyInfo, SessionAccessibility, TakeoverRuntimeInfo, TakeoverRuntimeStatus,
    TakeoverTransitionInfo, TakeoverTransitionKind, TakeoverTransitionResult,
    TakeoverVisibilityMode,
};

/// Session-scoped accessibility/takeover authority.
#[derive(Debug, Default)]
pub struct TakeoverRuntimeState {
    projection: TakeoverRuntimeInfo,
    degraded_reason: Option<String>,
}

impl TakeoverRuntimeState {
    pub fn projection(&self) -> TakeoverRuntimeInfo {
        self.projection.clone()
    }

    pub fn refresh(
        &mut self,
        launch_policy: &LaunchPolicyInfo,
        handoff: &HumanVerificationHandoffInfo,
    ) -> TakeoverRuntimeInfo {
        let visibility_mode = derive_visibility_mode(launch_policy);
        let session_accessibility = if session_is_user_accessible(launch_policy) {
            SessionAccessibility::UserAccessible
        } else {
            SessionAccessibility::AutomationOnly
        };
        let elevate_supported = session_supports_elevation(launch_policy);
        let (status, unavailable_reason) = match session_accessibility {
            SessionAccessibility::UserAccessible => match handoff.status {
                HumanVerificationHandoffStatus::Active => (TakeoverRuntimeStatus::Active, None),
                HumanVerificationHandoffStatus::Available
                | HumanVerificationHandoffStatus::Completed => {
                    (TakeoverRuntimeStatus::Available, None)
                }
                HumanVerificationHandoffStatus::Unavailable => (
                    TakeoverRuntimeStatus::Degraded,
                    Some("handoff_unavailable".to_string()),
                ),
            },
            SessionAccessibility::AutomationOnly => (
                TakeoverRuntimeStatus::Unavailable,
                Some(if elevate_supported {
                    "elevation_required".to_string()
                } else {
                    "automation_only_session".to_string()
                }),
            ),
        };

        let mut projection = TakeoverRuntimeInfo {
            status,
            session_accessibility,
            visibility_mode,
            elevate_supported,
            resume_supported: handoff.resume_supported
                && matches!(session_accessibility, SessionAccessibility::UserAccessible),
            automation_paused: handoff.automation_paused,
            unavailable_reason,
            last_transition: self.projection.last_transition.clone(),
        };
        if let Some(reason) = self.degraded_reason.clone() {
            projection.status = TakeoverRuntimeStatus::Degraded;
            projection.unavailable_reason = Some(reason);
        }

        self.projection = projection;
        self.projection()
    }

    pub fn record_transition(
        &mut self,
        kind: TakeoverTransitionKind,
        result: TakeoverTransitionResult,
        reason: Option<String>,
    ) -> TakeoverRuntimeInfo {
        self.projection.last_transition = Some(TakeoverTransitionInfo {
            kind,
            result,
            reason,
        });
        self.projection()
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) -> TakeoverRuntimeInfo {
        self.degraded_reason = Some(reason.into());
        self.projection.status = TakeoverRuntimeStatus::Degraded;
        self.projection.unavailable_reason = self.degraded_reason.clone();
        self.projection()
    }

    pub fn clear_degraded(&mut self) {
        self.degraded_reason = None;
    }
}

fn session_is_user_accessible(launch_policy: &LaunchPolicyInfo) -> bool {
    if matches!(
        launch_policy.connection_target.as_ref(),
        Some(ConnectionTarget::CdpUrl { .. } | ConnectionTarget::AutoDiscovered { .. })
    ) {
        return true;
    }

    !launch_policy.headless
}

fn derive_visibility_mode(launch_policy: &LaunchPolicyInfo) -> TakeoverVisibilityMode {
    if matches!(
        launch_policy.connection_target.as_ref(),
        Some(ConnectionTarget::CdpUrl { .. } | ConnectionTarget::AutoDiscovered { .. })
    ) {
        return TakeoverVisibilityMode::External;
    }

    if launch_policy.headless {
        TakeoverVisibilityMode::Headless
    } else {
        TakeoverVisibilityMode::Headed
    }
}

fn session_supports_elevation(launch_policy: &LaunchPolicyInfo) -> bool {
    !session_is_user_accessible(launch_policy)
        && !matches!(
            launch_policy.connection_target.as_ref(),
            Some(ConnectionTarget::CdpUrl { .. } | ConnectionTarget::AutoDiscovered { .. })
        )
}

#[cfg(test)]
mod tests {
    use super::TakeoverRuntimeState;
    use rub_core::model::{
        ConnectionTarget, HumanVerificationHandoffInfo, HumanVerificationHandoffStatus,
        LaunchPolicyInfo, SessionAccessibility, TakeoverRuntimeStatus, TakeoverTransitionKind,
        TakeoverTransitionResult, TakeoverVisibilityMode,
    };

    fn managed_launch_policy(headless: bool) -> LaunchPolicyInfo {
        LaunchPolicyInfo {
            headless,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: None,
            connection_target: Some(ConnectionTarget::Managed),
            stealth_level: None,
            stealth_patches: None,
            stealth_default_enabled: None,
            humanize_enabled: None,
            humanize_speed: None,
            stealth_coverage: None,
        }
    }

    #[test]
    fn managed_headed_sessions_project_available_takeover_runtime() {
        let mut state = TakeoverRuntimeState::default();
        let runtime = state.refresh(
            &managed_launch_policy(false),
            &HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Available,
                automation_paused: false,
                resume_supported: true,
                unavailable_reason: None,
            },
        );

        assert_eq!(runtime.status, TakeoverRuntimeStatus::Available);
        assert_eq!(
            runtime.session_accessibility,
            SessionAccessibility::UserAccessible
        );
        assert_eq!(runtime.visibility_mode, TakeoverVisibilityMode::Headed);
        assert!(!runtime.elevate_supported);
    }

    #[test]
    fn managed_headless_sessions_project_unavailable_takeover_runtime() {
        let mut state = TakeoverRuntimeState::default();
        let runtime = state.refresh(
            &managed_launch_policy(true),
            &HumanVerificationHandoffInfo::default(),
        );

        assert_eq!(runtime.status, TakeoverRuntimeStatus::Unavailable);
        assert_eq!(
            runtime.session_accessibility,
            SessionAccessibility::AutomationOnly
        );
        assert_eq!(runtime.visibility_mode, TakeoverVisibilityMode::Headless);
        assert!(runtime.elevate_supported);
        assert_eq!(
            runtime.unavailable_reason.as_deref(),
            Some("elevation_required")
        );
    }

    #[test]
    fn external_sessions_project_active_takeover_runtime_when_handoff_is_active() {
        let mut state = TakeoverRuntimeState::default();
        let runtime = state.refresh(
            &LaunchPolicyInfo {
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
            },
            &HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Active,
                automation_paused: true,
                resume_supported: true,
                unavailable_reason: None,
            },
        );

        assert_eq!(runtime.status, TakeoverRuntimeStatus::Active);
        assert_eq!(runtime.visibility_mode, TakeoverVisibilityMode::External);
        assert_eq!(
            runtime.session_accessibility,
            SessionAccessibility::UserAccessible
        );
        assert!(runtime.automation_paused);
    }

    #[test]
    fn transition_metadata_is_preserved_on_projection() {
        let mut state = TakeoverRuntimeState::default();
        state.record_transition(
            TakeoverTransitionKind::Start,
            TakeoverTransitionResult::Rejected,
            Some("automation_only_session".to_string()),
        );
        let runtime = state.refresh(
            &managed_launch_policy(true),
            &HumanVerificationHandoffInfo::default(),
        );
        let transition = runtime.last_transition.expect("transition should exist");
        assert_eq!(transition.kind, TakeoverTransitionKind::Start);
        assert_eq!(transition.result, TakeoverTransitionResult::Rejected);
        assert_eq!(
            transition.reason.as_deref(),
            Some("automation_only_session")
        );
    }

    #[test]
    fn continuity_degradation_overrides_accessible_projection_until_cleared() {
        let mut state = TakeoverRuntimeState::default();
        let _ = state.refresh(
            &managed_launch_policy(false),
            &HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Available,
                automation_paused: false,
                resume_supported: true,
                unavailable_reason: None,
            },
        );
        let degraded = state.mark_degraded("continuity_frame_stale");
        assert_eq!(degraded.status, TakeoverRuntimeStatus::Degraded);
        assert_eq!(
            degraded.unavailable_reason.as_deref(),
            Some("continuity_frame_stale")
        );

        state.clear_degraded();
        let refreshed = state.refresh(
            &managed_launch_policy(false),
            &HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Available,
                automation_paused: false,
                resume_supported: true,
                unavailable_reason: None,
            },
        );
        assert_eq!(refreshed.status, TakeoverRuntimeStatus::Available);
        assert_eq!(refreshed.unavailable_reason, None);
    }
}
