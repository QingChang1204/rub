use super::addressing::resolve_element;
use super::projection::{
    attach_result, attach_subject, coordinates_subject, element_subject, focused_frame_subject,
};
use super::request_args::{LocatorRequestArgs, parse_json_args};
use super::*;
use crate::runtime_refresh::{
    refresh_live_frame_runtime, refresh_live_interference_state, refresh_live_runtime_state,
};
use rub_core::model::{
    DownloadEvent, InteractionConfirmationKind, InteractionConfirmationStatus, InteractionOutcome,
    InterferenceRuntimeInfo, InterferenceRuntimeStatus, RuntimeStateSnapshot,
};
use std::future::Future;
use tokio::time::Duration;

const INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT: Duration = Duration::from_millis(250);
const INTERACTION_BROWSER_EVENT_QUIET_PERIOD: Duration = Duration::from_millis(20);

struct InteractionObservationBaseline {
    observatory_cursor: u64,
    observatory_drop_count: u64,
    request_cursor: u64,
    network_request_drop_count: u64,
    download_cursor: u64,
    browser_event_cursor: u64,
    runtime_before: Option<RuntimeStateSnapshot>,
    interference_before: InterferenceRuntimeInfo,
}

struct InteractionTraceWindows {
    observatory_events: Vec<rub_core::model::RuntimeObservatoryEvent>,
    observatory_authoritative: bool,
    observatory_degraded_reason: Option<String>,
    network_requests: Vec<rub_core::model::NetworkRequestRecord>,
    network_authoritative: bool,
    network_degraded_reason: Option<String>,
}

struct InteractionProjectionState {
    runtime_after: RuntimeStateSnapshot,
    frame_runtime: rub_core::model::FrameRuntimeInfo,
    interference_after: InterferenceRuntimeInfo,
}

