use super::args::{ClickArgs, TextEntryArgs, UploadArgs};
use super::projection::{
    InteractionObservationBaseline, InteractionObservationFence, capture_interaction_trace_windows,
    collect_post_interaction_projection, collect_stable_post_interaction_projection,
};
use crate::router::request_args::parse_json_args;
use crate::session::SessionState;
use rub_core::model::{
    ConsoleErrorEvent, FrameContextInfo, FrameContextStatus, FrameRuntimeInfo,
    InterferenceRuntimeInfo, InterferenceRuntimeStatus, NetworkRequestLifecycle,
    NetworkRequestRecord, ReadinessInfo, ReadinessStatus, RouteStability, RuntimeStateSnapshot,
    StateInspectorInfo, StateInspectorStatus,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn interaction_possible_commit_timeout_contract_redacts_raw_secret_bearing_args() {
    let contract = super::interaction_possible_commit_recovery_contract(
        "type",
        &serde_json::json!({
            "selector": "input.password",
            "text": "typed-secret",
            "keys": "Meta+K",
            "value": "secret-option",
            "path": "/tmp/private/upload.txt",
            "label": "sensitive label",
            "target_text": "sensitive text",
            "wait_after": {"text": "done"},
        }),
    );
    let serialized = serde_json::to_string(&contract).expect("serialize contract");

    assert_eq!(contract["kind"], "interaction_possible_commit");
    assert_eq!(contract["command"], "type");
    assert_eq!(contract["request"]["locator"]["selector"], "input.password");
    assert_eq!(contract["request"]["arguments_redacted"], true);
    assert!(
        !serialized.contains("typed-secret")
            && !serialized.contains("Meta+K")
            && !serialized.contains("secret-option")
            && !serialized.contains("/tmp/private")
            && !serialized.contains("sensitive label")
            && !serialized.contains("sensitive text"),
        "timeout recovery contract must not expose raw interaction args: {serialized}"
    );
}

#[tokio::test]
async fn post_interaction_projection_reads_refreshed_runtime_and_frame_state() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-interaction-test"),
        None,
    ));
    state
        .publish_runtime_state_snapshot(
            0,
            RuntimeStateSnapshot {
                state_inspector: StateInspectorInfo {
                    status: StateInspectorStatus::Inactive,
                    ..StateInspectorInfo::default()
                },
                readiness_state: ReadinessInfo {
                    status: ReadinessStatus::Inactive,
                    ..ReadinessInfo::default()
                },
            },
        )
        .await;
    state
        .set_frame_runtime(FrameRuntimeInfo {
            status: FrameContextStatus::Top,
            current_frame: Some(FrameContextInfo {
                frame_id: "frame-before".to_string(),
                name: None,
                parent_frame_id: None,
                target_id: None,
                url: Some("https://before.example".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            }),
            primary_frame: None,
            frame_lineage: vec!["frame-before".to_string()],
            degraded_reason: None,
        })
        .await;
    state
        .set_interference_runtime(InterferenceRuntimeInfo {
            status: InterferenceRuntimeStatus::Inactive,
            ..InterferenceRuntimeInfo::default()
        })
        .await;

    let refreshed_runtime = RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo {
            status: StateInspectorStatus::Active,
            cookie_count: 2,
            ..StateInspectorInfo::default()
        },
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            ..ReadinessInfo::default()
        },
    };
    let refreshed_frame = FrameRuntimeInfo {
        status: FrameContextStatus::Child,
        current_frame: Some(FrameContextInfo {
            frame_id: "frame-after".to_string(),
            name: Some("child".to_string()),
            parent_frame_id: Some("frame-root".to_string()),
            target_id: Some("target-after".to_string()),
            url: Some("https://after.example/frame".to_string()),
            depth: 1,
            same_origin_accessible: Some(true),
        }),
        primary_frame: Some(FrameContextInfo {
            frame_id: "frame-root".to_string(),
            name: None,
            parent_frame_id: None,
            target_id: Some("target-after".to_string()),
            url: Some("https://after.example".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        }),
        frame_lineage: vec!["frame-root".to_string(), "frame-after".to_string()],
        degraded_reason: None,
    };
    let refreshed_interference = InterferenceRuntimeInfo {
        status: InterferenceRuntimeStatus::Active,
        ..InterferenceRuntimeInfo::default()
    };

    let projection = collect_post_interaction_projection(&state, || {
        let state = state.clone();
        let refreshed_runtime = refreshed_runtime.clone();
        let refreshed_frame = refreshed_frame.clone();
        let refreshed_interference = refreshed_interference.clone();
        async move {
            state
                .publish_runtime_state_snapshot(1, refreshed_runtime)
                .await;
            state.set_frame_runtime(refreshed_frame).await;
            state.set_interference_runtime(refreshed_interference).await;
        }
    })
    .await;

    assert_eq!(projection.runtime_after, refreshed_runtime);
    assert_eq!(projection.frame_runtime, refreshed_frame);
    assert_eq!(projection.interference_after, refreshed_interference);
}

