use crate::readiness::ReadinessState;
use crate::state_inspector::StateInspectorState;
use rub_core::model::{ReadinessInfo, RuntimeStateSnapshot, StateInspectorInfo};

/// Session-scoped runtime-state authority.
///
/// The browser publishes runtime state as one combined snapshot. This store is
/// the SSOT for session-level `state_inspector` + `readiness_state` surfaces
/// and only accepts snapshots in monotonic probe-start sequence order.
#[derive(Debug, Default)]
pub struct RuntimeStateProjectionState {
    state_inspector: StateInspectorState,
    readiness: ReadinessState,
    last_sequence: u64,
}

impl RuntimeStateProjectionState {
    pub fn snapshot(&self) -> RuntimeStateSnapshot {
        RuntimeStateSnapshot {
            state_inspector: self.state_inspector.projection(),
            readiness_state: self.readiness.projection(),
        }
    }

    pub fn state_inspector(&self) -> StateInspectorInfo {
        self.state_inspector.projection()
    }

    pub fn readiness(&self) -> ReadinessInfo {
        self.readiness.projection()
    }

    pub fn replace(&mut self, sequence: u64, snapshot: RuntimeStateSnapshot) -> bool {
        if sequence < self.last_sequence {
            return false;
        }
        self.last_sequence = sequence;
        self.state_inspector.replace(snapshot.state_inspector);
        self.readiness.replace(snapshot.readiness_state);
        true
    }

    pub fn mark_degraded(&mut self, sequence: u64, reason: impl Into<String>) -> bool {
        if sequence < self.last_sequence {
            return false;
        }
        self.last_sequence = sequence;
        let reason = reason.into();
        self.state_inspector.mark_degraded(reason.clone());
        self.readiness.mark_degraded(reason);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeStateProjectionState;
    use rub_core::model::{
        AuthState, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
        RuntimeStateSnapshot, StateInspectorInfo, StateInspectorStatus,
    };

    fn sample_snapshot(cookie_count: u32, route_stability: RouteStability) -> RuntimeStateSnapshot {
        RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Active,
                auth_state: AuthState::Authenticated,
                cookie_count,
                local_storage_keys: vec!["token".to_string()],
                session_storage_keys: vec!["csrf".to_string()],
                auth_signals: vec!["cookies_present".to_string()],
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability,
                loading_present: false,
                skeleton_present: false,
                overlay_state: OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: Vec::new(),
                degraded_reason: None,
            },
        }
    }

    #[test]
    fn runtime_state_projection_replaces_both_surfaces_atomically() {
        let mut state = RuntimeStateProjectionState::default();
        assert!(state.replace(1, sample_snapshot(2, RouteStability::Stable)));

        let snapshot = state.snapshot();
        assert_eq!(snapshot.state_inspector.cookie_count, 2);
        assert_eq!(
            snapshot.readiness_state.route_stability,
            RouteStability::Stable
        );
    }

    #[test]
    fn runtime_state_projection_rejects_stale_sequences() {
        let mut state = RuntimeStateProjectionState::default();
        assert!(state.replace(2, sample_snapshot(2, RouteStability::Stable)));
        assert!(!state.replace(1, sample_snapshot(7, RouteStability::Transitioning)));

        let snapshot = state.snapshot();
        assert_eq!(snapshot.state_inspector.cookie_count, 2);
        assert_eq!(
            snapshot.readiness_state.route_stability,
            RouteStability::Stable
        );
    }
}
