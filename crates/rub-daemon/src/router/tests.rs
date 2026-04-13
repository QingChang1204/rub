use super::policy::command_allowed_during_handoff;
use super::queue::request_owns_authoritative_timeout;
use super::timeout::{
    TimeoutPhase, augment_wait_timeout_error, timeout_context, wait_timeout_error,
};
use super::transaction::{
    finalize_response, prepare_replay_fence, protocol_version_mismatch_response,
    replay_request_fingerprint,
};
use super::{
    DaemonRouter, PendingExternalDomCommit, TransactionDeadline, attach_interaction_projection,
    attach_select_projection, detection_risks,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    IdentityProbeStatus, IdentitySelfProbeInfo, InteractionActuation, InteractionConfirmation,
    InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
    InteractionSemanticClass, LaunchPolicyInfo, SelectOutcome,
};
use rub_ipc::codec::MAX_FRAME_BYTES;
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use std::sync::atomic::AtomicU64;
use std::{path::PathBuf, sync::Arc};

use crate::session::{ReplayCommandClaim, SessionState};

fn test_router() -> DaemonRouter {
    let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        },
    ));
    let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        rub_cdp::humanize::HumanizeConfig {
            enabled: false,
            speed: rub_cdp::humanize::HumanizeSpeed::Normal,
        },
    ));
    DaemonRouter::new(adapter)
}

#[test]
fn epoch_command_matrix_matches_current_contract() {
    for command in [
        "open",
        "click",
        "exec",
        "back",
        "keys",
        "type",
        "switch",
        "close-tab",
        "hover",
        "upload",
        "select",
    ] {
        assert!(
            super::policy::command_increments_epoch(command),
            "{command} should increment epoch"
        );
    }

    for command in [
        "state",
        "screenshot",
        "doctor",
        "sessions",
        "tabs",
        "wait",
        "scroll",
        "fill",
        "pipe",
        "get-text",
        "bbox",
        "cookies",
    ] {
        assert!(
            !super::policy::command_increments_epoch(command),
            "{command} should not increment epoch"
        );
    }
}

#[test]
fn fill_and_pipe_publish_current_dom_epoch_without_incrementing() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-epoch"),
        None,
    ));
    let current_epoch = state.increment_epoch();

    assert_eq!(
        super::policy::response_dom_epoch(
            "fill",
            &serde_json::json!({}),
            &state,
            PendingExternalDomCommit::Clear,
        ),
        Some(current_epoch)
    );
    assert_eq!(state.current_epoch(), current_epoch);

    assert_eq!(
        super::policy::response_dom_epoch(
            "pipe",
            &serde_json::json!({}),
            &state,
            PendingExternalDomCommit::Clear,
        ),
        Some(current_epoch)
    );
    assert_eq!(state.current_epoch(), current_epoch);
}

#[test]
fn dialog_accept_and_dismiss_commit_new_dom_epoch() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-dialog-epoch"),
        None,
    ));
    let base_epoch = state.current_epoch();

    assert_eq!(
        super::policy::response_dom_epoch(
            "dialog",
            &serde_json::json!({ "sub": "accept" }),
            &state,
            PendingExternalDomCommit::Clear,
        ),
        Some(base_epoch + 1)
    );
    assert_eq!(state.current_epoch(), base_epoch + 1);

    assert_eq!(
        super::policy::response_dom_epoch(
            "dialog",
            &serde_json::json!({ "sub": "dismiss" }),
            &state,
            PendingExternalDomCommit::Clear,
        ),
        Some(base_epoch + 2)
    );
    assert_eq!(state.current_epoch(), base_epoch + 2);

    assert_eq!(
        super::policy::response_dom_epoch(
            "dialog",
            &serde_json::json!({ "sub": "status" }),
            &state,
            PendingExternalDomCommit::Clear,
        ),
        None
    );
    assert_eq!(state.current_epoch(), base_epoch + 2);
}

