use super::{
    DocumentFenceProbe, FRAME_SCOPED_COOKIE_AUTHORITY_UNAVAILABLE_REASON,
    FRAME_SCOPED_COOKIE_AUTHORITY_UNAVAILABLE_SIGNAL, StorageProbe, append_degraded_reason,
    build_state_inspector_info, degrade_runtime_snapshot_for_document_fence,
    document_fence_is_authoritative, ensure_live_read_document_fence,
    frame_context_unavailable_snapshot, infer_auth_signals, infer_auth_state,
    infer_blocking_signals, normalize_document_ready_state, parse_document_fence_probe_json,
    parse_overlay_state, parse_readiness_probe_json, parse_route_stability,
    parse_storage_probe_json, runtime_document_fence_failure_reason, runtime_snapshot_frame_scope,
};
use crate::frame_runtime::ResolvedFrameContext;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    AuthState, FrameContextInfo, OverlayState, ReadinessStatus, RouteStability,
    StateInspectorStatus,
};

#[test]
fn malformed_storage_probe_result_is_rejected() {
    let error = parse_storage_probe_json("not-json").expect_err("malformed payload should fail");
    assert_eq!(error, "storage_probe_malformed");
}

#[test]
fn malformed_readiness_probe_result_is_rejected() {
    let error = parse_readiness_probe_json("{\"overlay_state\":")
        .expect_err("malformed payload should fail");
    assert_eq!(error, "probe_malformed");
}

#[test]
fn malformed_document_fence_probe_result_is_rejected() {
    let error =
        parse_document_fence_probe_json("not-json").expect_err("malformed payload should fail");
    assert_eq!(error, "document_fence_probe_malformed");
}

#[test]
fn infer_auth_state_is_conservative() {
    assert_eq!(
        infer_auth_state(0, &[], &[], true, true),
        AuthState::Anonymous
    );
    assert_eq!(
        infer_auth_state(1, &Vec::new(), &Vec::new(), true, true),
        AuthState::Unknown
    );
    assert_eq!(
        infer_auth_state(0, &["token".to_string()], &Vec::new(), true, true),
        AuthState::Unknown
    );
    assert_eq!(
        infer_auth_state(0, &Vec::new(), &Vec::new(), false, true),
        AuthState::Unknown
    );
    assert_eq!(
        infer_auth_state(0, &Vec::new(), &Vec::new(), true, false),
        AuthState::Unknown
    );
}

#[test]
fn auth_signals_capture_storage_and_cookie_evidence() {
    assert!(infer_auth_signals(0, &[], &[]).is_empty());
    assert_eq!(
        infer_auth_signals(2, &["token".to_string()], &["csrf".to_string()]),
        vec![
            "cookies_present".to_string(),
            "local_storage_present".to_string(),
            "session_storage_present".to_string(),
            "auth_like_storage_key_present".to_string(),
        ]
    );
}

#[test]
fn degraded_reason_joins_multiple_probe_failures() {
    let reasons = [
        "cookie_query_failed".to_string(),
        "storage_probe_failed".to_string(),
    ];
    assert_eq!(
        reasons.join(","),
        "cookie_query_failed,storage_probe_failed"
    );
}

#[test]
fn document_fence_requires_href_and_time_origin() {
    assert!(!document_fence_is_authoritative(
        &DocumentFenceProbe::default()
    ));
    assert!(document_fence_is_authoritative(&DocumentFenceProbe {
        href: "https://example.test/".to_string(),
        time_origin: Some(1.0),
    }));
}

#[test]
fn document_fence_change_marks_runtime_probe_invalid() {
    let before = DocumentFenceProbe {
        href: "https://example.test/a".to_string(),
        time_origin: Some(1.0),
    };
    let after = DocumentFenceProbe {
        href: "https://example.test/b".to_string(),
        time_origin: Some(1.0),
    };
    assert_eq!(
        runtime_document_fence_failure_reason(Some(&before), Some(&after)),
        Some("document_changed_during_runtime_probe")
    );
    assert_eq!(
        runtime_document_fence_failure_reason(None, Some(&after)),
        Some("document_fence_unavailable")
    );
}

#[test]
fn live_read_document_fence_fails_closed_on_document_drift() {
    let before = DocumentFenceProbe {
        href: "https://example.test/a".to_string(),
        time_origin: Some(1.0),
    };
    let after = DocumentFenceProbe {
        href: "https://example.test/b".to_string(),
        time_origin: Some(1.0),
    };

    let envelope =
        ensure_live_read_document_fence("text", "main-frame", Some(&before), Some(&after))
            .expect_err("live read must not publish mixed-document output")
            .into_envelope();

    assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
    assert_eq!(
        envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
        Some(&serde_json::json!("document_changed_during_live_read"))
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("operation")),
        Some(&serde_json::json!("text"))
    );
}

