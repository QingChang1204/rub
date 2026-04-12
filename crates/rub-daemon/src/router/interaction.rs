mod args;
mod explain;
mod projection;

use self::args::{
    ClickArgs, ClickGesture, HoverArgs, KeysArgs, SelectArgs, TextEntryArgs, UploadArgs,
    click_command_name, click_gesture_name, requested_click_gesture,
};
use self::explain::enrich_interactability_error_if_needed;
use self::projection::{
    capture_interaction_baseline, finalize_interaction_projection, finalize_select_projection,
};
use super::addressing::resolve_element;
use super::artifacts::annotate_path_reference_state;
use super::projection::{
    attach_result, attach_subject, coordinates_subject, element_subject, focused_frame_subject,
};
use super::request_args::parse_json_args;
use super::*;

pub(super) async fn cmd_click(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ClickArgs = parse_json_args(args, "click")?;
    cmd_click_with_gesture(router, args, parsed, deadline, state).await
}

async fn cmd_click_with_gesture(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: ClickArgs,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let gesture = requested_click_gesture(args.gesture.as_deref())?;
    if let Some([x, y]) = args.xy {
        let baseline = capture_interaction_baseline(router, state).await;
        let outcome = match gesture {
            ClickGesture::Single => router.browser.click_xy(x, y).await?,
            ClickGesture::Double => router.browser.dblclick_xy(x, y).await?,
            ClickGesture::Right => router.browser.rightclick_xy(x, y).await?,
        };
        let mut data = serde_json::json!({});
        attach_subject(&mut data, coordinates_subject(x, y));
        attach_result(
            &mut data,
            serde_json::json!({
                "gesture": click_gesture_name(gesture),
                "dialog_dismissed": null,
            }),
        );
        finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
        return Ok(data);
    }

    let resolved = resolve_element(
        router,
        raw_args,
        state,
        deadline,
        click_command_name(gesture),
    )
    .await?;
    let element = resolved.element;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match match gesture {
        ClickGesture::Single => router.browser.click(&element).await,
        ClickGesture::Double => router.browser.dblclick(&element).await,
        ClickGesture::Right => router.browser.rightclick(&element).await,
    } {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "click",
                &element,
                &resolved.snapshot_id,
                raw_args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "gesture": click_gesture_name(gesture),
            "dialog_dismissed": null,
        }),
    );
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_keys(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: KeysArgs = parse_json_args(args, "keys")?;
    let combo = rub_core::model::KeyCombo::parse(&parsed.keys)?;
    let baseline = capture_interaction_baseline(router, state).await;
    let selected_frame_id =
        super::frame_scope::effective_request_frame_id(router, args, state).await?;
    let outcome = router.browser.send_keys(&combo).await?;
    let mut data = serde_json::json!({});
    attach_subject(
        &mut data,
        focused_frame_subject(selected_frame_id.as_deref()),
    );
    attach_result(
        &mut data,
        serde_json::json!({
            "keys": parsed.keys,
        }),
    );
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_type(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_text_entry(router, args, deadline, state).await
}

async fn cmd_text_entry(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let args: TextEntryArgs = parse_json_args(raw_args, "type")?;
    let text = args.text;
    let clear = args.clear;
    let baseline = capture_interaction_baseline(router, state).await;
    let mut data = serde_json::json!({});
    attach_result(
        &mut data,
        serde_json::json!({
            "text": text,
            "clear": clear,
        }),
    );
    let outcome = if args.locator.is_requested() {
        let resolved = resolve_element(router, raw_args, state, deadline, "type").await?;
        attach_subject(
            &mut data,
            element_subject(&resolved.element, &resolved.snapshot_id),
        );
        match router
            .browser
            .type_into(&resolved.element, &text, clear)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(enrich_interactability_error_if_needed(
                    router,
                    state,
                    "type",
                    &resolved.element,
                    &resolved.snapshot_id,
                    raw_args,
                    error,
                )
                .await);
            }
        }
    } else if clear {
        return Err(RubError::domain(
            rub_core::error::ErrorCode::InvalidInput,
            "`type --clear` requires a target locator or index in the current baseline",
        ));
    } else {
        let selected_frame_id =
            super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
        attach_subject(
            &mut data,
            focused_frame_subject(selected_frame_id.as_deref()),
        );
        router
            .browser
            .type_text_in_frame(selected_frame_id.as_deref(), &text)
            .await?
    };
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_hover(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _: HoverArgs = parse_json_args(args, "hover")?;
    let resolved = resolve_element(router, args, state, deadline, "hover").await?;
    let element = resolved.element;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match router.browser.hover(&element).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "hover",
                &element,
                &resolved.snapshot_id,
                args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(&mut data, serde_json::json!({}));
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_upload(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: UploadArgs = parse_json_args(args, "upload")?;
    let resolved = resolve_element(router, args, state, deadline, "upload").await?;
    let element = resolved.element;
    let path = parsed.path;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match router.browser.upload_file(&element, &path).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "upload",
                &element,
                &resolved.snapshot_id,
                args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "path": path,
        }),
    );
    if let Some(result) = data.get_mut("result") {
        annotate_path_reference_state(
            result,
            "router.upload.input_path",
            "upload_command_request",
            "external_input_file",
        );
    }
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_select(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: SelectArgs = parse_json_args(args, "select")?;
    let resolved = resolve_element(router, args, state, deadline, "select").await?;
    let element = resolved.element;
    let value = parsed.value;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match router.browser.select_option(&element, &value).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "select",
                &element,
                &resolved.snapshot_id,
                args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "value": outcome.selected_value,
            "text": outcome.selected_text,
        }),
    );
    finalize_select_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_interactability_probe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    explain::cmd_interactability_probe(router, args, deadline, state).await
}

#[cfg(test)]
mod tests {
    use super::args::{ClickArgs, TextEntryArgs, UploadArgs};
    use super::projection::{
        InteractionObservationBaseline, InteractionObservationFence,
        capture_interaction_trace_windows, collect_post_interaction_projection,
        collect_stable_post_interaction_projection,
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
            observatory_drop_count: state.observatory().await.dropped_event_count,
            request_cursor: state.network_request_cursor().await,
            network_request_drop_count: state.network_request_drop_count().await,
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

        let stable =
            collect_stable_post_interaction_projection(&state, &baseline, || async {}).await;

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
            observatory_drop_count: 0,
            request_cursor: 0,
            network_request_drop_count: 0,
            download_cursor: 0,
            download_ingress_drop_count: 0,
            download_degraded_reason_before: None,
            browser_event_cursor: 0,
            runtime_before: None,
            interference_before: InterferenceRuntimeInfo::default(),
        };
        let fence = InteractionObservationFence {
            observatory_cursor,
            observatory_drop_count: state.observatory().await.dropped_event_count,
            request_cursor,
            network_request_drop_count: state.network_request_drop_count().await,
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
}