#[test]
fn incrementing_epoch_can_preserve_pending_external_dom_marker() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-preserve-pending"),
        None,
    ));
    state.mark_pending_external_dom_change();

    let epoch = super::policy::response_dom_epoch(
        "open",
        &serde_json::json!({}),
        &state,
        PendingExternalDomCommit::Preserve,
    );

    assert_eq!(epoch, Some(1));
    assert_eq!(state.current_epoch(), 1);
    assert!(state.has_pending_external_dom_change());
}

#[test]
fn download_save_owns_its_authoritative_timeout_at_router_boundary() {
    let request = IpcRequest::new(
        "download",
        serde_json::json!({
            "sub": "save",
            "file": "/tmp/assets.json",
            "output_dir": "/tmp/out",
        }),
        30_000,
    );
    assert!(request_owns_authoritative_timeout(&request));

    let wait_request = IpcRequest::new(
        "download",
        serde_json::json!({
            "sub": "wait",
        }),
        30_000,
    );
    assert!(!request_owns_authoritative_timeout(&wait_request));
}

#[test]
fn doctor_detection_risks_follow_structured_contract() {
    let launch_policy = LaunchPolicyInfo {
        headless: true,
        ignore_cert_errors: false,
        hide_infobars: true,
        user_data_dir: None,
        connection_target: None,
        stealth_level: Some("L1".to_string()),
        stealth_patches: Some(vec!["webdriver_undefined".to_string()]),
        stealth_default_enabled: Some(true),
        humanize_enabled: Some(false),
        humanize_speed: Some("normal".to_string()),
        stealth_coverage: Some(rub_core::model::StealthCoverageInfo {
            coverage_mode: Some("page_frame_only".to_string()),
            page_hook_installations: Some(1),
            page_hook_failures: Some(0),
            iframe_targets_detected: Some(0),
            worker_targets_detected: Some(0),
            service_worker_targets_detected: Some(0),
            shared_worker_targets_detected: Some(0),
            user_agent_override: Some(true),
            user_agent_metadata_override: Some(true),
            observed_target_types: vec!["page".to_string()],
            self_probe: None,
        }),
    };

    let risks = detection_risks(&launch_policy);
    assert_eq!(risks.len(), 2);
    assert_eq!(risks[0].risk, "headless_mode");
    assert_eq!(risks[0].severity, "medium");
    assert_eq!(risks[1].risk, "no_user_data_dir");
}

#[test]
fn doctor_detection_risks_report_self_probe_failures() {
    let launch_policy = LaunchPolicyInfo {
        headless: true,
        ignore_cert_errors: false,
        hide_infobars: true,
        user_data_dir: Some("/tmp/profile".to_string()),
        connection_target: None,
        stealth_level: Some("L1".to_string()),
        stealth_patches: Some(vec!["webdriver_undefined".to_string()]),
        stealth_default_enabled: Some(true),
        humanize_enabled: Some(false),
        humanize_speed: Some("normal".to_string()),
        stealth_coverage: Some(rub_core::model::StealthCoverageInfo {
            coverage_mode: Some("page_frame_worker_bridge".to_string()),
            page_hook_installations: Some(1),
            page_hook_failures: Some(0),
            iframe_targets_detected: Some(1),
            worker_targets_detected: Some(1),
            service_worker_targets_detected: Some(0),
            shared_worker_targets_detected: Some(0),
            user_agent_override: Some(true),
            user_agent_metadata_override: Some(true),
            observed_target_types: vec![
                "page".to_string(),
                "iframe".to_string(),
                "worker".to_string(),
            ],
            self_probe: Some(IdentitySelfProbeInfo {
                page_main_world: Some(IdentityProbeStatus::Passed),
                iframe_context: Some(IdentityProbeStatus::Failed),
                worker_context: Some(IdentityProbeStatus::Unknown),
                ua_consistency: Some(IdentityProbeStatus::Failed),
                webgl_surface: Some(IdentityProbeStatus::Failed),
                canvas_surface: Some(IdentityProbeStatus::Unknown),
                audio_surface: Some(IdentityProbeStatus::Failed),
                permissions_surface: Some(IdentityProbeStatus::Failed),
                viewport_surface: Some(IdentityProbeStatus::Failed),
                touch_surface: Some(IdentityProbeStatus::Unknown),
                window_metrics_surface: Some(IdentityProbeStatus::Failed),
                unsupported_surfaces: vec!["service_worker".to_string()],
            }),
        }),
    };

    let risks = detection_risks(&launch_policy);
    let risk_names: Vec<_> = risks.iter().map(|risk| risk.risk).collect();
    assert!(risk_names.contains(&"headless_mode"));
    assert!(risk_names.contains(&"iframe_context_unverified"));
    assert!(risk_names.contains(&"worker_context_unverified"));
    assert!(risk_names.contains(&"ua_consistency_unverified"));
    assert!(risk_names.contains(&"webgl_surface_unverified"));
    assert!(risk_names.contains(&"canvas_surface_unverified"));
    assert!(risk_names.contains(&"audio_surface_unverified"));
    assert!(risk_names.contains(&"permissions_surface_unverified"));
    assert!(risk_names.contains(&"viewport_surface_unverified"));
    assert!(risk_names.contains(&"touch_surface_unverified"));
    assert!(risk_names.contains(&"window_metrics_surface_unverified"));
}

