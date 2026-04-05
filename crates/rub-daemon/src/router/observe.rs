use super::element_semantics::semantic_role;
use super::observation_filter::{
    ObservationProjectionMode, apply_observation_projection,
    attach_observation_projection_metadata, parse_observation_projection,
};
use super::observation_scope::{
    apply_observation_scope, apply_projection_limit, attach_scope_metadata, parse_observation_scope,
};
use super::projection::{attach_result, attach_subject, snapshot_entity};
use super::snapshot::build_stable_snapshot;
use super::state_format::{
    summarize_element_label, summarize_snapshot_a11y, summarize_snapshot_compact,
};
use super::*;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserveArgs {
    #[serde(default)]
    full: bool,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
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

#[derive(Debug, serde::Serialize)]
struct ObserveElementMapEntry {
    index: u32,
    depth: u32,
    role: String,
    label: String,
    bbox: rub_core::model::BoundingBox,
}

pub(super) async fn cmd_observe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ObserveArgs = super::request_args::parse_json_args(args, "observe")?;
    let limit = parse_observe_limit(parsed.limit)?;
    let full = parsed.full;
    let path = parsed.path.as_deref();
    let observation_scope = parse_observation_scope(args)?;
    let observation_projection = parse_observation_projection(args, false)?;

    let capture_limit =
        if observation_scope.is_some() || observation_projection.depth_limit.is_some() {
            Some(0)
        } else {
            limit
        };
    let mut snapshot =
        build_stable_snapshot(router, args, state, capture_limit, true, false).await?;
    let mut scope_metadata = None::<(rub_core::observation::ObservationScope, u32, u32)>;
    if let Some(scope) = observation_scope.as_ref() {
        let scoped = apply_observation_scope(router, snapshot, scope).await?;
        scope_metadata = Some((
            scoped.scope.clone(),
            scoped.scope_total_count,
            scoped.scope_match_count,
        ));
        snapshot = scoped.snapshot;
    }
    let projection_metadata = apply_observation_projection(&mut snapshot, observation_projection);
    if observation_scope.is_some() || observation_projection.depth_limit.is_some() {
        apply_projection_limit(&mut snapshot, limit);
    }
    let snapshot = state.cache_snapshot(snapshot).await;

    let highlighted_count = router.browser.highlight_elements(&snapshot).await?;
    let screenshot_result = capture_screenshot_payload(router, full, path).await;
    let cleanup_result = router.browser.cleanup_highlights().await;

    let screenshot = screenshot_result?;
    cleanup_result?;

    let snapshot_summary = match projection_metadata.mode {
        ObservationProjectionMode::Interactive => summarize_snapshot_a11y(&snapshot),
        ObservationProjectionMode::Compact => summarize_snapshot_compact(&snapshot),
    };
    let summary_line_count = count_summary_lines(&snapshot_summary);
    let summary_format = match projection_metadata.mode {
        ObservationProjectionMode::Interactive => "a11y",
        ObservationProjectionMode::Compact => "compact",
    };
    let summary_text = snapshot_summary.clone();

    let mut snapshot_result = snapshot_entity(&snapshot);
    let Some(snapshot_object) = snapshot_result.as_object_mut() else {
        return Err(RubError::Internal(
            "Failed to project observation snapshot result".to_string(),
        ));
    };
    snapshot_object.insert("format".to_string(), serde_json::json!(summary_format));
    snapshot_object.insert(
        "summary".to_string(),
        serde_json::json!({
            "format": summary_format,
            "text": snapshot_summary,
            "line_count": summary_line_count,
        }),
    );
    snapshot_object.insert(
        "entry_count".to_string(),
        serde_json::json!(snapshot.elements.len()),
    );
    snapshot_object.insert(
        "total_count".to_string(),
        serde_json::json!(snapshot.total_count),
    );
    snapshot_object.insert(
        "truncated".to_string(),
        serde_json::json!(snapshot.truncated),
    );
    snapshot_object.insert("scroll".to_string(), serde_json::json!(&snapshot.scroll));
    snapshot_object.insert(
        "element_map".to_string(),
        serde_json::json!(build_element_map(&snapshot)),
    );
    match projection_metadata.mode {
        ObservationProjectionMode::Interactive => {
            snapshot_object.insert(
                "a11y_text".to_string(),
                serde_json::Value::String(summary_text.clone()),
            );
            snapshot_object.insert(
                "a11y_lines".to_string(),
                serde_json::json!(summary_line_count),
            );
        }
        ObservationProjectionMode::Compact => {
            snapshot_object.insert(
                "compact_text".to_string(),
                serde_json::Value::String(summary_text.clone()),
            );
            snapshot_object.insert(
                "compact_lines".to_string(),
                serde_json::json!(summary_line_count),
            );
        }
    };
    if let Some((scope, scope_total_count, scope_match_count)) = scope_metadata {
        attach_scope_metadata(
            &mut snapshot_result,
            &scope,
            scope_total_count,
            scope_match_count,
        );
    }
    attach_observation_projection_metadata(&mut snapshot_result, projection_metadata);

    let mut response = serde_json::json!({});
    attach_subject(
        &mut response,
        serde_json::json!({
            "kind": "page_observation",
            "format": summary_format,
            "frame_id": snapshot.frame_context.frame_id,
            "viewport_only": false,
            "capture_artifact": "screenshot",
            "full_page": full,
        }),
    );
    attach_result(
        &mut response,
        serde_json::json!({
            "snapshot": snapshot_result,
            "highlight": {
                "highlighted_count": highlighted_count,
                "cleanup": true,
            },
        }),
    );
    if let Some(response_object) = response.as_object_mut() {
        response_object.insert("artifact".to_string(), screenshot);
    }

    Ok(response)
}