#[derive(Debug, Clone, Copy)]
enum ClickGesture {
    Single,
    Double,
    Right,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    #[serde(default)]
    gesture: Option<String>,
    #[serde(default)]
    xy: Option<[f64; 2]>,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    _snapshot_id: Option<String>,
    #[serde(flatten)]
    _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysArgs {
    keys: String,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TextEntryArgs {
    text: String,
    #[serde(default)]
    clear: bool,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    _snapshot_id: Option<String>,
    #[serde(flatten)]
    locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HoverArgs {
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    _snapshot_id: Option<String>,
    #[serde(flatten)]
    _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadArgs {
    path: String,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    _snapshot_id: Option<String>,
    #[serde(flatten)]
    _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectArgs {
    value: String,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    _snapshot_id: Option<String>,
    #[serde(flatten)]
    _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

async fn capture_interaction_baseline(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> InteractionObservationBaseline {
    if let Ok(tabs) = router.browser.list_tabs().await {
        state.prime_interference_baseline(&tabs).await;
    }
    InteractionObservationBaseline {
        observatory_cursor: state.observatory_cursor().await,
        observatory_drop_count: state.observatory().await.dropped_event_count,
        request_cursor: state.network_request_cursor().await,
        network_request_drop_count: state.network_request_drop_count().await,
        download_cursor: state.download_cursor().await,
        browser_event_cursor: state.browser_event_cursor(),
        runtime_before: crate::interaction_trace::probe_runtime_state(&router.browser).await,
        interference_before: state.interference_runtime().await,
    }
}

async fn capture_interaction_trace_windows(
    state: &Arc<SessionState>,
    baseline: &InteractionObservationBaseline,
) -> InteractionTraceWindows {
    let observatory_window = state
        .observatory_event_window_after(
            baseline.observatory_cursor,
            baseline.observatory_drop_count,
        )
        .await;
    let network_window = state
        .network_request_window_after(baseline.request_cursor, baseline.network_request_drop_count)
        .await;
    InteractionTraceWindows {
        observatory_events: observatory_window.events,
        observatory_authoritative: observatory_window.authoritative,
        observatory_degraded_reason: observatory_window.degraded_reason,
        network_requests: network_window.records,
        network_authoritative: network_window.authoritative,
        network_degraded_reason: network_window.degraded_reason,
    }
}

async fn finalize_interaction_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    data: &mut serde_json::Value,
    outcome: &rub_core::model::InteractionOutcome,
    baseline: &InteractionObservationBaseline,
) {
    let projection_state = collect_post_interaction_projection(state, || async {
        refresh_live_runtime_state(&router.browser, state).await;
        refresh_live_frame_runtime(&router.browser, state).await;
        let _ = refresh_live_interference_state(&router.browser, state).await;
        let interference_after = state.interference_runtime().await;
        if should_promote_primary_context(outcome, &interference_after)
            && let Ok(tabs) = router.browser.list_tabs().await
        {
            state.adopt_interference_primary_context(&tabs).await;
        }
    })
    .await;
    state
        .wait_for_browser_event_quiescence_since(
            baseline.browser_event_cursor,
            INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT,
            INTERACTION_BROWSER_EVENT_QUIET_PERIOD,
        )
        .await;
    let trace_windows = capture_interaction_trace_windows(state, baseline).await;
    let download_events: Vec<DownloadEvent> =
        state.download_events_after(baseline.download_cursor).await;
    attach_interaction_projection(
        data,
        outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &projection_state.frame_runtime,
            runtime_before: baseline.runtime_before.as_ref(),
            runtime_after: Some(&projection_state.runtime_after),
            interference_before: Some(&baseline.interference_before),
            interference_after: Some(&projection_state.interference_after),
            observatory_events: &trace_windows.observatory_events,
            observatory_authoritative: trace_windows.observatory_authoritative,
            observatory_degraded_reason: trace_windows.observatory_degraded_reason.as_deref(),
            network_requests: &trace_windows.network_requests,
            network_authoritative: trace_windows.network_authoritative,
            network_degraded_reason: trace_windows.network_degraded_reason.as_deref(),
            download_events: &download_events,
        },
    );
}

fn should_promote_primary_context(
    outcome: &InteractionOutcome,
    interference_after: &InterferenceRuntimeInfo,
) -> bool {
    matches!(
        interference_after.status,
        InterferenceRuntimeStatus::Inactive
    ) && matches!(
        outcome
            .confirmation
            .as_ref()
            .map(|confirmation| (confirmation.status, confirmation.kind)),
        Some((
            InteractionConfirmationStatus::Confirmed,
            Some(InteractionConfirmationKind::ContextChange)
        ))
    )
}

async fn finalize_select_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    data: &mut serde_json::Value,
    outcome: &rub_core::model::SelectOutcome,
    baseline: &InteractionObservationBaseline,
) {
    let projection_state = collect_post_interaction_projection(state, || async {
        refresh_live_runtime_state(&router.browser, state).await;
        refresh_live_frame_runtime(&router.browser, state).await;
        let _ = refresh_live_interference_state(&router.browser, state).await;
    })
    .await;
    state
        .wait_for_browser_event_quiescence_since(
            baseline.browser_event_cursor,
            INTERACTION_BROWSER_EVENT_FENCE_TIMEOUT,
            INTERACTION_BROWSER_EVENT_QUIET_PERIOD,
        )
        .await;
    let trace_windows = capture_interaction_trace_windows(state, baseline).await;
    let download_events = state.download_events_after(baseline.download_cursor).await;
    attach_select_projection(
        data,
        outcome,
        crate::router::projection::ProjectionSignals {
            frame_runtime: &projection_state.frame_runtime,
            runtime_before: baseline.runtime_before.as_ref(),
            runtime_after: Some(&projection_state.runtime_after),
            interference_before: Some(&baseline.interference_before),
            interference_after: Some(&projection_state.interference_after),
            observatory_events: &trace_windows.observatory_events,
            observatory_authoritative: trace_windows.observatory_authoritative,
            observatory_degraded_reason: trace_windows.observatory_degraded_reason.as_deref(),
            network_requests: &trace_windows.network_requests,
            network_authoritative: trace_windows.network_authoritative,
            network_degraded_reason: trace_windows.network_degraded_reason.as_deref(),
            download_events: &download_events,
        },
    );
}

async fn collect_post_interaction_projection<F, Fut>(
    state: &Arc<SessionState>,
    refresh: F,
) -> InteractionProjectionState
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    refresh().await;
    InteractionProjectionState {
        runtime_after: state.runtime_state_snapshot().await,
        frame_runtime: state.frame_runtime().await,
        interference_after: state.interference_runtime().await,
    }
}

pub(super) async fn cmd_click(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ClickArgs = parse_json_args(args, "click")?;
    cmd_click_with_gesture(router, args, parsed, state).await
}

async fn cmd_click_with_gesture(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: ClickArgs,
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

    let resolved = resolve_element(router, raw_args, state, click_command_name(gesture)).await?;
    let element = resolved.element;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match gesture {
        ClickGesture::Single => router.browser.click(&element).await?,
        ClickGesture::Double => router.browser.dblclick(&element).await?,
        ClickGesture::Right => router.browser.rightclick(&element).await?,
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
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_text_entry(router, args, state).await
}

async fn cmd_text_entry(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
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
        let resolved = resolve_element(router, raw_args, state, "type").await?;
        attach_subject(
            &mut data,
            element_subject(&resolved.element, &resolved.snapshot_id),
        );
        router
            .browser
            .type_into(&resolved.element, &text, clear)
            .await?
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

fn requested_click_gesture(gesture: Option<&str>) -> Result<ClickGesture, RubError> {
    let gesture = gesture.unwrap_or("single");
    match gesture {
        "single" => Ok(ClickGesture::Single),
        "double" => Ok(ClickGesture::Double),
        "right" => Ok(ClickGesture::Right),
        other => Err(RubError::domain(
            rub_core::error::ErrorCode::InvalidInput,
            format!("Unsupported click gesture: {other}"),
        )),
    }
}

fn click_command_name(gesture: ClickGesture) -> &'static str {
    match gesture {
        ClickGesture::Single => "click",
        ClickGesture::Double | ClickGesture::Right => "click",
    }
}

fn click_gesture_name(gesture: ClickGesture) -> &'static str {
    match gesture {
        ClickGesture::Single => "single",
        ClickGesture::Double => "double",
        ClickGesture::Right => "right",
    }
}

pub(super) async fn cmd_hover(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _: HoverArgs = parse_json_args(args, "hover")?;
    let resolved = resolve_element(router, args, state, "hover").await?;
    let element = resolved.element;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = router.browser.hover(&element).await?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(&mut data, serde_json::json!({}));
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_upload(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: UploadArgs = parse_json_args(args, "upload")?;
    let resolved = resolve_element(router, args, state, "upload").await?;
    let element = resolved.element;
    let path = parsed.path;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = router.browser.upload_file(&element, &path).await?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "path": path,
        }),
    );
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_select(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: SelectArgs = parse_json_args(args, "select")?;
    let resolved = resolve_element(router, args, state, "select").await?;
    let element = resolved.element;
    let value = parsed.value;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = router.browser.select_option(&element, &value).await?;
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

#[cfg(test)]
mod tests {
    use super::{ClickArgs, collect_post_interaction_projection};
    use crate::router::request_args::parse_json_args;
    use crate::session::SessionState;
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, InterferenceRuntimeInfo,
        InterferenceRuntimeStatus, ReadinessInfo, ReadinessStatus, RouteStability,
        RuntimeStateSnapshot, StateInspectorInfo, StateInspectorStatus,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

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
            }),
            "click",
        )
        .expect("click payload should accept post-wait compatibility field");
        assert!(parsed._wait_after.is_some());
    }
}
