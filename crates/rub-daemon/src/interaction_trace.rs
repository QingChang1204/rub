use std::sync::Arc;

use rub_core::model::{
    InterferenceRuntimeInfo, InterferenceStateDelta, ReadinessInfo, RuntimeStateDelta,
    RuntimeStateSnapshot, StateInspectorInfo,
};
use rub_core::port::BrowserPort;

pub async fn probe_runtime_state(browser: &Arc<dyn BrowserPort>) -> Option<RuntimeStateSnapshot> {
    browser.probe_runtime_state().await.ok()
}

pub fn runtime_state_delta(
    before: &RuntimeStateSnapshot,
    after: &RuntimeStateSnapshot,
) -> Option<RuntimeStateDelta> {
    let changed = changed_runtime_state_fields(before, after);
    if changed.is_empty() {
        return None;
    }

    Some(RuntimeStateDelta {
        before: before.clone(),
        after: after.clone(),
        changed,
    })
}

pub fn interference_state_delta(
    before: &InterferenceRuntimeInfo,
    after: &InterferenceRuntimeInfo,
) -> Option<InterferenceStateDelta> {
    let changed = changed_interference_fields(before, after);
    if changed.is_empty() {
        return None;
    }

    Some(InterferenceStateDelta {
        before: before.clone(),
        after: after.clone(),
        changed,
    })
}

fn changed_runtime_state_fields(
    before: &RuntimeStateSnapshot,
    after: &RuntimeStateSnapshot,
) -> Vec<String> {
    let mut changed = Vec::new();
    append_state_inspector_changes(
        &mut changed,
        &before.state_inspector,
        &after.state_inspector,
    );
    append_readiness_changes(
        &mut changed,
        &before.readiness_state,
        &after.readiness_state,
    );
    changed
}

fn append_state_inspector_changes(
    changed: &mut Vec<String>,
    before: &StateInspectorInfo,
    after: &StateInspectorInfo,
) {
    if before.status != after.status {
        changed.push("state_inspector.status".to_string());
    }
    if before.auth_state != after.auth_state {
        changed.push("state_inspector.auth_state".to_string());
    }
    if before.cookie_count != after.cookie_count {
        changed.push("state_inspector.cookie_count".to_string());
    }
    if before.local_storage_keys != after.local_storage_keys {
        changed.push("state_inspector.local_storage_keys".to_string());
    }
    if before.session_storage_keys != after.session_storage_keys {
        changed.push("state_inspector.session_storage_keys".to_string());
    }
    if before.auth_signals != after.auth_signals {
        changed.push("state_inspector.auth_signals".to_string());
    }
    if before.degraded_reason != after.degraded_reason {
        changed.push("state_inspector.degraded_reason".to_string());
    }
}

fn append_readiness_changes(
    changed: &mut Vec<String>,
    before: &ReadinessInfo,
    after: &ReadinessInfo,
) {
    if before.status != after.status {
        changed.push("readiness_state.status".to_string());
    }
    if before.route_stability != after.route_stability {
        changed.push("readiness_state.route_stability".to_string());
    }
    if before.loading_present != after.loading_present {
        changed.push("readiness_state.loading_present".to_string());
    }
    if before.skeleton_present != after.skeleton_present {
        changed.push("readiness_state.skeleton_present".to_string());
    }
    if before.overlay_state != after.overlay_state {
        changed.push("readiness_state.overlay_state".to_string());
    }
    if before.document_ready_state != after.document_ready_state {
        changed.push("readiness_state.document_ready_state".to_string());
    }
    if before.blocking_signals != after.blocking_signals {
        changed.push("readiness_state.blocking_signals".to_string());
    }
    if before.degraded_reason != after.degraded_reason {
        changed.push("readiness_state.degraded_reason".to_string());
    }
}