#[test]
fn interaction_projection_preserves_confirmation_contract() {
    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::ToggleState,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::ToggleState),
            details: Some(serde_json::json!({ "after_checked": true })),
        }),
    };
    let mut value = serde_json::json!({ "index": 1 });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &[],
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(value["interaction"]["semantic_class"], "toggle_state");
    assert_eq!(value["interaction"]["element_verified"], true);
    assert_eq!(value["interaction"]["actuation"], "pointer");
    assert_eq!(value["interaction"]["interaction_confirmed"], true);
    assert_eq!(value["interaction"]["confirmation_status"], "confirmed");
    assert_eq!(value["interaction"]["confirmation_kind"], "toggle_state");
    assert_eq!(
        value["interaction"]["confirmation_details"]["after_checked"],
        true
    );
}

#[test]
fn select_projection_preserves_confirmation_contract() {
    let outcome = SelectOutcome {
        semantic_class: InteractionSemanticClass::SelectChoice,
        element_verified: false,
        selected_value: "2".to_string(),
        selected_text: "Two".to_string(),
        actuation: Some(InteractionActuation::Programmatic),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::SelectionApplied),
            details: Some(serde_json::json!({ "selected_value": "2" })),
        }),
    };
    let mut value = serde_json::json!({
        "result": {
            "value": "2",
            "text": "Two"
        }
    });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_select_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &[],
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(value["interaction"]["semantic_class"], "select_choice");
    assert_eq!(value["interaction"]["element_verified"], false);
    assert_eq!(value["interaction"]["actuation"], "programmatic");
    assert_eq!(value["interaction"]["interaction_confirmed"], true);
    assert_eq!(value["interaction"]["confirmation_status"], "confirmed");
    assert_eq!(
        value["interaction"]["confirmation_kind"],
        "selection_applied"
    );
    assert_eq!(
        value["interaction"]["confirmation_details"]["selected_value"],
        "2"
    );
    assert_eq!(value["result"]["value"], "2");
    assert_eq!(value["result"]["text"], "Two");
}

#[test]
fn interaction_projection_attaches_runtime_state_delta() {
    use rub_core::model::{
        AuthState, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
        RuntimeStateSnapshot, StateInspectorInfo, StateInspectorStatus,
    };

    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::PageMutation),
            details: Some(serde_json::json!({ "context_changed": false })),
        }),
    };
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

    let mut value = serde_json::json!({ "index": 1 });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: Some(&before),
            runtime_after: Some(&after),
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &[],
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(
        value["interaction"]["runtime_state_delta"]["changed"],
        serde_json::json!([
            "state_inspector.auth_state",
            "state_inspector.local_storage_keys",
            "state_inspector.auth_signals",
            "readiness_state.route_stability",
            "readiness_state.loading_present",
            "readiness_state.blocking_signals"
        ])
    );
}

