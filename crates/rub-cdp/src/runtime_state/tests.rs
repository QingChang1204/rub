use super::{
    DocumentFenceProbe, FRAME_SCOPED_PAGE_GLOBAL_COOKIE_REASON,
    FRAME_SCOPED_PAGE_GLOBAL_COOKIE_SIGNAL, StorageProbe, append_degraded_reason,
    build_state_inspector_info, document_fence_is_authoritative,
    frame_context_unavailable_snapshot, infer_auth_signals, infer_auth_state,
    infer_blocking_signals, normalize_document_ready_state, parse_document_fence_probe_json,
    parse_overlay_state, parse_readiness_probe_json, parse_route_stability,
    parse_storage_probe_json, runtime_document_fence_failure_reason,
};
use rub_core::error::RubError;
use rub_core::model::{
    AuthState, OverlayState, ReadinessStatus, RouteStability, StateInspectorStatus,
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
    assert_eq!(infer_auth_state(0, &[], &[]), AuthState::Anonymous);
    assert_eq!(
        infer_auth_state(1, &Vec::new(), &Vec::new()),
        AuthState::Unknown
    );
    assert_eq!(
        infer_auth_state(0, &["token".to_string()], &Vec::new()),
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
fn frame_context_unavailable_snapshot_marks_both_surfaces_degraded() {
    let snapshot =
        frame_context_unavailable_snapshot(RubError::Internal("frame missing".to_string()));

    assert_eq!(
        snapshot.state_inspector.status,
        StateInspectorStatus::Degraded
    );
    assert_eq!(snapshot.readiness_state.status, ReadinessStatus::Degraded);
    assert!(
        snapshot
            .state_inspector
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("frame_context_unavailable"))
    );
    assert!(
        snapshot
            .readiness_state
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("frame_context_unavailable"))
    );
}

#[test]
fn explicit_frame_state_inspector_marks_cookie_authority_as_mixed_plane() {
    let snapshot = build_state_inspector_info(
        3,
        StorageProbe {
            local_storage_keys: vec!["token".to_string()],
            session_storage_keys: vec![],
        },
        Vec::new(),
        true,
    );

    assert_eq!(snapshot.status, StateInspectorStatus::Degraded);
    assert_eq!(snapshot.cookie_count, 3);
    assert!(
        snapshot
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason.contains(FRAME_SCOPED_PAGE_GLOBAL_COOKIE_REASON))
    );
    assert!(
        snapshot
            .auth_signals
            .contains(&FRAME_SCOPED_PAGE_GLOBAL_COOKIE_SIGNAL.to_string())
    );
    assert!(
        !snapshot
            .auth_signals
            .contains(&"cookies_present".to_string()),
        "frame-scoped auth signals must not present page-global cookies as frame-local evidence"
    );
}
