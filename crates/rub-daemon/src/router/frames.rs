use super::*;
use crate::runtime_refresh::refresh_live_frame_runtime;
use rub_core::error::ErrorCode;
use rub_core::model::FrameInventoryEntry;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameSelectionArgs {
    #[serde(default)]
    top: bool,
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    name: Option<String>,
}

pub(super) async fn cmd_frames(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let frames = router.browser.list_frames().await?;
    state.apply_frame_inventory(&frames).await;
    let projected = state.project_frame_inventory(&frames).await;
    let runtime = state.frame_runtime().await;
    Ok(serde_json::json!({
        "subject": {
            "kind": "frame_inventory",
        },
        "result": {
            "current_frame": runtime.current_frame.clone(),
            "items": projected,
        },
        "runtime": runtime,
    }))
}

pub(super) async fn cmd_frame(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: FrameSelectionArgs = super::request_args::parse_json_args(args, "frame")?;
    refresh_live_frame_runtime(&router.browser, state).await;
    let frames = router.browser.list_frames().await?;
    let selected_frame_id = resolve_selected_frame_id(&parsed, &frames)?;
    state.select_frame(selected_frame_id).await;
    state.apply_frame_inventory(&frames).await;

    let frame_runtime = state.frame_runtime().await;
    let projected = state.project_frame_inventory(&frames).await;
    Ok(serde_json::json!({
        "subject": {
            "kind": "frame_selection",
            "selector": frame_selection_subject(&parsed),
        },
        "result": {
            "current_frame": frame_runtime.current_frame.clone(),
            "items": projected,
        },
        "runtime": frame_runtime,
    }))
}

fn frame_selection_subject(args: &FrameSelectionArgs) -> serde_json::Value {
    if args.top {
        return serde_json::json!({
            "kind": "top",
        });
    }
    if let Some(index) = args.index {
        return serde_json::json!({
            "kind": "index",
            "index": index,
        });
    }
    if let Some(name) = args.name.as_deref() {
        return serde_json::json!({
            "kind": "name",
            "name": name,
        });
    }
    serde_json::json!({
        "kind": "unknown",
    })
}

fn resolve_selected_frame_id(
    args: &FrameSelectionArgs,
    frames: &[FrameInventoryEntry],
) -> Result<Option<String>, RubError> {
    if args.top {
        return Ok(None);
    }

    if let Some(index) = args.index {
        let entry = frames
            .iter()
            .find(|entry| entry.index == index)
            .ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Frame index {index} is not present in the current inventory"),
                )
            })?;
        ensure_frame_switchable(entry)?;
        return Ok(Some(entry.frame.frame_id.clone()));
    }

    if let Some(name) = args.name.as_deref() {
        let trimmed = name.trim();
        let matches = frames
            .iter()
            .filter(|entry| entry.frame.name.as_deref() == Some(trimmed))
            .cloned()
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Frame named '{trimmed}' is not present in the current inventory"),
            ));
        }
        if matches.len() > 1 {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("Frame name '{trimmed}' is ambiguous"),
                serde_json::json!({
                    "name": trimmed,
                    "matching_indices": matches.iter().map(|entry| entry.index).collect::<Vec<_>>(),
                }),
            ));
        }
        let entry = matches.into_iter().next().ok_or_else(|| {
            RubError::Internal("Frame selection unexpectedly lost its single match".to_string())
        })?;
        ensure_frame_switchable(&entry)?;
        return Ok(Some(entry.frame.frame_id));
    }

    Err(RubError::domain(
        ErrorCode::InvalidInput,
        "frame requires exactly one selector: <index>, --name, or --top",
    ))
}

fn ensure_frame_switchable(entry: &FrameInventoryEntry) -> Result<(), RubError> {
    if entry.is_primary || matches!(entry.frame.same_origin_accessible, Some(true)) {
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!(
            "Frame '{}' is not same-origin accessible for frame-scoped snapshot and interaction",
            entry.frame.frame_id
        ),
        serde_json::json!({
            "frame_id": entry.frame.frame_id,
            "same_origin_accessible": entry.frame.same_origin_accessible,
            "index": entry.index,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::{FrameSelectionArgs, frame_selection_subject};
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;

    #[test]
    fn typed_frame_payload_rejects_unknown_fields() {
        let error = parse_json_args::<FrameSelectionArgs>(
            &serde_json::json!({
                "index": 1,
                "mystery": true,
            }),
            "frame",
        )
        .expect_err("unknown frame fields should be rejected")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn frame_selection_subject_prefers_typed_selector() {
        let selector = FrameSelectionArgs {
            top: false,
            index: Some(2),
            name: Some("ignored".to_string()),
        };
        assert_eq!(frame_selection_subject(&selector)["kind"], "index");
        assert_eq!(frame_selection_subject(&selector)["index"], 2);
    }
}
