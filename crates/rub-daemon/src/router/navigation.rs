use super::observation_filter::{
    apply_observation_projection, attach_observation_projection_metadata,
    parse_observation_projection,
};
use super::observation_scope::{
    apply_observation_scope, apply_projection_limit, attach_scope_metadata, parse_observation_scope,
};
use super::projection::{
    attach_result, attach_subject, navigation_subject, page_entity, tab_entity, tab_subject,
    viewport_subject,
};
use super::request_args::parse_json_args;
use super::snapshot::{
    ExternalDomFenceOutcome, build_stable_snapshot, settle_external_dom_fence,
    sleep_full_settle_window,
};
use super::state_format::{StateFormat, project_snapshot};
use super::url_normalization::normalize_open_url;
use super::*;
use rub_core::model::TabInfo;
use rub_ipc::codec::MAX_FRAME_BYTES;

const INLINE_SCREENSHOT_RESPONSE_OVERHEAD_BYTES: usize = 64 * 1024;
const NAVIGATION_PROJECTION_SETTLE_RETRIES: usize = 6;
const NAVIGATION_PROJECTION_SETTLE_DELAY_MS: u64 = 100;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    url: String,
    #[serde(default)]
    load_strategy: Option<String>,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StateArgs {
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    a11y: bool,
    #[serde(default)]
    viewport: bool,
    #[serde(default)]
    diff: Option<String>,
    #[serde(default)]
    listeners: bool,
    #[serde(default, rename = "compact")]
    _compact: bool,
    #[serde(default, rename = "depth")]
    _depth: Option<u64>,
    #[serde(default, rename = "scope")]
    _scope: Option<serde_json::Value>,
    #[serde(default, rename = "scope_selector")]
    _scope_selector: Option<String>,
    #[serde(default, rename = "scope_role")]
    _scope_role: Option<String>,
    #[serde(default, rename = "scope_label")]
    _scope_label: Option<String>,
    #[serde(default, rename = "scope_testid")]
    _scope_testid: Option<String>,
    #[serde(default, rename = "scope_first")]
    _scope_first: bool,
    #[serde(default, rename = "scope_last")]
    _scope_last: bool,
    #[serde(default, rename = "scope_nth")]
    _scope_nth: Option<u64>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    amount: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReloadArgs {
    #[serde(default)]
    load_strategy: Option<String>,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    #[serde(default)]
    full: bool,
    #[serde(default)]
    highlight: bool,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SwitchArgs {
    index: u32,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseTabArgs {
    #[serde(default)]
    index: Option<u32>,
}

fn parse_optional_load_strategy(
    value: Option<&str>,
    name: &str,
) -> Result<rub_core::model::LoadStrategy, RubError> {
    let Some(value) = value else {
        return Ok(rub_core::model::LoadStrategy::default());
    };
    serde_json::from_value(serde_json::Value::String(value.to_string())).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Invalid {name} '{}'; expected one of: load, domcontentloaded, networkidle",
                value
            ),
        )
    })
}

fn parse_optional_scroll_direction(
    value: Option<&str>,
    name: &str,
) -> Result<rub_core::model::ScrollDirection, RubError> {
    let Some(value) = value else {
        return Ok(rub_core::model::ScrollDirection::Down);
    };
    serde_json::from_value(serde_json::Value::String(value.to_string())).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid {name} '{}'; expected one of: up, down", value),
        )
    })
}

