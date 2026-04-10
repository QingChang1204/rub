use base64::Engine;

use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes;
use rub_ipc::codec::MAX_FRAME_BYTES;

use crate::router::DaemonRouter;
use crate::router::artifacts::{annotate_file_artifact_state, output_artifact_durability};
use crate::router::request_args::parse_json_args;

use super::args::ScreenshotArgs;

const INLINE_SCREENSHOT_RESPONSE_OVERHEAD_BYTES: usize = 64 * 1024;

pub(crate) async fn cmd_screenshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    let parsed: ScreenshotArgs = parse_json_args(args, "screenshot")?;
    let full_page = parsed.full;
    let highlight = parsed.highlight;

    let highlight_info = if highlight {
        let snapshot = router.browser.snapshot(Some(0)).await?;
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
        write_screenshot_artifact(
            path,
            &png_bytes,
            "router.screenshot_artifact",
            "page_screenshot_result",
        )?
    } else {
        ensure_inline_screenshot_fits_protocol(png_bytes.len())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        serde_json::json!({
            "kind": "screenshot",
            "format": "png",
            "base64": b64,
            "size_bytes": png_bytes.len(),
        })
    };
    let mut data = serde_json::json!({});
    crate::router::projection::attach_subject(
        &mut data,
        serde_json::json!({
            "kind": "page_view",
            "full_page": full_page,
        }),
    );
    crate::router::projection::attach_result(
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

pub(crate) fn write_screenshot_artifact(
    path: &str,
    png_bytes: &[u8],
    artifact_authority: &str,
    upstream_truth: &str,
) -> Result<serde_json::Value, RubError> {
    let commit_outcome = atomic_write_bytes(std::path::Path::new(path), png_bytes, 0o600)
        .map_err(|error| RubError::Internal(format!("Cannot write screenshot file: {error}")))?;
    let mut artifact = serde_json::json!({
        "kind": "screenshot",
        "format": "png",
        "output_path": path,
        "size_bytes": png_bytes.len(),
    });
    annotate_file_artifact_state(
        &mut artifact,
        artifact_authority,
        upstream_truth,
        output_artifact_durability(commit_outcome),
    );
    Ok(artifact)
}

pub(crate) fn inline_screenshot_payload_exceeds_limit(png_bytes_len: usize) -> bool {
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
