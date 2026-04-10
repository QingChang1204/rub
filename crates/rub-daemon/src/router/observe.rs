mod args;
mod projection;

use self::args::{ObserveArgs, parse_observe_limit};
use self::projection::{build_element_map, capture_screenshot_payload, count_summary_lines};
use super::observation_filter::{
    ObservationProjectionMode, apply_observation_projection,
    attach_observation_projection_metadata, parse_observation_projection,
};
use super::observation_scope::{
    apply_observation_scope, apply_projection_limit, attach_scope_metadata, parse_observation_scope,
};
use super::projection::{attach_result, attach_subject, snapshot_entity};
use super::snapshot::build_stable_snapshot;
use super::state_format::{summarize_snapshot_a11y, summarize_snapshot_compact};
use super::*;

pub(super) async fn cmd_observe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
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
        build_stable_snapshot(router, args, state, deadline, capture_limit, true, false).await?;
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

#[cfg(test)]
mod tests {
    use super::args::{ObserveArgs, parse_observe_limit};
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

    #[test]
    fn typed_observe_payload_accepts_path_state_metadata() {
        let parsed = parse_json_args::<ObserveArgs>(
            &serde_json::json!({
                "path": "/tmp/observe.png",
                "path_state": {
                    "path_authority": "cli.observe.path"
                }
            }),
            "observe",
        )
        .expect("observe payload should accept display-only path metadata");
        assert_eq!(parsed.path.as_deref(), Some("/tmp/observe.png"));
    }
}