fn changed_interference_fields(
    before: &InterferenceRuntimeInfo,
    after: &InterferenceRuntimeInfo,
) -> Vec<String> {
    let mut changed = Vec::new();
    if before.mode != after.mode {
        changed.push("interference_runtime.mode".to_string());
    }
    if before.status != after.status {
        changed.push("interference_runtime.status".to_string());
    }
    if before.current_interference != after.current_interference {
        changed.push("interference_runtime.current_interference".to_string());
    }
    if before.last_interference != after.last_interference {
        changed.push("interference_runtime.last_interference".to_string());
    }
    if before.active_policies != after.active_policies {
        changed.push("interference_runtime.active_policies".to_string());
    }
    if before.recovery_in_progress != after.recovery_in_progress {
        changed.push("interference_runtime.recovery_in_progress".to_string());
    }
    if before.last_recovery_action != after.last_recovery_action {
        changed.push("interference_runtime.last_recovery_action".to_string());
    }
    if before.last_recovery_result != after.last_recovery_result {
        changed.push("interference_runtime.last_recovery_result".to_string());
    }
    if before.handoff_required != after.handoff_required {
        changed.push("interference_runtime.handoff_required".to_string());
    }
    if before.degraded_reason != after.degraded_reason {
        changed.push("interference_runtime.degraded_reason".to_string());
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::{interference_state_delta, runtime_state_delta};
    use rub_core::model::{
        AuthState, InterferenceKind, InterferenceMode, InterferenceObservation,
        InterferenceRuntimeInfo, InterferenceRuntimeStatus, OverlayState, ReadinessInfo,
        ReadinessStatus, RouteStability, RuntimeStateSnapshot, StateInspectorInfo,
        StateInspectorStatus,
    };

    #[test]
    fn runtime_state_delta_reports_changed_fields() {
        let before = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Active,
                auth_state: AuthState::Anonymous,
                cookie_count: 0,
                local_storage_keys: Vec::new(),
                session_storage_keys: Vec::new(),
                auth_signals: Vec::new(),
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability: RouteStability::Stable,
                loading_present: false,
                skeleton_present: false,
                overlay_state: OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: Vec::new(),
                degraded_reason: None,
            },
        };
        let after = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Active,
                auth_state: AuthState::Unknown,
                cookie_count: 0,
                local_storage_keys: vec!["authToken".to_string()],
                session_storage_keys: Vec::new(),
                auth_signals: vec![
                    "local_storage_present".to_string(),
                    "auth_like_storage_key_present".to_string(),
                ],
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability: RouteStability::Transitioning,
                loading_present: true,
                skeleton_present: false,
                overlay_state: OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: vec![
                    "loading_present".to_string(),
                    "route_transitioning".to_string(),
                ],
                degraded_reason: None,
            },
        };

        let delta = runtime_state_delta(&before, &after).expect("delta should exist");
        assert_eq!(
            delta.changed,
            vec![
                "state_inspector.auth_state",
                "state_inspector.local_storage_keys",
                "state_inspector.auth_signals",
                "readiness_state.route_stability",
                "readiness_state.loading_present",
                "readiness_state.blocking_signals",
            ]
        );
    }

    #[test]
    fn runtime_state_delta_omits_empty_changes() {
        let snapshot = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo::default(),
            readiness_state: ReadinessInfo::default(),
        };
        assert!(runtime_state_delta(&snapshot, &snapshot).is_none());
    }

    #[test]
    fn runtime_state_delta_reports_degraded_reason_changes() {
        let before = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo::default(),
            readiness_state: ReadinessInfo::default(),
        };
        let after = RuntimeStateSnapshot {
            state_inspector: StateInspectorInfo {
                status: StateInspectorStatus::Degraded,
                degraded_reason: Some("live_probe_failed:no_page".to_string()),
                ..StateInspectorInfo::default()
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Degraded,
                degraded_reason: Some("live_probe_failed:no_page".to_string()),
                ..ReadinessInfo::default()
            },
        };

        let delta = runtime_state_delta(&before, &after).expect("delta should exist");
        assert!(
            delta
                .changed
                .contains(&"state_inspector.degraded_reason".to_string())
        );
        assert!(
            delta
                .changed
                .contains(&"readiness_state.degraded_reason".to_string())
        );
    }

    #[test]
    fn interference_state_delta_reports_changed_fields() {
        let before = InterferenceRuntimeInfo::default();
        let after = InterferenceRuntimeInfo {
            mode: InterferenceMode::PublicWebStable,
            status: InterferenceRuntimeStatus::Active,
            current_interference: Some(InterferenceObservation {
                kind: InterferenceKind::InterstitialNavigation,
                summary: "interstitial-like navigation drift detected".to_string(),
                current_url: Some("https://example.test/interstitial#vignette".to_string()),
                primary_url: Some("https://example.test/app".to_string()),
            }),
            active_policies: vec![
                "safe_recovery".to_string(),
                "handoff_escalation".to_string(),
            ],
            handoff_required: false,
            ..InterferenceRuntimeInfo::default()
        };

        let delta = interference_state_delta(&before, &after).expect("delta should exist");
        assert_eq!(
            delta.changed,
            vec![
                "interference_runtime.mode",
                "interference_runtime.status",
                "interference_runtime.current_interference",
                "interference_runtime.active_policies",
            ]
        );
    }
}
