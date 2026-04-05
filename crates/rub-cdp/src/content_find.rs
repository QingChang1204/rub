use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::ContentFindMatch;

use crate::live_dom_locator::LOCATOR_JS_HELPERS;

#[derive(Debug, serde::Deserialize)]
struct ContentFindPayload {
    locator_error: Option<String>,
    matches: Vec<ContentFindMatch>,
}

pub(crate) async fn find_content_matches(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<Vec<ContentFindMatch>, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(page, frame_id).await?;
    let script = content_find_script(locator)?;
    let payload: ContentFindPayload = serde_json::from_str(
        &crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            &script,
        )
        .await?,
    )
    .map_err(|error| RubError::Internal(format!("Parse content-find payload failed: {error}")))?;

    if let Some(locator_error) = payload.locator_error {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!("Invalid locator for content find: {locator_error}"),
            serde_json::json!({
                "locator": locator,
                "frame_id": frame_context.frame.frame_id,
            }),
            "Check the locator syntax, or run 'rub inspect page --format compact' to inspect nearby content anchors",
        ));
    }

    Ok(payload.matches)
}

fn content_find_script(locator: &LiveLocator) -> Result<String, RubError> {
    let locator = serde_json::to_string(locator).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize content-find locator: {error}"),
        )
    })?;

    Ok(format!(
        r#"JSON.stringify((() => {{
            const locator = {locator};
            {LOCATOR_JS_HELPERS}
            const describeMatch = (el) => {{
                const testid = testingId(el);
                return {{
                    tag_name: String(el.tagName || '').toLowerCase(),
                    text: String(el.textContent || '').replace(/\s+/g, ' ').trim(),
                    role: semanticRole(el),
                    label: accessibleLabel(el),
                    testid: testid || null,
                }};
            }};

            try {{
                const matches = resolveLocatorMatches(locator);
                const selected = selectMatches(matches, locator.selection);
                return {{
                    locator_error: null,
                    matches: selected.map(describeMatch),
                }};
            }} catch (error) {{
                return {{
                    locator_error: String(error && error.message ? error.message : error),
                    matches: [],
                }};
            }}
        }})())"#
    ))
}

#[cfg(test)]
mod tests {
    use super::content_find_script;
    use rub_core::locator::{CanonicalLocator, LiveLocator, LocatorSelection};

    #[test]
    fn content_find_script_serializes_target_text_locator_and_selection() {
        let locator = LiveLocator::try_from(CanonicalLocator::TargetText {
            text: "External links".to_string(),
            selection: Some(LocatorSelection::First),
        })
        .expect("target_text should be a valid live locator");
        let script = content_find_script(&locator).expect("content find script should serialize");

        assert!(script.contains("\"kind\":\"target_text\""));
        assert!(script.contains("\"text\":\"External links\""));
        assert!(script.contains("\"selection\":\"first\""));
        assert!(script.contains("matches: selected.map(describeMatch)"));
    }
}