async fn settle_navigation_projection(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    reason: &str,
    deadline: TransactionDeadline,
) -> PendingExternalDomCommit {
    state.clear_all_snapshots().await;
    state.select_frame(None).await;
    let pending_external_dom_commit =
        match settle_external_dom_fence(state, reason, Some(deadline)).await {
            ExternalDomFenceOutcome::Settled => PendingExternalDomCommit::Clear,
            ExternalDomFenceOutcome::IncompleteDueToDeadline
            | ExternalDomFenceOutcome::Unstable => PendingExternalDomCommit::Preserve,
        };

    for attempt in 0..NAVIGATION_PROJECTION_SETTLE_RETRIES {
        if deadline.remaining_duration().is_none() {
            tracing::debug!(
                reason,
                attempt,
                "settle_navigation_projection: deadline exhausted, stopping early"
            );
            return pending_external_dom_commit;
        }
        let tabs = router.browser.list_tabs().await.ok();
        if let Some(tabs) = tabs.as_ref() {
            state.adopt_interference_primary_context(tabs).await;
        }
        crate::runtime_refresh::refresh_live_frame_runtime(&router.browser, state).await;
        if active_tab_and_frame_runtime_converged(state, tabs.as_deref()).await {
            return pending_external_dom_commit;
        }
        if attempt + 1 < NAVIGATION_PROJECTION_SETTLE_RETRIES
            && !sleep_full_settle_window(Some(deadline), NAVIGATION_PROJECTION_SETTLE_DELAY_MS)
                .await
        {
            tracing::debug!(
                reason,
                attempt,
                "settle_navigation_projection: deadline too close for another full settle window, stopping early"
            );
            return pending_external_dom_commit;
        }
    }

    pending_external_dom_commit
}

fn is_page_load_timeout(error: &RubError) -> bool {
    matches!(error, RubError::Domain(envelope) if envelope.code == ErrorCode::PageLoadTimeout)
}

async fn active_tab_and_frame_runtime_converged(
    state: &Arc<SessionState>,
    tabs: Option<&[TabInfo]>,
) -> bool {
    let Some(active_tab) = tabs.and_then(|tabs| tabs.iter().find(|tab| tab.active)) else {
        return false;
    };
    state
        .frame_runtime()
        .await
        .current_frame
        .is_some_and(|frame| {
            frame.target_id.as_deref() == Some(active_tab.target_id.as_str())
                && frame.url.as_deref() == Some(active_tab.url.as_str())
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
    let mut snapshot =
        build_stable_snapshot(router, args, state, capture_limit, a11y, listeners).await?;
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

pub(super) async fn cmd_screenshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    let parsed: ScreenshotArgs = parse_json_args(args, "screenshot")?;
    let full_page = parsed.full;
    let highlight = parsed.highlight;

    let highlight_info = if highlight {
        let snapshot = router.browser.snapshot(None).await?;
        let count = router.browser.highlight_elements(&snapshot).await?;
        Some(count)
    } else {
        None
    };

    let screenshot_result = router.browser.screenshot(full_page).await;
    let highlight_cleanup_result = if highlight_info.is_some() {
        Some(router.browser.cleanup_highlights().await)
    } else {
        None
    };
    let png_bytes = match (screenshot_result, highlight_cleanup_result) {
        (Ok(bytes), Some(Ok(()))) => bytes,
        (Ok(bytes), None) => bytes,
        (Ok(_), Some(Err(cleanup_error))) => return Err(cleanup_error),
        (Err(screenshot_error), Some(Ok(()))) => return Err(screenshot_error),
        (Err(screenshot_error), Some(Err(cleanup_error))) => {
            return Err(RubError::domain_with_context(
                ErrorCode::InternalError,
                format!("Failed to capture screenshot: {screenshot_error}"),
                serde_json::json!({
                    "highlight_cleanup_error": cleanup_error.to_string(),
                }),
            ));
        }
        (Err(screenshot_error), None) => return Err(screenshot_error),
    };

    let highlight_requested = highlight_info.is_some();

    let artifact = if let Some(path) = parsed.path.as_deref() {
        std::fs::write(path, &png_bytes)?;
        serde_json::json!({
            "kind": "screenshot",
            "format": "png",
            "output_path": path,
            "size_bytes": png_bytes.len(),
        })
    } else {
        ensure_inline_screenshot_fits_protocol(png_bytes.len())?;
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        serde_json::json!({
            "kind": "screenshot",
            "format": "png",
            "base64": b64,
            "size_bytes": png_bytes.len(),
        })
    };
    let mut data = serde_json::json!({});
    attach_subject(
        &mut data,
        serde_json::json!({
            "kind": "page_view",
            "full_page": full_page,
        }),
    );
    attach_result(
        &mut data,
        serde_json::json!({
            "artifact": artifact,
            "highlight": {
                "requested": highlight_requested,
                "highlighted_count": highlight_info,
                "cleanup": highlight_requested,
            },
        }),
    );
    Ok(data)
}

pub(super) fn inline_screenshot_payload_exceeds_limit(png_bytes_len: usize) -> bool {
    let encoded_len = png_bytes_len.saturating_add(2) / 3 * 4;
    encoded_len.saturating_add(INLINE_SCREENSHOT_RESPONSE_OVERHEAD_BYTES) > MAX_FRAME_BYTES
}

fn ensure_inline_screenshot_fits_protocol(png_bytes_len: usize) -> Result<(), RubError> {
    if inline_screenshot_payload_exceeds_limit(png_bytes_len) {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "Inline screenshot payload exceeds IPC frame limit; save to a file with --path",
            serde_json::json!({
                "reason": "inline_screenshot_exceeds_ipc_frame_limit",
                "size_bytes": png_bytes_len,
                "max_frame_bytes": MAX_FRAME_BYTES,
            }),
        ));
    }
    Ok(())
}

