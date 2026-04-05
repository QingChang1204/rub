use rub_core::model::StateInspectorInfo;

/// Session-scoped state inspector authority.
#[derive(Debug, Default)]
pub struct StateInspectorState {
    projection: StateInspectorInfo,
}

impl StateInspectorState {
    pub fn projection(&self) -> StateInspectorInfo {
        self.projection.clone()
    }

    pub fn is_ready(&self) -> bool {
        !matches!(
            self.projection.status,
            rub_core::model::StateInspectorStatus::Inactive
        )
    }

    pub fn replace(&mut self, projection: StateInspectorInfo) {
        self.projection = projection;
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) {
        self.projection = StateInspectorInfo {
            status: rub_core::model::StateInspectorStatus::Degraded,
            degraded_reason: Some(reason.into()),
            ..StateInspectorInfo::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::StateInspectorState;
    use rub_core::model::{AuthState, StateInspectorInfo, StateInspectorStatus};

    #[test]
    fn state_inspector_state_tracks_projection_and_readiness() {
        let mut state = StateInspectorState::default();
        assert!(!state.is_ready());

        state.replace(StateInspectorInfo {
            status: StateInspectorStatus::Active,
            auth_state: AuthState::Authenticated,
            cookie_count: 2,
            local_storage_keys: vec!["token".to_string()],
            session_storage_keys: vec!["csrf".to_string()],
            auth_signals: vec![
                "cookies_present".to_string(),
                "local_storage_present".to_string(),
                "session_storage_present".to_string(),
                "auth_like_storage_key_present".to_string(),
            ],
            degraded_reason: None,
        });

        let projection = state.projection();
        assert!(state.is_ready());
        assert_eq!(projection.auth_state, AuthState::Authenticated);
        assert_eq!(projection.cookie_count, 2);
        assert_eq!(projection.local_storage_keys, vec!["token"]);
        assert_eq!(projection.session_storage_keys, vec!["csrf"]);
        assert_eq!(
            projection.auth_signals,
            vec![
                "cookies_present",
                "local_storage_present",
                "session_storage_present",
                "auth_like_storage_key_present",
            ]
        );
        assert_eq!(projection.degraded_reason, None);
    }

    #[test]
    fn state_inspector_state_can_mark_degraded() {
        let mut state = StateInspectorState::default();
        state.mark_degraded("live_probe_failed:no_page");

        let projection = state.projection();
        assert!(state.is_ready());
        assert_eq!(projection.status, StateInspectorStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("live_probe_failed:no_page")
        );
        assert_eq!(projection.cookie_count, 0);
        assert!(projection.local_storage_keys.is_empty());
    }
}