#[test]
fn interaction_projection_attaches_context_turnover() {
    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Degraded,
            kind: Some(InteractionConfirmationKind::PageMutation),
            details: Some(serde_json::json!({
                "context_changed": true,
                "before_page": {
                    "url": "https://example.com/a",
                    "title": "A",
                    "context_replaced": false
                },
                "after_page": {
                    "url": "https://example.com/b",
                    "title": "B",
                    "context_replaced": true
                }
            })),
        }),
    };

    let mut value = serde_json::json!({ "index": 1 });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &[],
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(
        value["interaction"]["context_turnover"]["context_changed"],
        true
    );
    assert_eq!(
        value["interaction"]["context_turnover"]["context_replaced"],
        true
    );
    assert_eq!(
        value["interaction"]["context_turnover"]["before_page"]["url"],
        "https://example.com/a"
    );
    assert_eq!(
        value["interaction"]["context_turnover"]["after_page"]["url"],
        "https://example.com/b"
    );
}

#[test]
fn interaction_projection_attaches_runtime_observatory_events() {
    use rub_core::model::{
        ConsoleErrorEvent, InteractionActuation, InteractionConfirmation,
        InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
        InteractionSemanticClass, RuntimeObservatoryEvent, RuntimeObservatoryEventPayload,
    };

    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::PageMutation),
            details: None,
        }),
    };
    let events = vec![RuntimeObservatoryEvent {
        sequence: 7,
        payload: RuntimeObservatoryEventPayload::ConsoleError(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "boom".to_string(),
            source: Some("main".to_string()),
        }),
    }];

    let mut value = serde_json::json!({ "index": 1 });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &events,
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &[],
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(
        value["interaction"]["runtime_observatory_events"][0]["kind"],
        "console_error"
    );
    assert_eq!(
        value["interaction"]["runtime_observatory_events"][0]["sequence"],
        7
    );
    assert_eq!(
        value["interaction"]["runtime_observatory_events"][0]["event"]["message"],
        "boom"
    );
}

#[test]
fn interaction_projection_attaches_network_request_grouping() {
    use rub_core::model::{
        InteractionActuation, InteractionConfirmation, InteractionConfirmationKind,
        InteractionConfirmationStatus, InteractionOutcome, InteractionSemanticClass,
        NetworkRequestLifecycle, NetworkRequestRecord,
    };
    use std::collections::BTreeMap;

    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::PageMutation),
            details: None,
        }),
    };
    let requests = vec![
        NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 12,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api/orders".to_string(),
            method: "POST".to_string(),
            tab_target_id: None,
            status: Some(200),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: Some("xhr".to_string()),
            mime_type: Some("application/json".to_string()),
        },
        NetworkRequestRecord {
            request_id: "req-2".to_string(),
            sequence: 13,
            lifecycle: NetworkRequestLifecycle::Failed,
            url: "https://example.com/api/error".to_string(),
            method: "GET".to_string(),
            tab_target_id: None,
            status: Some(500),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: Some("HTTP 500".to_string()),
            frame_id: None,
            resource_type: Some("fetch".to_string()),
            mime_type: Some("application/json".to_string()),
        },
    ];

    let mut value = serde_json::json!({ "index": 1 });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &requests,
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(
        value["interaction"]["network_requests"]["requests"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2
    );
    assert_eq!(
        value["interaction"]["network_requests"]["terminal_count"],
        2
    );
    assert_eq!(
        value["interaction"]["network_requests"]["last_request"]["request_id"],
        "req-2"
    );
    assert_eq!(
        value["interaction"]["network_requests"]["requests"][0]["request_id"],
        "req-1"
    );
}

