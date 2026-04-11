use super::observation_filter::{
    apply_observation_projection, attach_observation_projection_metadata,
    parse_observation_projection,
};
use super::observation_scope::{
    apply_observation_scope, apply_projection_limit, attach_scope_metadata, parse_observation_scope,
};
use super::projection::{
    attach_result, attach_subject, navigation_subject, page_entity, viewport_subject,
};
use super::request_args::parse_json_args;
use super::snapshot::build_stable_snapshot;
use super::state_format::{StateFormat, project_snapshot};
use super::url_normalization::normalize_open_url;
use super::*;

mod args;
mod screenshot;
mod settle;
mod tabs;

use self::args::{
    OpenArgs, ReloadArgs, ScrollArgs, StateArgs, parse_optional_load_strategy,
    parse_optional_scroll_direction,
};
pub(super) use self::screenshot::{
    cmd_screenshot, inline_screenshot_payload_exceeds_limit, write_screenshot_artifact,
};
use self::settle::{active_tab_entity, is_page_load_timeout, settle_navigation_projection};
pub(super) use self::tabs::{cmd_close_tab, cmd_switch, cmd_tabs};

pub(super) async fn cmd_open(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let parsed: OpenArgs = parse_json_args(args, "open")?;
    let requested_url = parsed.url;
    let url = normalize_open_url(&requested_url);
    let strategy = parse_optional_load_strategy(parsed.load_strategy.as_deref(), "load_strategy")?;

    let page = match router
        .browser
        .navigate(&url, strategy, deadline.remaining_ms())
        .await
    {
        Ok(page) => page,
        Err(error) => {
            if is_page_load_timeout(&error) {
                settle_navigation_projection(router, state, "open", deadline).await;
                state.mark_pending_external_dom_change();
            }
            return Err(error);
        }
    };
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "open", deadline).await;
    let active_tab = active_tab_entity(router).await?;
    let mut data = serde_json::json!({});
    attach_subject(
        &mut data,
        serde_json::json!({
            "kind": "tab_navigation",
            "action": "open",
            "requested_url": requested_url,
            "normalized_url": url,
        }),
    );
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&page),
            "active_tab": active_tab,
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

pub(super) async fn cmd_state(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: StateArgs = parse_json_args(args, "state")?;
    let limit = parsed.limit.map(|value| value.min(u32::MAX as u64) as u32);
    let format = StateFormat::parse(parsed.format.as_deref())?;
    let a11y_requested = parsed.a11y;
    let viewport = parsed.viewport;
    let diff_base_id = parsed.diff.as_deref();
    let listeners = parsed.listeners;
    let observation_scope = parse_observation_scope(args)?;
    let observation_projection =
        parse_observation_projection(args, matches!(format, StateFormat::Compact))?;
    let a11y = a11y_requested || matches!(format, StateFormat::A11y);

    if diff_base_id.is_some() && !matches!(format, StateFormat::Snapshot) {
        return Err(RubError::domain(
            rub_core::error::ErrorCode::InvalidInput,
            "state --diff cannot be combined with --format",
        ));
    }
    if matches!(format, StateFormat::A11y)
        && matches!(
            observation_projection.mode,
            super::observation_filter::ObservationProjectionMode::Compact
        )
    {
        return Err(RubError::domain(
            rub_core::error::ErrorCode::InvalidInput,
            "state --format a11y cannot be combined with --compact",
        ));
    }
    if diff_base_id.is_some() && observation_scope.is_some() {
        return Err(RubError::domain(
            rub_core::error::ErrorCode::InvalidInput,
            "state --diff cannot be combined with observation scoping",
        ));
    }

    let capture_limit =
        if observation_scope.is_some() || observation_projection.depth_limit.is_some() {
            Some(0)
        } else {
            limit
        };
    let mut snapshot = build_stable_snapshot(
        router,
        args,
        state,
        deadline,
        capture_limit,
        a11y,
        listeners,
    )
    .await?;
    let mut scoped_metadata = None::<(rub_core::observation::ObservationScope, u32, u32)>;
    if let Some(scope) = observation_scope.as_ref() {
        let scoped = apply_observation_scope(router, snapshot, scope).await?;
        scoped_metadata = Some((
            scoped.scope.clone(),
            scoped.scope_total_count,
            scoped.scope_match_count,
        ));
        snapshot = scoped.snapshot;
    }

    if viewport {
        let (vw, vh) = router.browser.viewport_dimensions().await?;
        snapshot.elements.retain(|el| {
            if let Some(ref bb) = el.bounding_box {
                bb.x + bb.width > 0.0
                    && bb.y + bb.height > 0.0
                    && bb.x < vw
                    && bb.y < vh
                    && (bb.width > 0.0 || bb.height > 0.0)
            } else {
                false
            }
        });
        snapshot.viewport_filtered = Some(true);
        snapshot.viewport_count = Some(snapshot.elements.len() as u32);
    }

    let projection_metadata = apply_observation_projection(&mut snapshot, observation_projection);

    if observation_scope.is_some() || observation_projection.depth_limit.is_some() {
        apply_projection_limit(&mut snapshot, limit);
    }

    let snapshot = state.cache_snapshot(snapshot).await;

    if let Some(base_id) = diff_base_id {
        let old_snapshot = state.get_snapshot(base_id).await.ok_or_else(|| {
            RubError::domain(
                rub_core::error::ErrorCode::StaleSnapshot,
                format!("Snapshot '{base_id}' not found in cache"),
            )
        })?;
        if old_snapshot.frame_context.frame_id != snapshot.frame_context.frame_id {
            return Err(RubError::domain_with_context(
                rub_core::error::ErrorCode::InvalidInput,
                "state --diff cannot compare snapshots captured from different frame contexts",
                serde_json::json!({
                    "base_snapshot_id": base_id,
                    "base_frame_id": old_snapshot.frame_context.frame_id.clone(),
                    "current_frame_id": snapshot.frame_context.frame_id.clone(),
                }),
            ));
        }
        let diff = rub_cdp::dom::diff_snapshots(&old_snapshot, &snapshot);
        let mut data = serde_json::json!({});
        attach_subject(
            &mut data,
            serde_json::json!({
                "kind": "page_observation_diff",
                "base_snapshot_id": base_id,
                "current_snapshot_id": snapshot.snapshot_id.clone(),
                "frame_id": snapshot.frame_context.frame_id.clone(),
            }),
        );
        attach_result(
            &mut data,
            serde_json::json!({
                "diff": diff,
            }),
        );
        return Ok(data);
    }

    let frame_id = snapshot.frame_context.frame_id.clone();
    let mut projected = project_snapshot(&snapshot, format)?;
    if let Some((scope, scope_total_count, scope_match_count)) = scoped_metadata {
        attach_scope_metadata(&mut projected, &scope, scope_total_count, scope_match_count);
    }
    attach_observation_projection_metadata(&mut projected, projection_metadata);
    let mut data = serde_json::json!({});
    attach_subject(
        &mut data,
        serde_json::json!({
            "kind": "page_observation",
            "format": format.as_str(),
            "frame_id": frame_id,
            "viewport_only": viewport,
        }),
    );
    attach_result(
        &mut data,
        serde_json::json!({
            "snapshot": projected,
        }),
    );
    Ok(data)
}