#[tokio::test]
async fn stable_post_interaction_projection_waits_for_browser_quiescence_before_sampling() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-interaction-stable-test"),
        None,
    ));
    let runtime_before = RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo {
            status: StateInspectorStatus::Inactive,
            ..StateInspectorInfo::default()
        },
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Inactive,
            ..ReadinessInfo::default()
        },
    };
    let interference_before = InterferenceRuntimeInfo {
        status: InterferenceRuntimeStatus::Inactive,
        ..InterferenceRuntimeInfo::default()
    };
    state
        .publish_runtime_state_snapshot(0, runtime_before.clone())
        .await;
    state
        .set_frame_runtime(FrameRuntimeInfo {
            status: FrameContextStatus::Top,
            current_frame: Some(FrameContextInfo {
                frame_id: "frame-before".to_string(),
                name: None,
                parent_frame_id: None,
                target_id: None,
                url: Some("https://before.example".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            }),
            primary_frame: None,
            frame_lineage: vec!["frame-before".to_string()],
            degraded_reason: None,
        })
        .await;
    state
        .set_interference_runtime(interference_before.clone())
        .await;

    let baseline = InteractionObservationBaseline {
        observatory_cursor: state.observatory_cursor().await,
        observatory_ingress_drop_count: state.observatory_ingress_drop_count(),
        request_cursor: state.network_request_cursor().await,
        network_request_ingress_drop_count: state.network_request_ingress_drop_count(),
        download_cursor: state.download_cursor().await,
        download_ingress_drop_count: state.download_event_ingress_drop_count(),
        download_degraded_reason_before: None,
        browser_event_cursor: state.browser_event_cursor(),
        runtime_before: Some(runtime_before),
        interference_before,
    };

    let refreshed_runtime = RuntimeStateSnapshot {
        state_inspector: StateInspectorInfo {
            status: StateInspectorStatus::Active,
            cookie_count: 3,
            ..StateInspectorInfo::default()
        },
        readiness_state: ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            ..ReadinessInfo::default()
        },
    };
    let refreshed_frame = FrameRuntimeInfo {
        status: FrameContextStatus::Child,
        current_frame: Some(FrameContextInfo {
            frame_id: "frame-after".to_string(),
            name: Some("child".to_string()),
            parent_frame_id: Some("frame-root".to_string()),
            target_id: Some("target-after".to_string()),
            url: Some("https://after.example/frame".to_string()),
            depth: 1,
            same_origin_accessible: Some(true),
        }),
        primary_frame: Some(FrameContextInfo {
            frame_id: "frame-root".to_string(),
            name: None,
            parent_frame_id: None,
            target_id: Some("target-after".to_string()),
            url: Some("https://after.example".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        }),
        frame_lineage: vec!["frame-root".to_string(), "frame-after".to_string()],
        degraded_reason: None,
    };
    let refreshed_interference = InterferenceRuntimeInfo {
        status: InterferenceRuntimeStatus::Active,
        ..InterferenceRuntimeInfo::default()
    };

    let delayed = state.clone();
    let refreshed_runtime_for_task = refreshed_runtime.clone();
    let refreshed_frame_for_task = refreshed_frame.clone();
    let refreshed_interference_for_task = refreshed_interference.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let browser_sequence = delayed.allocate_browser_event_sequence();
        delayed
            .publish_runtime_state_snapshot(1, refreshed_runtime_for_task)
            .await;
        delayed.set_frame_runtime(refreshed_frame_for_task).await;
        delayed
            .set_interference_runtime(refreshed_interference_for_task)
            .await;
        delayed
            .record_download_started_sequenced(
                1,
                1,
                "guid-after".to_string(),
                "https://after.example/file.txt".to_string(),
                "file.txt".to_string(),
                Some("frame-after".to_string()),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        delayed.record_browser_event_commit(browser_sequence);
    });

    let stable = collect_stable_post_interaction_projection(&state, &baseline, || async {}).await;

    assert_eq!(stable.projection_state.runtime_after, refreshed_runtime);
    assert_eq!(stable.projection_state.frame_runtime, refreshed_frame);
    assert_eq!(
        stable.projection_state.interference_after,
        refreshed_interference
    );
    assert_eq!(stable.trace_windows.download_events.len(), 1);
    assert_eq!(
        stable.trace_windows.download_events[0]
            .download
            .suggested_filename
            .as_deref(),
        Some("file.txt")
    );
}

