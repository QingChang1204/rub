use super::{
    FrameId, HistoryDirection, NavigateCommitKind, build_scroll_evaluate_params,
    classify_navigate_commit, committed_navigation_frame, history_boundary_from_history_state,
    history_navigation_deadline, optional_history_budget, parse_scroll_position_json,
    required_history_budget, wait_for_lifecycle_event_from_listener,
    wait_for_same_document_navigation_from_listener,
};
use chromiumoxide::cdp::browser_protocol::page::{
    EventNavigatedWithinDocument, NavigatedWithinDocumentNavigationType,
};
use rub_core::error::ErrorCode;
use std::sync::Arc;
use std::time::Duration;

fn frame_id(value: &str) -> FrameId {
    serde_json::from_value(serde_json::json!(value)).expect("frame id")
}

#[tokio::test]
async fn lifecycle_listener_ending_before_event_is_navigation_failed() {
    let mut listener = futures::stream::empty();
    let error = wait_for_lifecycle_event_from_listener(
        frame_id("main"),
        &mut listener,
        "networkIdle",
        Duration::from_millis(50),
    )
    .await
    .expect_err("listener EOF before lifecycle event should fail");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::NavigationFailed);
    assert!(envelope.message.contains("Lifecycle listener ended"));
}

#[tokio::test]
async fn lifecycle_wait_honors_caller_timeout_budget() {
    let mut listener = futures::stream::pending();
    let error = tokio::time::timeout(
        Duration::from_millis(150),
        wait_for_lifecycle_event_from_listener(
            frame_id("main"),
            &mut listener,
            "networkIdle",
            Duration::from_millis(30),
        ),
    )
    .await
    .expect("the wait fence should honor the caller timeout instead of sleeping on a hidden multi-second budget")
    .expect_err("pending listener should time out at the caller budget");
    assert_eq!(error.into_envelope().code, ErrorCode::PageLoadTimeout);
}

#[tokio::test]
async fn same_document_navigation_wait_succeeds_for_main_frame_commit() {
    let mut listener = futures::stream::iter(vec![Arc::new(EventNavigatedWithinDocument {
        frame_id: frame_id("main"),
        url: "https://example.com/page#section".to_string(),
        navigation_type: NavigatedWithinDocumentNavigationType::Fragment,
    })]);

    wait_for_same_document_navigation_from_listener(
        frame_id("main"),
        &mut listener,
        Duration::from_millis(50),
    )
    .await
    .expect("same-document commit should satisfy the fence");
}

#[tokio::test]
async fn same_document_navigation_wait_honors_caller_timeout_budget() {
    let mut listener = futures::stream::pending();
    let error = tokio::time::timeout(
        Duration::from_millis(150),
        wait_for_same_document_navigation_from_listener(
            frame_id("main"),
            &mut listener,
            Duration::from_millis(30),
        ),
    )
    .await
    .expect("the same-document fence should honor the caller timeout")
    .expect_err("pending same-document listener should time out at the caller budget");
    assert_eq!(error.into_envelope().code, ErrorCode::PageLoadTimeout);
}

#[test]
fn navigate_commit_classifies_protocol_result_exhaustively() {
    assert_eq!(
        classify_navigate_commit(None, true, true, "https://example.com/file.csv")
            .expect("download navigation should classify"),
        NavigateCommitKind::Download {
            warning: "Navigation to https://example.com/file.csv triggered a browser download; the active page remained on the current document".to_string(),
        }
    );
    assert_eq!(
        classify_navigate_commit(None, false, false, "https://example.com/page#section")
            .expect("same-document navigation should classify"),
        NavigateCommitKind::SameDocument
    );
    assert_eq!(
        classify_navigate_commit(None, false, true, "https://example.com")
            .expect("cross-document navigation should classify"),
        NavigateCommitKind::Lifecycle
    );

    let error = classify_navigate_commit(
        Some("net::ERR_NAME_NOT_RESOLVED"),
        false,
        true,
        "https://missing.invalid",
    )
    .expect_err("protocol error text should fail immediately");
    assert_eq!(error.into_envelope().code, ErrorCode::NavigationFailed);
}

#[test]
fn navigation_wait_uses_committed_response_frame_authority() {
    assert_eq!(
        committed_navigation_frame(frame_id("pre-main"), frame_id("committed-main")),
        frame_id("committed-main")
    );
}

#[test]
fn parse_scroll_position_json_rejects_invalid_probe_payload() {
    let error = parse_scroll_position_json("{".to_string()).expect_err("invalid json should fail");
    assert_eq!(error.into_envelope().code, ErrorCode::InternalError);
}

#[test]
fn scroll_evaluate_params_include_execution_context_when_frame_scoped() {
    let params = build_scroll_evaluate_params(
        "window.scrollBy(0, 80);".to_string(),
        Some(serde_json::from_value(serde_json::json!(7)).expect("execution context id")),
    );
    let value = serde_json::to_value(&params).expect("scroll params should serialize");
    assert_eq!(value["contextId"], serde_json::json!(7));
}

#[test]
fn scroll_evaluate_params_omit_execution_context_for_primary_frame() {
    let params = build_scroll_evaluate_params("window.scrollBy(0, 80);".to_string(), None);
    let value = serde_json::to_value(&params).expect("scroll params should serialize");
    assert!(
        value.get("contextId").is_none(),
        "primary-frame scroll should not force a context id"
    );
}

#[test]
fn history_boundary_from_history_state_tracks_back_edge() {
    assert_eq!(
        history_boundary_from_history_state(HistoryDirection::Back, 0, 3),
        Some(true)
    );
    assert_eq!(
        history_boundary_from_history_state(HistoryDirection::Back, 1, 3),
        Some(false)
    );
}

#[test]
fn history_boundary_from_history_state_tracks_forward_edge() {
    assert_eq!(
        history_boundary_from_history_state(HistoryDirection::Forward, 2, 3),
        Some(true)
    );
    assert_eq!(
        history_boundary_from_history_state(HistoryDirection::Forward, 1, 3),
        Some(false)
    );
}

#[tokio::test]
async fn history_navigation_required_budget_is_shared_across_phases() {
    let deadline =
        history_navigation_deadline(Duration::from_millis(100), "back").expect("deadline");

    tokio::time::sleep(Duration::from_millis(25)).await;
    let first = required_history_budget(deadline, "phase one").expect("remaining budget");
    assert!(first < Duration::from_millis(100));
    assert!(first > Duration::from_millis(40));

    tokio::time::sleep(Duration::from_millis(25)).await;
    let second = required_history_budget(deadline, "phase two").expect("remaining budget");
    assert!(second < first);
    assert!(second > Duration::from_millis(10));
}

#[tokio::test]
async fn history_navigation_required_budget_fails_closed_once_deadline_is_exhausted() {
    let deadline =
        history_navigation_deadline(Duration::from_millis(10), "forward").expect("deadline");

    tokio::time::sleep(Duration::from_millis(20)).await;

    let error =
        required_history_budget(deadline, "history phase exhausted").expect_err("budget exhausted");
    assert_eq!(error.into_envelope().code, ErrorCode::PageLoadTimeout);
    assert!(
        optional_history_budget(deadline).is_none(),
        "optional degraded probes must also stop once the shared deadline is exhausted"
    );
}