#[test]
fn interaction_projection_marks_confirmed_follow_up_activity_for_page_mutation_with_terminal_network()
 {
    use rub_core::model::{
        InteractionActuation, InteractionConfirmation, InteractionConfirmationKind,
        InteractionConfirmationStatus, InteractionOutcome, InteractionSemanticClass,
        NetworkRequestLifecycle, NetworkRequestRecord,
    };
    use std::collections::BTreeMap;

    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::PageMutation),
            details: Some(serde_json::json!({
                "before": {
                    "url": "https://example.test/signup",
                    "title": "Signup",
                },
                "after": {
                    "url": "https://example.test/signup",
                    "title": "Signup",
                }
            })),
        }),
    };
    let requests = vec![NetworkRequestRecord {
        request_id: "req-1".to_string(),
        sequence: 12,
        lifecycle: NetworkRequestLifecycle::Completed,
        url: "https://example.test/api/signup".to_string(),
        method: "POST".to_string(),
        tab_target_id: None,
        status: Some(202),
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_body: None,
        response_body: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        frame_id: None,
        resource_type: Some("xhr".to_string()),
        mime_type: Some("application/json".to_string()),
    }];

    let mut value = serde_json::json!({
        "result": {
            "gesture": "single"
        }
    });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &requests,
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(
        value["result"]["outcome_summary"]["class"],
        "confirmed_follow_up_activity"
    );
    assert_eq!(value["result"]["outcome_summary"]["authoritative"], true);
    assert_eq!(
        value["result"]["outcome_summary"]["activity"]["surface"],
        "network_requests"
    );
    assert_eq!(
        value["result"]["outcome_summary"]["activity"]["terminal_request_count"],
        1
    );
    assert_eq!(
        value["result"]["outcome_summary"]["activity"]["last_request"]["request_id"],
        "req-1"
    );
    assert_eq!(
        value["result"]["outcome_summary"]["activity"]["last_request"]["method"],
        "POST"
    );
    assert_eq!(
        value["result"]["outcome_summary"]["activity"]["last_request"]["url"],
        "https://example.test/api/signup"
    );
    assert_eq!(
        value["result"]["outcome_summary"]["activity"]["last_request"]["status"],
        202
    );
}

#[test]
fn interaction_projection_omits_follow_up_activity_when_network_surface_is_not_authoritative() {
    use rub_core::model::{
        InteractionActuation, InteractionConfirmation, InteractionConfirmationKind,
        InteractionConfirmationStatus, InteractionOutcome, InteractionSemanticClass,
        NetworkRequestLifecycle, NetworkRequestRecord,
    };
    use std::collections::BTreeMap;

    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::PageMutation),
            details: Some(serde_json::json!({})),
        }),
    };
    let requests = vec![NetworkRequestRecord {
        request_id: "req-1".to_string(),
        sequence: 12,
        lifecycle: NetworkRequestLifecycle::Completed,
        url: "https://example.test/api/signup".to_string(),
        method: "POST".to_string(),
        tab_target_id: None,
        status: Some(202),
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_body: None,
        response_body: None,
        original_url: None,
        rewritten_url: None,
        applied_rule_effects: Vec::new(),
        error_text: None,
        frame_id: None,
        resource_type: Some("xhr".to_string()),
        mime_type: Some("application/json".to_string()),
    }];

    let mut value = serde_json::json!({
        "result": {
            "gesture": "single"
        }
    });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &[],
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &requests,
            network_authoritative: false,
            network_degraded_reason: Some("overflow"),
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert!(value["result"].get("outcome_summary").is_none(), "{value}");
}