pub(super) async fn cmd_tabs(router: &DaemonRouter) -> Result<serde_json::Value, RubError> {
    let tabs = router.browser.list_tabs().await?;
    let active_tab = tabs.iter().find(|tab| tab.active).map(tab_entity);
    Ok(serde_json::json!({
        "subject": {
            "kind": "tab_registry",
        },
        "result": {
            "items": tabs,
            "active_tab": active_tab,
        },
    }))
}

pub(super) async fn cmd_switch(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let parsed: SwitchArgs = parse_json_args(args, "switch")?;
    let tab = router.browser.switch_tab(parsed.index).await?;
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "switch", deadline).await;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, tab_subject(parsed.index));
    attach_result(
        &mut data,
        serde_json::json!({
            "active_tab": tab_entity(&tab),
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

pub(super) async fn cmd_close_tab(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let parsed: CloseTabArgs = parse_json_args(args, "close-tab")?;
    let index = parsed.index;
    let before_tabs = router.browser.list_tabs().await?;
    let closed_index = index.unwrap_or_else(|| {
        before_tabs
            .iter()
            .find(|tab| tab.active)
            .map(|tab| tab.index)
            .unwrap_or(0)
    });
    let tabs = router.browser.close_tab(index).await?;
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "close-tab", deadline).await;
    let active_tab = tabs.iter().find(|tab| tab.active).ok_or_else(|| {
        RubError::Internal("close-tab completed without an active tab".to_string())
    })?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, tab_subject(closed_index));
    attach_result(
        &mut data,
        serde_json::json!({
            "remaining_tabs": tabs.len(),
            "active_tab": tab_entity(active_tab),
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

async fn active_tab_entity(router: &DaemonRouter) -> Result<serde_json::Value, RubError> {
    let tabs = router.browser.list_tabs().await?;
    let active_tab = tabs.iter().find(|tab| tab.active).ok_or_else(|| {
        RubError::Internal("navigation completed without an active tab".to_string())
    })?;
    Ok(tab_entity(active_tab))
}

#[cfg(test)]
mod tests {
    use super::{
        OpenArgs, ScrollArgs, StateArgs, inline_screenshot_payload_exceeds_limit,
        parse_optional_load_strategy, parse_optional_scroll_direction,
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
            }),
            "open",
        )
        .expect("open payload should accept post-wait compatibility field");
        assert!(parsed._wait_after.is_some());
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
    fn inline_screenshot_limit_helper_flags_oversized_payloads() {
        assert!(!inline_screenshot_payload_exceeds_limit(1024));
        assert!(inline_screenshot_payload_exceeds_limit(7 * 1024 * 1024));
    }
}
