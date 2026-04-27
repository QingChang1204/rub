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
use self::settle::{active_tab_projection, is_page_load_timeout, settle_navigation_projection};
pub(super) use self::tabs::{cmd_close_tab, cmd_switch, cmd_tabs};

fn snapshot_diff_metadata(
    base_snapshot_id: &str,
    current_snapshot_id: &str,
    frame_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "snapshot_comparison",
        "base_snapshot": {
            "snapshot_id": base_snapshot_id,
            "frame_id": frame_id,
        },
        "current_snapshot": {
            "snapshot_id": current_snapshot_id,
            "frame_id": frame_id,
        },
    })
}

fn snapshot_diff_mismatch_context(
    base_snapshot_id: &str,
    current_snapshot_id: &str,
    base_frame_id: &str,
    current_frame_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "comparison": {
            "kind": "snapshot_comparison",
            "base_snapshot": {
                "snapshot_id": base_snapshot_id,
                "frame_id": base_frame_id,
            },
            "current_snapshot": {
                "snapshot_id": current_snapshot_id,
                "frame_id": current_frame_id,
            },
        },
    })
}

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
    let active_tab = active_tab_projection(router).await;
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
            "active_tab": active_tab.tab,
            "active_tab_degraded_reason": active_tab.degraded_reason,
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
                snapshot_diff_mismatch_context(
                    base_id,
                    &snapshot.snapshot_id,
                    &old_snapshot.frame_context.frame_id,
                    &snapshot.frame_context.frame_id,
                ),
            ));
        }
        let diff = rub_cdp::dom::diff_snapshots(&old_snapshot, &snapshot);
        let snapshot = state.cache_snapshot(snapshot).await;
        let mut data = serde_json::json!({});
        attach_subject(
            &mut data,
            serde_json::json!({
                "kind": "page_observation_diff",
                "frame_id": snapshot.frame_context.frame_id.clone(),
                "comparison": snapshot_diff_metadata(
                    base_id,
                    &snapshot.snapshot_id,
                    &snapshot.frame_context.frame_id,
                ),
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

    let snapshot = state.cache_snapshot(snapshot).await;
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
    let selected_frame_id =
        super::frame_scope::effective_request_frame_id(router, args, state).await?;
    let position = router
        .browser
        .scroll(selected_frame_id.as_deref(), direction, amount)
        .await?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryBoundaryProjection {
    value: Option<bool>,
    degraded_reason: Option<&'static str>,
}

fn history_boundary_projection(
    value: Option<bool>,
    unavailable_reason: &'static str,
) -> HistoryBoundaryProjection {
    match value {
        Some(flag) => HistoryBoundaryProjection {
            value: Some(flag),
            degraded_reason: None,
        },
        None => HistoryBoundaryProjection {
            value: None,
            degraded_reason: Some(unavailable_reason),
        },
    }
}

pub(super) async fn cmd_back(
    router: &DaemonRouter,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let navigation = match router
        .browser
        .back_with_boundary(deadline.remaining_ms())
        .await
    {
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
    let at_start =
        history_boundary_projection(navigation.at_boundary, "history_boundary_probe_failed");
    let active_tab = active_tab_projection(router).await;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, navigation_subject("back"));
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&navigation.page),
            "active_tab": active_tab.tab,
            "active_tab_degraded_reason": active_tab.degraded_reason,
            "at_start": at_start.value,
            "at_start_degraded_reason": at_start.degraded_reason,
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
    let navigation = match router
        .browser
        .forward_with_boundary(deadline.remaining_ms())
        .await
    {
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
    let at_end =
        history_boundary_projection(navigation.at_boundary, "history_boundary_probe_failed");
    let active_tab = active_tab_projection(router).await;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, navigation_subject("forward"));
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&navigation.page),
            "active_tab": active_tab.tab,
            "active_tab_degraded_reason": active_tab.degraded_reason,
            "at_end": at_end.value,
            "at_end_degraded_reason": at_end.degraded_reason,
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
    let active_tab = active_tab_projection(router).await;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, navigation_subject("reload"));
    attach_result(
        &mut data,
        serde_json::json!({
            "page": page_entity(&page),
            "active_tab": active_tab.tab,
            "active_tab_degraded_reason": active_tab.degraded_reason,
            "load_strategy": strategy,
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

pub(crate) fn semantic_replay_args(
    command: &str,
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    match command {
        "open" => {
            let parsed: OpenArgs = parse_json_args(args, "open").ok()?;
            Some(serde_json::json!({
                "url": parsed.url,
                "load_strategy": parse_optional_load_strategy(parsed.load_strategy.as_deref(), "load_strategy").ok()?,
                "wait_after": args.get("wait_after").cloned(),
            }))
        }
        "state" => {
            let parsed: StateArgs = parse_json_args(args, "state").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("limit".to_string(), serde_json::json!(parsed.limit));
            projected.insert("format".to_string(), serde_json::json!(parsed.format));
            projected.insert("a11y".to_string(), serde_json::json!(parsed.a11y));
            projected.insert("viewport".to_string(), serde_json::json!(parsed.viewport));
            projected.insert("diff".to_string(), serde_json::json!(parsed.diff));
            projected.insert("listeners".to_string(), serde_json::json!(parsed.listeners));
            copy_semantic_raw_field(args, "compact", &mut projected);
            copy_semantic_raw_field(args, "depth", &mut projected);
            copy_semantic_raw_field(args, "scope", &mut projected);
            copy_semantic_raw_field(args, "scope_selector", &mut projected);
            copy_semantic_raw_field(args, "scope_role", &mut projected);
            copy_semantic_raw_field(args, "scope_label", &mut projected);
            copy_semantic_raw_field(args, "scope_testid", &mut projected);
            copy_semantic_raw_field(args, "scope_first", &mut projected);
            copy_semantic_raw_field(args, "scope_last", &mut projected);
            copy_semantic_raw_field(args, "scope_nth", &mut projected);
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        "scroll" => {
            let parsed: ScrollArgs = parse_json_args(args, "scroll").ok()?;
            Some(serde_json::json!({
                "direction": parse_optional_scroll_direction(parsed.direction.as_deref(), "direction").ok()?,
                "amount": parsed.amount,
            }))
        }
        "reload" => {
            let parsed: ReloadArgs = parse_json_args(args, "reload").ok()?;
            Some(serde_json::json!({
                "load_strategy": parse_optional_load_strategy(parsed.load_strategy.as_deref(), "load_strategy").ok()?,
                "wait_after": args.get("wait_after").cloned(),
            }))
        }
        "screenshot" => {
            let parsed: self::args::ScreenshotArgs = parse_json_args(args, "screenshot").ok()?;
            Some(serde_json::json!({
                "full": parsed.full,
                "highlight": parsed.highlight,
                "path": parsed.path,
            }))
        }
        "switch" => {
            let parsed: self::args::SwitchArgs = parse_json_args(args, "switch").ok()?;
            Some(serde_json::json!({
                "index": parsed.index,
                "wait_after": args.get("wait_after").cloned(),
            }))
        }
        "close-tab" => {
            let parsed: self::args::CloseTabArgs = parse_json_args(args, "close-tab").ok()?;
            Some(serde_json::json!({
                "index": parsed.index,
            }))
        }
        _ => None,
    }
}

fn copy_semantic_raw_field(
    args: &serde_json::Value,
    key: &str,
    projected: &mut serde_json::Map<String, serde_json::Value>,
) {
    if let Some(value) = args.get(key) {
        projected.insert(key.to_string(), value.clone());
    }
}

#[cfg(test)]
mod tests;