#[test]
fn interaction_projection_attaches_observed_effects() {
    use rub_core::model::{
        ConsoleErrorEvent, InteractionActuation, InteractionConfirmation,
        InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
        InteractionSemanticClass, RuntimeObservatoryEvent, RuntimeObservatoryEventPayload,
    };

    let outcome = InteractionOutcome {
        semantic_class: InteractionSemanticClass::Activate,
        element_verified: true,
        actuation: Some(InteractionActuation::Pointer),
        confirmation: Some(InteractionConfirmation {
            status: InteractionConfirmationStatus::Confirmed,
            kind: Some(InteractionConfirmationKind::FocusChange),
            details: Some(serde_json::json!({
                "before_active": false,
                "after_active": true,
            })),
        }),
    };
    let events = vec![RuntimeObservatoryEvent {
        sequence: 9,
        payload: RuntimeObservatoryEventPayload::ConsoleError(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "focused".to_string(),
            source: Some("main".to_string()),
        }),
    }];

    let mut value = serde_json::json!({ "index": 1 });
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    attach_interaction_projection(
        &mut value,
        &outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &frame_runtime,
            runtime_before: None,
            runtime_after: None,
            interference_before: None,
            interference_after: None,
            observatory_events: &events,
            observatory_authoritative: true,
            observatory_degraded_reason: None,
            network_requests: &[],
            network_authoritative: true,
            network_degraded_reason: None,
            download_events: &[],
            download_authoritative: true,
            download_degraded_reason: None,
        },
    );

    assert_eq!(
        value["interaction"]["observed_effects"]["before_active"],
        false
    );
    assert_eq!(
        value["interaction"]["observed_effects"]["after_active"],
        true
    );
    assert_eq!(
        value["interaction"]["runtime_observatory_events"][0]["sequence"],
        9
    );
}

#[test]
fn timeout_context_reports_phase_and_budget() {
    let context = timeout_context("exec", TimeoutPhase::Execution, 1000, 120, Some(880));
    assert_eq!(context["command"], "exec");
    assert_eq!(context["phase"], "execution");
    assert_eq!(context["transaction_timeout_ms"], 1000);
    assert_eq!(context["queue_ms"], 120);
    assert_eq!(context["exec_budget_ms"], 880);
}

#[test]
fn wait_timeout_context_merges_probe_and_execution_attribution() {
    let err = wait_timeout_error(
        &serde_json::json!({ "selector": ".ready" }),
        5_000,
        250,
        Some(4_750),
    );
    let RubError::Domain(envelope) = augment_wait_timeout_error(
        err,
        &serde_json::json!({ "selector": ".ready" }),
        5_000,
        250,
    ) else {
        panic!("expected domain error");
    };

    assert_eq!(envelope.code, ErrorCode::WaitTimeout);
    let context = envelope
        .context
        .expect("wait timeout should include context");
    assert_eq!(context["command"], "wait");
    assert_eq!(context["phase"], "execution");
    assert_eq!(context["kind"], "selector");
    assert_eq!(context["value"], ".ready");
    assert_eq!(context["transaction_timeout_ms"], 5_000);
    assert_eq!(context["queue_ms"], 250);
    assert_eq!(context["exec_budget_ms"], 4_750);
}

#[test]
fn handoff_allowlist_blocks_mutating_commands_but_keeps_runtime_surfaces_reachable() {
    assert!(!command_allowed_during_handoff("click"));
    assert!(!command_allowed_during_handoff("exec"));
    assert!(command_allowed_during_handoff("doctor"));
    assert!(command_allowed_during_handoff("runtime"));
    assert!(command_allowed_during_handoff("handoff"));
    assert!(command_allowed_during_handoff("takeover"));
    assert!(command_allowed_during_handoff("state"));
    assert!(command_allowed_during_handoff("tabs"));
}

#[tokio::test]
async fn finalize_response_releases_replay_owner_without_caching_early_queue_errors() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
        .with_command_id("cmd-queue")
        .expect("static command_id must be valid");
    let replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-1",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should claim replay owner")
    .expect("replay owner should be present");

    let queue_timeout = IpcResponse::error(
        "req-1",
        rub_core::error::ErrorEnvelope::new(
            ErrorCode::IpcTimeout,
            "Command timed out waiting in queue after 250ms",
        ),
    );
    let finalized =
        finalize_response(&request, queue_timeout, false, Some(replay_owner), &state).await;

    assert_eq!(finalized.command_id.as_deref(), Some("cmd-queue"));
    let replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-2",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("replay fence should be reclaimable after early finalize")
    .expect("replay owner should be present");
    assert_eq!(replay_owner.command_id, "cmd-queue");
}