#[tokio::test]
async fn interaction_trace_windows_are_capped_to_observation_fence() {
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-interaction-window-fence-test"),
        None,
    ));
    state
        .record_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "first".to_string(),
            source: None,
        })
        .await;
    let observatory_cursor = state.observatory_cursor().await;
    state
        .record_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "second".to_string(),
            source: None,
        })
        .await;

    state
        .upsert_network_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.test/first".to_string(),
            method: "GET".to_string(),
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
            resource_type: None,
            mime_type: None,
        })
        .await;
    let request_cursor = state.network_request_cursor().await;
    state
        .upsert_network_request_record(NetworkRequestRecord {
            request_id: "req-2".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.test/second".to_string(),
            method: "GET".to_string(),
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
            resource_type: None,
            mime_type: None,
        })
        .await;

    state
        .record_download_started_sequenced(
            0,
            1,
            "guid-1".to_string(),
            "https://example.test/file-1.txt".to_string(),
            "file-1.txt".to_string(),
            None,
        )
        .await;
    let download_cursor = state.download_cursor().await;
    state
        .record_download_started_sequenced(
            0,
            2,
            "guid-2".to_string(),
            "https://example.test/file-2.txt".to_string(),
            "file-2.txt".to_string(),
            None,
        )
        .await;

    let baseline = InteractionObservationBaseline {
        observatory_cursor: 0,
        observatory_ingress_drop_count: 0,
        request_cursor: 0,
        network_request_ingress_drop_count: 0,
        download_cursor: 0,
        download_ingress_drop_count: 0,
        download_degraded_reason_before: None,
        browser_event_cursor: 0,
        runtime_before: None,
        interference_before: InterferenceRuntimeInfo::default(),
    };
    let fence = InteractionObservationFence {
        observatory_cursor,
        observatory_ingress_drop_count: state.observatory_ingress_drop_count(),
        request_cursor,
        network_request_ingress_drop_count: state.network_request_ingress_drop_count(),
        download_cursor,
        download_ingress_drop_count: state.download_event_ingress_drop_count(),
        download_degraded_reason_after: None,
        browser_event_cursor: 0,
    };

    let windows = capture_interaction_trace_windows(&state, &baseline, &fence).await;

    assert_eq!(windows.observatory_events.len(), 1);
    assert!(matches!(
        windows.observatory_events[0].payload,
        rub_core::model::RuntimeObservatoryEventPayload::ConsoleError(_)
    ));
    assert_eq!(windows.network_requests.len(), 1);
    assert_eq!(windows.network_requests[0].request_id, "req-1");
    assert_eq!(windows.download_events.len(), 1);
    assert_eq!(windows.download_events[0].download.guid, "guid-1");
}

#[test]
fn typed_click_payload_accepts_ref_alias_locator() {
    let parsed = parse_json_args::<ClickArgs>(
        &serde_json::json!({
            "ref": "frame:7",
            "gesture": "double",
        }),
        "click",
    )
    .expect("click payload should accept ref alias");
    assert_eq!(parsed.gesture.as_deref(), Some("double"));
}

#[test]
fn typed_click_payload_accepts_wait_after_compat_field() {
    let parsed = parse_json_args::<ClickArgs>(
        &serde_json::json!({
            "selector": "#submit",
            "wait_after": {"text":"Saved"},
            "_trigger": {"kind": "trigger_action"},
        }),
        "click",
    )
    .expect("click payload should accept post-wait compatibility field");
    assert!(parsed._wait_after.is_some());
    assert!(parsed._trigger.is_some());
}

#[test]
fn typed_type_payload_accepts_trigger_metadata() {
    let parsed = parse_json_args::<TextEntryArgs>(
        &serde_json::json!({
            "selector": "#message",
            "text": "Ada",
            "_trigger": {"kind": "trigger_action"},
        }),
        "type",
    )
    .expect("type payload should accept trigger metadata");
    assert_eq!(parsed.text, "Ada");
    assert!(parsed._trigger.is_some());
}

#[test]
fn typed_upload_payload_accepts_path_state_metadata() {
    let parsed = parse_json_args::<UploadArgs>(
        &serde_json::json!({
            "selector": "input[type=file]",
            "path": "/tmp/upload.txt",
            "path_state": {
                "path_authority": "cli.upload.path"
            }
        }),
        "upload",
    )
    .expect("upload payload should accept display-only path metadata");
    assert_eq!(parsed.path, "/tmp/upload.txt");
}
