use rub_core::model::ReadinessInfo;

/// Session-scoped readiness heuristics authority.
#[derive(Debug, Default)]
pub struct ReadinessState {
    projection: ReadinessInfo,
}

impl ReadinessState {
    pub fn projection(&self) -> ReadinessInfo {
        self.projection.clone()
    }

    pub fn is_ready(&self) -> bool {
        !matches!(
            self.projection.status,
            rub_core::model::ReadinessStatus::Inactive
        )
    }

    pub fn replace(&mut self, projection: ReadinessInfo) {
        self.projection = projection;
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) {
        self.projection = ReadinessInfo {
            status: rub_core::model::ReadinessStatus::Degraded,
            degraded_reason: Some(reason.into()),
            ..ReadinessInfo::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::ReadinessState;
    use rub_core::model::{OverlayState, ReadinessInfo, ReadinessStatus, RouteStability};

    #[test]
    fn readiness_state_tracks_projection_and_readiness() {
        let mut state = ReadinessState::default();
        assert!(!state.is_ready());

        state.replace(ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Transitioning,
            loading_present: true,
            skeleton_present: true,
            overlay_state: OverlayState::Development,
            document_ready_state: Some("interactive".to_string()),
            blocking_signals: vec![
                "document_ready_state:interactive".to_string(),
                "loading_present".to_string(),
                "skeleton_present".to_string(),
                "overlay:development".to_string(),
                "route_transitioning".to_string(),
            ],
            degraded_reason: None,
        });

        let projection = state.projection();
        assert!(state.is_ready());
        assert_eq!(projection.route_stability, RouteStability::Transitioning);
        assert!(projection.loading_present);
        assert!(projection.skeleton_present);
        assert_eq!(projection.overlay_state, OverlayState::Development);
        assert_eq!(
            projection.document_ready_state.as_deref(),
            Some("interactive")
        );
        assert_eq!(
            projection.blocking_signals,
            vec![
                "document_ready_state:interactive",
                "loading_present",
                "skeleton_present",
                "overlay:development",
                "route_transitioning",
            ]
        );
        assert_eq!(projection.degraded_reason, None);
    }

    #[test]
    fn readiness_state_can_mark_degraded() {
        let mut state = ReadinessState::default();
        state.mark_degraded("live_probe_failed:no_page");

        let projection = state.projection();
        assert!(state.is_ready());
        assert_eq!(projection.status, ReadinessStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("live_probe_failed:no_page")
        );
        assert_eq!(projection.document_ready_state, None);
        assert!(projection.blocking_signals.is_empty());
    }
}
