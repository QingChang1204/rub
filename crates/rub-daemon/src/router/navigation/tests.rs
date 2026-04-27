use super::args::{
    OpenArgs, ReloadArgs, ScreenshotArgs, ScrollArgs, StateArgs, SwitchArgs,
    parse_optional_load_strategy, parse_optional_scroll_direction,
};
use super::{
    history_boundary_projection, inline_screenshot_payload_exceeds_limit, write_screenshot_artifact,
};
use crate::router::request_args::parse_json_args;
use rub_core::error::ErrorCode;
use rub_core::model::{LoadStrategy, ScrollDirection};

#[test]
fn invalid_load_strategy_is_rejected() {
    let error = parse_optional_load_strategy(Some("slow"), "load_strategy")
        .expect_err("invalid load strategy should fail");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn valid_load_strategy_parses() {
    let strategy = parse_optional_load_strategy(Some("domcontentloaded"), "load_strategy")
        .expect("valid load strategy");
    assert_eq!(strategy, LoadStrategy::DomContentLoaded);
}

#[test]
fn invalid_scroll_direction_is_rejected() {
    let error = parse_optional_scroll_direction(Some("sideways"), "direction")
        .expect_err("invalid direction should fail");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn valid_scroll_direction_parses() {
    let direction =
        parse_optional_scroll_direction(Some("up"), "direction").expect("valid direction");
    assert_eq!(direction, ScrollDirection::Up);
}

#[test]
fn typed_open_payload_accepts_navigation_fields() {
    let parsed: OpenArgs = parse_json_args(
        &serde_json::json!({
            "url": "example.com",
            "load_strategy": "load",
        }),
        "open",
    )
    .expect("open payload should parse");
    assert_eq!(parsed.url, "example.com");
    assert_eq!(parsed.load_strategy.as_deref(), Some("load"));
}

#[test]
fn typed_open_payload_accepts_wait_after_compat_field() {
    let parsed: OpenArgs = parse_json_args(
        &serde_json::json!({
            "url": "https://example.com",
            "wait_after": {"text":"Ready"},
            "_trigger": {"kind": "trigger_action"},
            "_orchestration": {"frame_id": "frame-1"},
        }),
        "open",
    )
    .expect("open payload should accept post-wait compatibility field");
    assert!(parsed._wait_after.is_some());
    assert!(parsed._trigger.is_some());
    assert!(parsed._orchestration.is_some());
}

#[test]
fn typed_reload_payload_accepts_trigger_metadata() {
    let parsed: ReloadArgs = parse_json_args(
        &serde_json::json!({
            "load_strategy": "load",
            "_trigger": {"kind": "trigger_action"},
            "_orchestration": {"frame_id": "frame-1"},
        }),
        "reload",
    )
    .expect("reload payload should accept trigger metadata");
    assert_eq!(parsed.load_strategy.as_deref(), Some("load"));
    assert!(parsed._trigger.is_some());
    assert!(parsed._orchestration.is_some());
}

#[test]
fn workflow_allowed_navigation_payloads_accept_hidden_orchestration_metadata() {
    let scroll = parse_json_args::<ScrollArgs>(
        &serde_json::json!({
            "direction": "down",
            "_orchestration": {"frame_id": "frame-1"},
        }),
        "scroll",
    )
    .expect("scroll payload should accept orchestration frame metadata");
    assert!(scroll._orchestration.is_some());

    let screenshot = parse_json_args::<ScreenshotArgs>(
        &serde_json::json!({
            "highlight": true,
            "_orchestration": {"frame_id": "frame-1"},
        }),
        "screenshot",
    )
    .expect("screenshot payload should accept orchestration metadata");
    assert!(screenshot._orchestration.is_some());

    let switch = parse_json_args::<SwitchArgs>(
        &serde_json::json!({
            "index": 1,
            "_orchestration": {"frame_id": "frame-1"},
        }),
        "switch",
    )
    .expect("switch payload should accept orchestration metadata");
    assert!(switch._orchestration.is_some());

    let close_tab = parse_json_args::<super::args::CloseTabArgs>(
        &serde_json::json!({
            "index": 1,
            "_orchestration": {"frame_id": "frame-1"},
        }),
        "close-tab",
    )
    .expect("close-tab payload should accept orchestration metadata");
    assert!(close_tab._orchestration.is_some());
}

#[test]
fn typed_state_payload_rejects_unknown_fields() {
    let error = parse_json_args::<StateArgs>(
        &serde_json::json!({
            "limit": 10,
            "mystery": true,
        }),
        "state",
    )
    .expect_err("unknown state fields should be rejected")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn typed_scroll_payload_rejects_unknown_fields() {
    let error = parse_json_args::<ScrollArgs>(
        &serde_json::json!({
            "direction": "down",
            "other": 1,
        }),
        "scroll",
    )
    .expect_err("unknown scroll fields should fail")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn typed_switch_payload_accepts_trigger_metadata_but_stays_strict_otherwise() {
    let parsed = parse_json_args::<SwitchArgs>(
        &serde_json::json!({
            "index": 2,
            "_trigger": {
                "kind": "trigger_action",
            }
        }),
        "switch",
    )
    .expect("switch payload should accept trigger metadata");
    assert_eq!(parsed.index, 2);

    let error = parse_json_args::<SwitchArgs>(
        &serde_json::json!({
            "index": 2,
            "mystery": true,
        }),
        "switch",
    )
    .expect_err("unknown switch fields should still fail")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn inline_screenshot_limit_helper_flags_oversized_payloads() {
    assert!(!inline_screenshot_payload_exceeds_limit(1024));
    assert!(inline_screenshot_payload_exceeds_limit(7 * 1024 * 1024));
}

#[test]
fn screenshot_artifact_marks_file_truth_boundary() {
    let root =
        std::env::temp_dir().join(format!("rub-screenshot-artifact-{}", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp root");
    let output = root.join("page.png");

    let artifact = write_screenshot_artifact(
        output.to_str().expect("utf-8 path"),
        b"png",
        "router.screenshot_artifact",
        "page_screenshot_result",
        crate::router::TransactionDeadline::new(1_000),
    )
    .expect("write screenshot artifact");

    assert_eq!(artifact["output_path"], output.display().to_string());
    assert_eq!(
        artifact["artifact_state"]["truth_level"],
        "command_artifact"
    );
    assert_eq!(
        artifact["artifact_state"]["artifact_authority"],
        "router.screenshot_artifact"
    );
    assert_eq!(
        artifact["artifact_state"]["upstream_truth"],
        "page_screenshot_result"
    );
    assert_eq!(artifact["artifact_state"]["durability"], "durable");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn inspect_page_routing_sub_field_is_stripped_by_cmd_inspect_before_reaching_state_args() {
    // Documentation test: confirm that StateArgs correctly rejects "sub" when it
    // appears directly. This verifies that cmd_inspect's strip_inspect_routing_key
    // is required — if it were removed, inspect page would fail with INVALID_INPUT.
    let error = parse_json_args::<StateArgs>(
        &serde_json::json!({ "sub": "page", "format": "compact" }),
        "state",
    )
    .expect_err("StateArgs must reject 'sub' — stripping is cmd_inspect's responsibility");
    assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
}

#[test]
fn state_args_still_rejects_genuinely_unknown_fields() {
    // Guard: ensure the schema stays strict for all unknown fields.
    let error = parse_json_args::<StateArgs>(
        &serde_json::json!({ "limit": 10, "mystery": true }),
        "state",
    )
    .expect_err("unknown state fields must still be rejected")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn typed_screenshot_payload_accepts_path_state_metadata() {
    let parsed = parse_json_args::<ScreenshotArgs>(
        &serde_json::json!({
            "path": "/tmp/capture.png",
            "path_state": {
                "path_authority": "cli.screenshot.path"
            }
        }),
        "screenshot",
    )
    .expect("screenshot payload should accept display-only path metadata");
    assert_eq!(parsed.path.as_deref(), Some("/tmp/capture.png"));
}

#[test]
fn snapshot_diff_metadata_projects_comparison_scope() {
    let projection = super::snapshot_diff_metadata("base-snap", "current-snap", "frame-1");
    assert_eq!(
        projection,
        serde_json::json!({
            "kind": "snapshot_comparison",
            "base_snapshot": {
                "snapshot_id": "base-snap",
                "frame_id": "frame-1",
            },
            "current_snapshot": {
                "snapshot_id": "current-snap",
                "frame_id": "frame-1",
            },
        })
    );
}

#[test]
fn snapshot_diff_mismatch_context_projects_comparison_scope() {
    let context =
        super::snapshot_diff_mismatch_context("base-snap", "current-snap", "frame-a", "frame-b");
    assert_eq!(
        context,
        serde_json::json!({
            "comparison": {
                "kind": "snapshot_comparison",
                "base_snapshot": {
                    "snapshot_id": "base-snap",
                    "frame_id": "frame-a",
                },
                "current_snapshot": {
                    "snapshot_id": "current-snap",
                    "frame_id": "frame-b",
                },
            },
        })
    );
}

#[test]
fn history_boundary_projection_preserves_boolean_truth() {
    let projection = history_boundary_projection(Some(true), "probe_failed");
    assert_eq!(projection.value, Some(true));
    assert_eq!(projection.degraded_reason, None);
}

#[test]
fn history_boundary_projection_degrades_when_probe_fails() {
    let projection = history_boundary_projection(None, "probe_failed");
    assert_eq!(projection.value, None);
    assert_eq!(projection.degraded_reason, Some("probe_failed"));
}