#[tokio::test]
async fn prepare_replay_fence_rejects_conflicting_request_fingerprint() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let first = IpcRequest::new("doctor", serde_json::json!({ "verbose": true }), 250)
        .with_command_id("cmd-conflict")
        .expect("static command_id must be valid");
    let conflicting = IpcRequest::new("doctor", serde_json::json!({ "verbose": false }), 250)
        .with_command_id("cmd-conflict")
        .expect("static command_id must be valid");

    let _owner = prepare_replay_fence(
        &first,
        &state,
        "req-1",
        TransactionDeadline::new(first.timeout_ms),
    )
    .await
    .expect("first request should become replay owner")
    .expect("replay owner should be present");

    let conflict = prepare_replay_fence(
        &conflicting,
        &state,
        "req-2",
        TransactionDeadline::new(conflicting.timeout_ms),
    )
    .await
    .expect_err("conflicting replay fingerprint should fail");
    assert_eq!(
        conflict.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
    assert_eq!(conflict.command_id.as_deref(), Some("cmd-conflict"));
}

#[tokio::test]
async fn prepare_replay_fence_uses_remaining_transaction_budget() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let request = IpcRequest::new("doctor", serde_json::json!({}), 10)
        .with_command_id("cmd-budget")
        .expect("static command_id must be valid");
    let _owner = prepare_replay_fence(
        &request,
        &state,
        "req-1",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should become replay owner");

    let deadline = TransactionDeadline::new(request.timeout_ms);
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;

    let timeout = prepare_replay_fence(&request, &state, "req-2", deadline)
        .await
        .expect_err("expired deadline should fail before replay wait");
    assert_eq!(
        timeout.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcTimeout)
    );
    assert_eq!(timeout.command_id.as_deref(), Some("cmd-budget"));
    assert_eq!(
        timeout
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|ctx| ctx["phase"].as_str()),
        Some("replay_fence")
    );
}

#[tokio::test]
async fn replay_wait_without_cached_response_reclaims_released_owner() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
        .with_command_id("cmd-missing")
        .expect("static command_id must be valid");

    let _owner = prepare_replay_fence(
        &request,
        &state,
        "req-1",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should become replay owner")
    .expect("replay owner should be present");

    let waiting_state = state.clone();
    let waiting_request = request.clone();
    let waiter = tokio::spawn(async move {
        prepare_replay_fence(
            &waiting_request,
            &waiting_state,
            "req-2",
            TransactionDeadline::new(waiting_request.timeout_ms),
        )
        .await
        .expect("released replay fence without cache should let waiter claim ownership")
    });

    tokio::task::yield_now().await;
    state.release_replay_command("cmd-missing");

    let replay_owner = waiter
        .await
        .expect("waiter should complete")
        .expect("released replay fence without cache should let waiter reclaim ownership");
    assert_eq!(replay_owner.command_id, "cmd-missing");

    match state.claim_replay_command("cmd-missing", replay_request_fingerprint(&request)) {
        ReplayCommandClaim::Wait(_) => {}
        other => panic!("reclaimed replay fence should now be owned in-flight, got {other:?}"),
    }
}

#[tokio::test]
async fn finalize_response_preserves_command_id_for_protocol_version_mismatch() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let request = IpcRequest::new("doctor", serde_json::json!({}), 250)
        .with_command_id("cmd-version")
        .expect("static command_id must be valid");
    let response = protocol_version_mismatch_response(
        "req-1",
        &IpcRequest {
            ipc_protocol_version: "0.0.0".to_string(),
            ..request.clone()
        },
    );
    let finalized = finalize_response(&request, response, false, None, &state).await;
    assert_eq!(finalized.command_id.as_deref(), Some("cmd-version"));
    assert_eq!(
        finalized.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcVersionMismatch)
    );
}

