use std::collections::HashMap;
use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::locator::{CanonicalLocator, LiveLocator};
use rub_core::model::BoundingBox;
use serde::de::DeserializeOwned;

use crate::live_dom_locator::LOCATOR_JS_HELPERS;

#[derive(Debug, Clone, Copy)]
enum ReadQueryKind {
    Text,
    Html,
    Value,
    Attributes,
    Bbox,
}

impl ReadQueryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Html => "html",
            Self::Value => "value",
            Self::Attributes => "attributes",
            Self::Bbox => "bbox",
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct ReadQueryPayload {
    locator_error: Option<String>,
    match_count: usize,
    selected_count: usize,
    value: Option<serde_json::Value>,
    values: Vec<serde_json::Value>,
}

fn read_query_invalid_locator_suggestion(locator: &LiveLocator) -> &'static str {
    match locator.as_canonical() {
        CanonicalLocator::Selector { .. } => {
            "Check the CSS selector syntax, or switch to --role/--label/--testid. Run 'rub inspect page --format compact' to inspect nearby content"
        }
        _ => {
            "Check the locator value, or run 'rub inspect page --format compact' to inspect the current content root"
        }
    }
}

fn read_query_not_found_suggestion(locator: &LiveLocator) -> &'static str {
    match locator.as_canonical() {
        CanonicalLocator::Selector { .. } => {
            "Verify the selector in the current frame, or switch to --role/--label/--testid for a more stable locator. Run 'rub inspect page --format compact' to inspect nearby content"
        }
        _ => {
            "Run 'rub inspect page --format compact' to inspect the current content root, then refine the locator or add --first/--last/--nth if multiple matches are expected"
        }
    }
}

pub(crate) async fn query_text(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<String, RubError> {
    execute_read_query(page, frame_id, locator, ReadQueryKind::Text).await
}

pub(crate) async fn query_text_many(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<Vec<String>, RubError> {
    execute_read_query_many(page, frame_id, locator, ReadQueryKind::Text).await
}

pub(crate) async fn query_html(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<String, RubError> {
    execute_read_query(page, frame_id, locator, ReadQueryKind::Html).await
}

pub(crate) async fn query_html_many(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<Vec<String>, RubError> {
    execute_read_query_many(page, frame_id, locator, ReadQueryKind::Html).await
}

pub(crate) async fn query_value(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<String, RubError> {
    execute_read_query(page, frame_id, locator, ReadQueryKind::Value).await
}

pub(crate) async fn query_value_many(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<Vec<String>, RubError> {
    execute_read_query_many(page, frame_id, locator, ReadQueryKind::Value).await
}

pub(crate) async fn query_attributes(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<HashMap<String, String>, RubError> {
    execute_read_query(page, frame_id, locator, ReadQueryKind::Attributes).await
}

pub(crate) async fn query_attributes_many(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<Vec<HashMap<String, String>>, RubError> {
    execute_read_query_many(page, frame_id, locator, ReadQueryKind::Attributes).await
}

pub(crate) async fn query_bbox(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<BoundingBox, RubError> {
    execute_read_query(page, frame_id, locator, ReadQueryKind::Bbox).await
}

pub(crate) async fn query_bbox_many(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
) -> Result<Vec<BoundingBox>, RubError> {
    execute_read_query_many(page, frame_id, locator, ReadQueryKind::Bbox).await
}

async fn execute_read_query<T: DeserializeOwned>(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
    kind: ReadQueryKind,
) -> Result<T, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(page, frame_id).await?;
    let script = read_query_script(locator, kind)?;
    let payload: ReadQueryPayload = serde_json::from_str(
        &crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            &script,
        )
        .await?,
    )
    .map_err(|error| RubError::Internal(format!("Parse read query payload failed: {error}")))?;

    if let Some(locator_error) = payload.locator_error {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "Invalid locator for {} query: {locator_error}",
                kind.as_str()
            ),
            serde_json::json!({
                "kind": kind.as_str(),
                "locator": locator,
                "frame_id": frame_context.frame.frame_id,
            }),
            read_query_invalid_locator_suggestion(locator),
        ));
    }

    if payload.selected_count == 0 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!(
                "Read-only {} query did not resolve to any live DOM element",
                kind.as_str()
            ),
            serde_json::json!({
                "kind": kind.as_str(),
                "locator": locator,
                "match_count": payload.match_count,
                "frame_id": frame_context.frame.frame_id,
            }),
            read_query_not_found_suggestion(locator),
        ));
    }

    if payload.match_count > 1 && locator.selection().is_none() {
        let context = serde_json::json!({
            "kind": kind.as_str(),
            "locator": locator,
            "match_count": payload.match_count,
            "frame_id": frame_context.frame.frame_id,
        });
        return Err(RubError::Domain(
            ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                format!(
                    "Read-only {} query matched {} live DOM elements; refine the locator",
                    kind.as_str(),
                    payload.match_count
                ),
            )
            .with_context(context)
            .with_suggestion(
                "Refine the locator, or use --first, --last, or --nth to select a single match",
            ),
        ));
    }

    let value = payload.value.ok_or_else(|| {
        RubError::Internal(format!(
            "Read-only {} query returned no value for a selected element",
            kind.as_str()
        ))
    })?;
    serde_json::from_value(value)
        .map_err(|error| RubError::Internal(format!("Parse read query result failed: {error}")))
}

