use super::super::element_semantics::semantic_role;
use super::super::navigation::write_screenshot_artifact;
use super::super::state_format::summarize_element_label;
use super::super::{DaemonRouter, RubError};

#[derive(Debug, serde::Serialize)]
pub(super) struct ObserveElementMapEntry {
    index: u32,
    depth: u32,
    role: String,
    label: String,
    bbox: rub_core::model::BoundingBox,
}

pub(super) fn count_summary_lines(summary: &str) -> usize {
    summary
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

pub(super) fn build_element_map(
    snapshot: &rub_core::model::Snapshot,
) -> Vec<ObserveElementMapEntry> {
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

pub(super) async fn capture_screenshot_payload(
    router: &DaemonRouter,
    full: bool,
    path: Option<&str>,
) -> Result<serde_json::Value, RubError> {
    let png_bytes = router.browser.screenshot(full).await?;
    if let Some(path) = path {
        return write_screenshot_artifact(
            path,
            &png_bytes,
            "router.observe_capture_artifact",
            "observe_capture_artifact",
        );
    }

    if super::super::navigation::inline_screenshot_payload_exceeds_limit(png_bytes.len()) {
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