pub(super) async fn cmd_scroll(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ScrollArgs = parse_json_args(args, "scroll")?;
    let direction = parse_optional_scroll_direction(parsed.direction.as_deref(), "direction")?;
    let amount = parsed.amount;
    let position = router.browser.scroll(direction, amount).await?;
    let selected_frame_id = state.selected_frame_id().await;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, viewport_subject(selected_frame_id.as_deref()));
    attach_result(
        &mut data,
        serde_json::json!({
            "direction": direction,
            "amount": amount,
            "position": position,
        }),
    );
    Ok(data)
}

pub(super) async fn cmd_back(
    router: &DaemonRouter,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let page = match router.browser.back(deadline.remaining_ms()).await {
        Ok(page) => page,
        Err(error) => {
            if is_page_load_timeout(&error) {
                settle_navigation_projection(router, state, "back", deadline).await;
                state.mark_pending_external_dom_change();
            }
            return Err(error);
        }
    };
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "back", deadline).await;
    let at_start = router
        .browser
        .execute_js(
            "(() => { const nav = globalThis.navigation; if (nav && typeof nav.canGoBack === 'boolean') return !nav.canGoBack; return history.length <= 1; })()",
        )
        .await
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let active_tab = active_tab_entity(router).await?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, navigation_subject("back"));
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&page),
            "active_tab": active_tab,
            "at_start": at_start,
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

pub(super) async fn cmd_forward(
    router: &DaemonRouter,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let page = match router.browser.forward(deadline.remaining_ms()).await {
        Ok(page) => page,
        Err(error) => {
            if is_page_load_timeout(&error) {
                settle_navigation_projection(router, state, "forward", deadline).await;
                state.mark_pending_external_dom_change();
            }
            return Err(error);
        }
    };
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "forward", deadline).await;
    let at_end = router
        .browser
        .execute_js(
            "(() => { const nav = globalThis.navigation; if (nav && typeof nav.canGoForward === 'boolean') return !nav.canGoForward; return false; })()",
        )
        .await
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let active_tab = active_tab_entity(router).await?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, navigation_subject("forward"));
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&page),
            "active_tab": active_tab,
            "at_end": at_end,
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

pub(super) async fn cmd_reload(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let parsed: ReloadArgs = parse_json_args(args, "reload")?;
    let strategy = parse_optional_load_strategy(parsed.load_strategy.as_deref(), "load_strategy")?;

    let page = match router
        .browser
        .reload(strategy, deadline.remaining_ms())
        .await
    {
        Ok(page) => page,
        Err(error) => {
            if is_page_load_timeout(&error) {
                settle_navigation_projection(router, state, "reload", deadline).await;
                state.mark_pending_external_dom_change();
            }
            return Err(error);
        }
    };
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "reload", deadline).await;
    let active_tab = active_tab_entity(router).await?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, navigation_subject("reload"));
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&page),
            "active_tab": active_tab,
            "load_strategy": strategy,
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

#[cfg(test)]
mod tests {
    use super::args::{
        OpenArgs, ReloadArgs, ScreenshotArgs, ScrollArgs, StateArgs, SwitchArgs,
        parse_optional_load_strategy, parse_optional_scroll_direction,
    };
    use super::{inline_screenshot_payload_exceeds_limit, write_screenshot_artifact};
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
            }),
            "open",
        )
        .expect("open payload should accept post-wait compatibility field");
        assert!(parsed._wait_after.is_some());
        assert!(parsed._trigger.is_some());
    }

    #[test]
    fn typed_reload_payload_accepts_trigger_metadata() {
        let parsed: ReloadArgs = parse_json_args(
            &serde_json::json!({
                "load_strategy": "load",
                "_trigger": {"kind": "trigger_action"},
            }),
            "reload",
        )
        .expect("reload payload should accept trigger metadata");
        assert_eq!(parsed.load_strategy.as_deref(), Some("load"));
        assert!(parsed._trigger.is_some());
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
}