fn parse_observe_limit(limit: Option<u64>) -> Result<Option<u32>, RubError> {
    let Some(raw_limit) = limit else {
        return Ok(None);
    };
    let limit = u32::try_from(raw_limit).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "observe limit {raw_limit} exceeds the supported maximum {}",
                u32::MAX
            ),
        )
    })?;
    Ok(Some(limit))
}

fn count_summary_lines(summary: &str) -> usize {
    summary
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn build_element_map(snapshot: &rub_core::model::Snapshot) -> Vec<ObserveElementMapEntry> {
    snapshot
        .elements
        .iter()
        .filter_map(|element| {
            element.bounding_box.map(|bbox| ObserveElementMapEntry {
                index: element.index,
                depth: element.depth.unwrap_or(0),
                role: semantic_role(element),
                label: summarize_element_label(element),
                bbox,
            })
        })
        .collect()
}

async fn capture_screenshot_payload(
    router: &DaemonRouter,
    full: bool,
    path: Option<&str>,
) -> Result<serde_json::Value, RubError> {
    let png_bytes = router.browser.screenshot(full).await?;
    if let Some(path) = path {
        std::fs::write(path, &png_bytes)?;
        return Ok(serde_json::json!({
            "kind": "screenshot",
            "format": "png",
            "output_path": path,
            "size_bytes": png_bytes.len(),
        }));
    }

    if super::navigation::inline_screenshot_payload_exceeds_limit(png_bytes.len()) {
        return Ok(serde_json::json!({
            "kind": "screenshot",
            "format": "png",
            "available": false,
            "omitted_reason": "inline_frame_limit_exceeded",
            "size_bytes": png_bytes.len(),
            "suggestion": "Use --path to save the screenshot to disk",
        }));
    }

    use base64::Engine;
    Ok(serde_json::json!({
        "kind": "screenshot",
        "format": "png",
        "base64": base64::engine::general_purpose::STANDARD.encode(&png_bytes),
        "size_bytes": png_bytes.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{ObserveArgs, parse_observe_limit};
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;

    #[test]
    fn observe_limit_rejects_values_above_u32_max() {
        let args = serde_json::json!({
            "limit": (u64::from(u32::MAX) + 1),
        });
        let error =
            parse_observe_limit(args["limit"].as_u64()).expect_err("overflowing limit must fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn typed_observe_payload_rejects_unknown_fields() {
        let error = parse_json_args::<ObserveArgs>(
            &serde_json::json!({
                "full": true,
                "mystery": true,
            }),
            "observe",
        )
        .expect_err("unknown observe fields should fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }
}