async fn execute_read_query_many<T: DeserializeOwned>(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    locator: &LiveLocator,
    kind: ReadQueryKind,
) -> Result<Vec<T>, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(page, frame_id).await?;
    let script = read_query_script(locator, kind)?;
    let payload: ReadQueryPayload = serde_json::from_str(
        &crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            &script,
        )
        .await?,
    )
    .map_err(|error| RubError::Internal(format!("Parse read query payload failed: {error}")))?;

    if let Some(locator_error) = payload.locator_error {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "Invalid locator for {} query: {locator_error}",
                kind.as_str()
            ),
            serde_json::json!({
                "kind": kind.as_str(),
                "locator": locator,
                "frame_id": frame_context.frame.frame_id,
            }),
            read_query_invalid_locator_suggestion(locator),
        ));
    }

    if payload.selected_count == 0 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!(
                "Read-only {} query did not resolve to any live DOM element",
                kind.as_str()
            ),
            serde_json::json!({
                "kind": kind.as_str(),
                "locator": locator,
                "match_count": payload.match_count,
                "frame_id": frame_context.frame.frame_id,
            }),
            read_query_not_found_suggestion(locator),
        ));
    }

    serde_json::from_value(serde_json::Value::Array(payload.values))
        .map_err(|error| RubError::Internal(format!("Parse read query results failed: {error}")))
}

fn read_query_script(locator: &LiveLocator, kind: ReadQueryKind) -> Result<String, RubError> {
    let locator = serde_json::to_string(locator).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize read-query locator: {error}"),
        )
    })?;
    let kind = serde_json::to_string(kind.as_str()).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize read-query kind: {error}"),
        )
    })?;

    Ok(format!(
        r#"JSON.stringify((() => {{
            const locator = {locator};
            const kind = {kind};
            {LOCATOR_JS_HELPERS}
            const readOne = (el) => {{
                switch (kind) {{
                    case 'text':
                        return String(el.textContent || '').replace(/\s+/g, ' ').trim();
                    case 'html':
                        return String(el.outerHTML || '');
                    case 'value':
                        return 'value' in el ? String(el.value ?? '') : '';
                    case 'attributes':
                        return Object.fromEntries(
                            Array.from(el.attributes || []).map(attr => [attr.name, attr.value])
                        );
                    case 'bbox': {{
                        const rect = el.getBoundingClientRect();
                        return {{
                            x: rect.x || 0,
                            y: rect.y || 0,
                            width: rect.width || 0,
                            height: rect.height || 0
                        }};
                    }}
                    default:
                        return null;
                }}
            }};

            try {{
                const matches = resolveLocatorMatches(locator);
                const selected = selectMatches(matches, locator.selection);
                return {{
                    locator_error: null,
                    match_count: matches.length,
                    selected_count: selected.length,
                    value: selected.length ? readOne(selected[0]) : null,
                    values: selected.map(readOne),
                }};
            }} catch (error) {{
                return {{
                    locator_error: String(error && error.message ? error.message : error),
                    match_count: 0,
                    selected_count: 0,
                    value: null,
                    values: [],
                }};
            }}
        }})())"#
    ))
}

#[cfg(test)]
mod tests {
    use super::{ReadQueryKind, read_query_script};
    use rub_core::locator::{CanonicalLocator, LiveLocator, LocatorSelection};

    #[test]
    fn read_query_script_serializes_semantic_locator_and_selection() {
        let locator = LiveLocator::try_from(CanonicalLocator::Label {
            label: "Search".to_string(),
            selection: Some(LocatorSelection::Nth(1)),
        })
        .expect("label should be a valid live locator");
        let script = read_query_script(&locator, ReadQueryKind::Text)
            .expect("read query script should serialize");

        assert!(script.contains("\"kind\":\"label\""));
        assert!(script.contains("\"label\":\"Search\""));
        assert!(script.contains("\"nth\":1"));
        assert!(script.contains("accessibleLabel"));
    }

    #[test]
    fn read_query_script_supports_html_kind() {
        let locator = LiveLocator::try_from(CanonicalLocator::Selector {
            css: "article".to_string(),
            selection: None,
        })
        .expect("selector should be a valid live locator");
        let script = read_query_script(&locator, ReadQueryKind::Html)
            .expect("read query script should serialize");

        assert!(script.contains("const kind = \"html\""));
        assert!(script.contains("outerHTML"));
        assert!(script.contains("values: selected.map(readOne)"));
    }
}
