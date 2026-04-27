use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct TextPresencePayload {
    found: bool,
}

pub(crate) async fn page_has_text(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    text: &str,
) -> Result<bool, RubError> {
    let needle = serde_json::to_string(text).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize trigger text probe: {error}"),
        )
    })?;

    let script = format!(
        r#"JSON.stringify((() => {{
            const needle = {needle};
            const text = String(
                (document.body && document.body.innerText)
                || (document.documentElement && document.documentElement.innerText)
                || ''
            ).replace(/\s+/g, ' ').trim();
            return {{ found: needle.length > 0 && text.includes(needle) }};
        }})())"#
    );

    let frame_context = crate::frame_runtime::resolve_frame_context(page, frame_id).await?;
    let document_before = crate::runtime_state::probe_live_read_document_fence(
        page,
        frame_context.execution_context_id,
    )
    .await;
    let raw = serde_json::from_value::<String>(
        crate::evaluation::execute_js_in_context(
            page,
            script.as_str(),
            frame_context.execution_context_id,
        )
        .await?,
    )
    .map_err(|error| {
        RubError::Internal(format!("Parse trigger text-probe payload failed: {error}"))
    })?;
    let document_after = crate::runtime_state::probe_live_read_document_fence(
        page,
        frame_context.execution_context_id,
    )
    .await;
    crate::runtime_state::ensure_live_read_document_fence(
        "trigger_text_probe",
        frame_context.frame.frame_id.as_str(),
        document_before.as_ref(),
        document_after.as_ref(),
    )?;

    let payload: TextPresencePayload = serde_json::from_str(&raw).map_err(|error| {
        RubError::Internal(format!("Parse trigger text-probe payload failed: {error}"))
    })?;
    Ok(payload.found)
}

#[cfg(test)]
mod tests {
    use super::TextPresencePayload;

    #[test]
    fn text_presence_payload_deserializes_boolean_shape() {
        let payload: TextPresencePayload =
            serde_json::from_str(r#"{"found":true}"#).expect("payload");
        assert!(payload.found);
    }
}
