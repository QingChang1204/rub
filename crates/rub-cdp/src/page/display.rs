use chromiumoxide::Page;
use rub_core::error::RubError;
use rub_core::model::Snapshot;
use serde::Deserialize;
use std::sync::Arc;

pub async fn viewport_dimensions(page: &Arc<Page>) -> Result<(f64, f64), RubError> {
    #[derive(Deserialize)]
    struct Viewport {
        w: f64,
        h: f64,
    }

    let result = page
        .evaluate("JSON.stringify({ w: window.innerWidth, h: window.innerHeight })")
        .await
        .map_err(|e| RubError::Internal(format!("viewport_dimensions failed: {e}")))?;
    let json_str = result
        .into_value::<String>()
        .map_err(|e| RubError::Internal(format!("viewport parse failed: {e}")))?;
    let viewport: Viewport = serde_json::from_str(&json_str)
        .map_err(|e| RubError::Internal(format!("viewport JSON parse failed: {e}")))?;
    Ok((viewport.w, viewport.h))
}

pub async fn highlight_elements(page: &Arc<Page>, snapshot: &Snapshot) -> Result<u32, RubError> {
    let script = crate::dom::highlight_overlay_js(snapshot)?;
    let result = page
        .evaluate(script)
        .await
        .map_err(|e| RubError::Internal(format!("highlight injection failed: {e}")))?;
    let value = result
        .into_value::<serde_json::Value>()
        .map_err(|e| RubError::Internal(format!("highlight count decode failed: {e}")))?;
    parse_highlight_count(value)
}

pub async fn cleanup_highlights(page: &Arc<Page>) -> Result<(), RubError> {
    page.evaluate(crate::dom::CLEANUP_HIGHLIGHT_JS)
        .await
        .map_err(|e| RubError::Internal(format!("highlight cleanup failed: {e}")))?;
    Ok(())
}

fn parse_highlight_count(value: serde_json::Value) -> Result<u32, RubError> {
    let count = value.as_f64().ok_or_else(|| {
        RubError::domain_with_context(
            rub_core::error::ErrorCode::InternalError,
            "Highlight overlay returned a non-numeric element count".to_string(),
            serde_json::json!({
                "reason": "highlight_count_invalid",
                "returned_value": value,
            }),
        )
    })?;
    if !count.is_finite() || count < 0.0 || count.fract() != 0.0 || count > u32::MAX as f64 {
        return Err(RubError::domain_with_context(
            rub_core::error::ErrorCode::InternalError,
            "Highlight overlay returned an out-of-range element count".to_string(),
            serde_json::json!({
                "reason": "highlight_count_out_of_range",
                "returned_value": count,
            }),
        ));
    }
    Ok(count as u32)
}

#[cfg(test)]
mod tests {
    use super::parse_highlight_count;

    #[test]
    fn parse_highlight_count_accepts_integer_number() {
        let count = parse_highlight_count(serde_json::json!(3)).expect("numeric count");
        assert_eq!(count, 3);
    }

    #[test]
    fn parse_highlight_count_rejects_non_numeric_output() {
        let error = parse_highlight_count(serde_json::json!({"count": 0}))
            .expect_err("non-numeric output must fail closed")
            .into_envelope();
        assert_eq!(error.code, rub_core::error::ErrorCode::InternalError);
        assert_eq!(
            error.context.expect("context")["reason"],
            serde_json::json!("highlight_count_invalid")
        );
    }

    #[test]
    fn parse_highlight_count_rejects_fractional_output() {
        let error = parse_highlight_count(serde_json::json!(1.5))
            .expect_err("fractional count must fail closed")
            .into_envelope();
        assert_eq!(
            error.context.expect("context")["reason"],
            serde_json::json!("highlight_count_out_of_range")
        );
    }

    #[test]
    fn parse_highlight_count_rejects_overflow_output() {
        let error = parse_highlight_count(serde_json::json!((u32::MAX as f64) + 1.0))
            .expect_err("overflow count must fail closed")
            .into_envelope();
        assert_eq!(
            error.context.expect("context")["reason"],
            serde_json::json!("highlight_count_out_of_range")
        );
    }
}