#[tokio::test]
async fn finalize_response_turns_oversized_payload_into_structured_domain_error() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let request = IpcRequest::new("state", serde_json::json!({}), 250)
        .with_command_id("cmd-large")
        .expect("static command_id must be valid");
    let oversized = IpcResponse::success(
        "req-large",
        serde_json::json!({
            "payload": "x".repeat(MAX_FRAME_BYTES),
        }),
    );

    let finalized = finalize_response(&request, oversized, false, None, &state).await;

    assert_eq!(finalized.command_id.as_deref(), Some("cmd-large"));
    assert_eq!(
        finalized.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
    assert_eq!(finalized.status, rub_ipc::protocol::ResponseStatus::Error);
    let error_context = finalized
        .error
        .as_ref()
        .and_then(|error| error.context.as_ref())
        .expect("frame limit rejection should preserve error context");
    assert_eq!(
        error_context
            .get("reason")
            .and_then(serde_json::Value::as_str),
        Some("response_exceeds_ipc_frame_limit")
    );
}

#[tokio::test]
async fn replay_cache_commits_oversized_response_in_its_final_on_wire_shape() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let request = IpcRequest::new("state", serde_json::json!({}), 250)
        .with_command_id("cmd-large-replay")
        .expect("static command_id must be valid");
    let mut replay_owner = prepare_replay_fence(
        &request,
        &state,
        "req-1",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect("first request should claim replay owner")
    .expect("replay owner should be present");
    replay_owner.mark_execution_started();

    let oversized = IpcResponse::success(
        "req-large",
        serde_json::json!({
            "payload": "x".repeat(MAX_FRAME_BYTES),
        }),
    );

    let finalized = finalize_response(&request, oversized, false, Some(replay_owner), &state).await;
    assert_eq!(finalized.command_id.as_deref(), Some("cmd-large-replay"));
    assert_eq!(
        finalized.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );

    let replay = prepare_replay_fence(
        &request,
        &state,
        "req-2",
        TransactionDeadline::new(request.timeout_ms),
    )
    .await
    .expect_err("replay should return cached finalized response");
    assert_eq!(replay.command_id.as_deref(), Some("cmd-large-replay"));
    assert_eq!(
        replay.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
    assert_eq!(
        replay
            .error
            .as_ref()
            .and_then(|error| error.context.as_ref())
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("response_exceeds_ipc_frame_limit")
    );
    assert!(
        replay.data.is_none(),
        "replay cache should not preserve the oversized pre-fence success payload"
    );
}

#[tokio::test]
async fn unknown_command_is_classified_as_invalid_input() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let result = super::dispatch::dispatch_named_command(
        &test_router(),
        "definitely-not-a-command",
        &serde_json::json!({}),
        TransactionDeadline::new(1_000),
        &state,
    )
    .await
    .expect_err("unknown command should fail");
    let envelope = result.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert!(envelope.message.contains("Unknown command"));
}

#[tokio::test]
async fn handshake_bypasses_fifo_when_router_is_busy() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-test"),
        None,
    ));
    let router = test_router();
    let _permit: tokio::sync::SemaphorePermit<'_> = router
        .exec_semaphore
        .acquire()
        .await
        .expect("test should hold fifo permit");

    let response = router
        .dispatch(
            IpcRequest::new("_handshake", serde_json::json!({}), 50),
            &state,
        )
        .await;
    assert_eq!(response.status, rub_ipc::protocol::ResponseStatus::Success);
    assert_eq!(
        response
            .data
            .as_ref()
            .and_then(|data| data["ipc_protocol_version"].as_str()),
        Some(rub_ipc::protocol::IPC_PROTOCOL_VERSION)
    );
}

#[tokio::test]
async fn orchestration_target_dispatch_stays_out_of_user_post_commit_projection() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-router-internal"),
        None,
    ));
    let router = test_router();

    let response = router
        .dispatch(
            IpcRequest::new("_orchestration_target_dispatch", serde_json::json!({}), 50),
            &state,
        )
        .await;
    assert_eq!(response.status, rub_ipc::protocol::ResponseStatus::Error);
    assert_eq!(state.pending_post_commit_projection_count(), 0);
    assert!(
        state.command_history(5).await.entries.is_empty(),
        "internal orchestration transport must not leak into user history"
    );
    assert!(
        state.workflow_capture(5).await.entries.is_empty(),
        "internal orchestration transport must not leak into workflow capture"
    );
}