#[test]
fn degraded_reason_append_is_deduplicated() {
    assert_eq!(
        append_degraded_reason(Some("probe_timeout".to_string()), "probe_timeout"),
        Some("probe_timeout".to_string())
    );
    assert_eq!(
        append_degraded_reason(
            Some("probe_timeout".to_string()),
            "document_fence_unavailable"
        ),
        Some("probe_timeout,document_fence_unavailable".to_string())
    );
}

#[test]
fn readiness_probe_parsers_accept_known_values() {
    assert_eq!(parse_route_stability("stable"), RouteStability::Stable);
    assert_eq!(
        parse_route_stability("transitioning"),
        RouteStability::Transitioning
    );
    assert_eq!(parse_route_stability("other"), RouteStability::Unknown);

    assert_eq!(
        parse_overlay_state("development"),
        OverlayState::Development
    );
    assert_eq!(parse_overlay_state("error"), OverlayState::Error);
    assert_eq!(
        parse_overlay_state("user_blocking"),
        OverlayState::UserBlocking
    );
    assert_eq!(parse_overlay_state("none"), OverlayState::None);
}

#[test]
fn readiness_details_capture_blocking_signals() {
    assert_eq!(
        normalize_document_ready_state("interactive"),
        Some("interactive".to_string())
    );
    assert_eq!(normalize_document_ready_state("other"), None);

    assert_eq!(
        infer_blocking_signals(
            "interactive",
            true,
            false,
            OverlayState::Development,
            RouteStability::Transitioning,
        ),
        vec![
            "document_ready_state:interactive".to_string(),
            "loading_present".to_string(),
            "overlay:development".to_string(),
            "route_transitioning".to_string(),
        ]
    );

    assert_eq!(
        infer_blocking_signals(
            "complete",
            false,
            false,
            OverlayState::UserBlocking,
            RouteStability::Stable,
        ),
        vec!["overlay:user_blocking".to_string()]
    );
}

#[test]
fn frame_context_unavailable_snapshot_uses_stable_internal_reason_shape() {
    let snapshot =
        frame_context_unavailable_snapshot(RubError::Internal("frame missing".to_string()));

    assert_eq!(
        snapshot.state_inspector.status,
        StateInspectorStatus::Degraded
    );
    assert_eq!(snapshot.readiness_state.status, ReadinessStatus::Degraded);
    assert_eq!(
        snapshot.state_inspector.degraded_reason.as_deref(),
        Some("frame_context_unavailable:internal_error")
    );
    assert_eq!(
        snapshot.readiness_state.degraded_reason.as_deref(),
        Some("frame_context_unavailable:internal_error")
    );
}

#[test]
fn frame_context_unavailable_snapshot_does_not_leak_internal_error_text() {
    let snapshot = frame_context_unavailable_snapshot(RubError::Internal(
        "Resolve frame execution context failed: socket closed".to_string(),
    ));

    assert_eq!(
        snapshot.state_inspector.degraded_reason.as_deref(),
        Some("frame_context_unavailable:internal_error")
    );
    assert_eq!(
        snapshot.readiness_state.degraded_reason.as_deref(),
        Some("frame_context_unavailable:internal_error")
    );
}

#[test]
fn frame_context_unavailable_snapshot_preserves_domain_reason_shape() {
    let snapshot = frame_context_unavailable_snapshot(RubError::domain_with_context(
        ErrorCode::BrowserCrashed,
        "frame missing",
        serde_json::json!({
            "reason": "current_frame_context_missing",
        }),
    ));

    assert_eq!(
        snapshot.state_inspector.degraded_reason.as_deref(),
        Some("frame_context_unavailable:BROWSER_CRASHED:current_frame_context_missing")
    );
    assert_eq!(
        snapshot.readiness_state.degraded_reason.as_deref(),
        Some("frame_context_unavailable:BROWSER_CRASHED:current_frame_context_missing")
    );
}

#[test]
fn frame_context_unavailable_snapshot_preserves_explicit_frame_inventory_reason() {
    let snapshot = frame_context_unavailable_snapshot(RubError::domain_with_context(
        ErrorCode::InvalidInput,
        "frame missing",
        serde_json::json!({
            "reason": "frame_inventory_missing",
            "frame_id": "child-1",
        }),
    ));

    assert_eq!(
        snapshot.state_inspector.degraded_reason.as_deref(),
        Some("frame_context_unavailable:INVALID_INPUT:frame_inventory_missing")
    );
    assert_eq!(
        snapshot.readiness_state.degraded_reason.as_deref(),
        Some("frame_context_unavailable:INVALID_INPUT:frame_inventory_missing")
    );
}

#[test]
fn frame_context_unavailable_snapshot_preserves_explicit_frame_execution_context_reason() {
    let snapshot = frame_context_unavailable_snapshot(RubError::domain_with_context(
        ErrorCode::InvalidInput,
        "frame has no execution context",
        serde_json::json!({
            "reason": "frame_execution_context_missing",
            "frame_id": "child-2",
        }),
    ));

    assert_eq!(
        snapshot.state_inspector.degraded_reason.as_deref(),
        Some("frame_context_unavailable:INVALID_INPUT:frame_execution_context_missing")
    );
    assert_eq!(
        snapshot.readiness_state.degraded_reason.as_deref(),
        Some("frame_context_unavailable:INVALID_INPUT:frame_execution_context_missing")
    );
}

#[test]
fn document_fence_degradation_scrubs_semantic_runtime_fields() {
    let mut state_inspector = super::StateInspectorInfo {
        status: StateInspectorStatus::Active,
        auth_state: AuthState::Authenticated,
        cookie_count: 3,
        local_storage_keys: vec!["token".to_string()],
        session_storage_keys: vec!["csrf".to_string()],
        auth_signals: vec!["cookies_present".to_string()],
        degraded_reason: Some("cookie_query_failed".to_string()),
    };
    let mut readiness_state = super::ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Stable,
        loading_present: true,
        skeleton_present: true,
        overlay_state: OverlayState::Development,
        document_ready_state: Some("interactive".to_string()),
        blocking_signals: vec!["loading_present".to_string()],
        degraded_reason: Some("probe_timeout".to_string()),
    };

    degrade_runtime_snapshot_for_document_fence(
        &mut state_inspector,
        &mut readiness_state,
        "document_fence_unavailable",
    );

    assert_eq!(state_inspector.status, StateInspectorStatus::Degraded);
    assert_eq!(state_inspector.auth_state, AuthState::Unknown);
    assert_eq!(state_inspector.cookie_count, 0);
    assert!(state_inspector.local_storage_keys.is_empty());
    assert!(state_inspector.session_storage_keys.is_empty());
    assert!(state_inspector.auth_signals.is_empty());
    assert_eq!(
        state_inspector.degraded_reason.as_deref(),
        Some("cookie_query_failed,document_fence_unavailable")
    );

    assert_eq!(readiness_state.status, ReadinessStatus::Degraded);
    assert_eq!(readiness_state.route_stability, RouteStability::Unknown);
    assert!(!readiness_state.loading_present);
    assert!(!readiness_state.skeleton_present);
    assert_eq!(readiness_state.overlay_state, OverlayState::None);
    assert_eq!(readiness_state.document_ready_state, None);
    assert!(readiness_state.blocking_signals.is_empty());
    assert_eq!(
        readiness_state.degraded_reason.as_deref(),
        Some("probe_timeout,document_fence_unavailable")
    );
}

#[test]
fn explicit_frame_state_inspector_omits_page_global_cookie_authority() {
    let snapshot = build_state_inspector_info(
        0,
        StorageProbe {
            local_storage_keys: vec!["token".to_string()],
            session_storage_keys: vec![],
        },
        Vec::new(),
        false,
        true,
        true,
    );

    assert_eq!(snapshot.status, StateInspectorStatus::Degraded);
    assert_eq!(snapshot.cookie_count, 0);
    assert!(
        snapshot
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason.contains(FRAME_SCOPED_COOKIE_AUTHORITY_UNAVAILABLE_REASON))
    );
    assert!(
        snapshot
            .auth_signals
            .contains(&FRAME_SCOPED_COOKIE_AUTHORITY_UNAVAILABLE_SIGNAL.to_string())
    );
    assert!(
        !snapshot
            .auth_signals
            .contains(&"cookies_present".to_string()),
        "frame-scoped auth signals must not present page-global cookies as frame-local evidence"
    );
    assert_eq!(
        snapshot.auth_state,
        AuthState::Unknown,
        "withheld page-global cookie authority must not collapse to anonymous"
    );
}

#[test]
fn explicit_primary_frame_runtime_snapshot_preserves_top_level_scope() {
    let primary_context = ResolvedFrameContext {
        frame: FrameContextInfo {
            frame_id: "frame-main".to_string(),
            name: Some("main".to_string()),
            parent_frame_id: None,
            target_id: Some("target-1".to_string()),
            url: Some("https://example.test".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        },
        lineage: vec!["frame-main".to_string()],
        execution_context_id: None,
        frame_scoped: false,
    };

    assert!(
        !runtime_snapshot_frame_scope(&primary_context),
        "resolved primary-frame context must preserve top-level runtime scope"
    );
}

#[test]
fn degraded_cookie_or_storage_authority_cannot_project_anonymous() {
    let cookie_degraded = build_state_inspector_info(
        0,
        StorageProbe::default(),
        vec!["cookie_query_failed".to_string()],
        false,
        true,
        false,
    );
    assert_eq!(cookie_degraded.auth_state, AuthState::Unknown);

    let storage_degraded = build_state_inspector_info(
        0,
        StorageProbe::default(),
        vec!["probe_timeout".to_string()],
        true,
        false,
        false,
    );
    assert_eq!(storage_degraded.auth_state, AuthState::Unknown);
}
