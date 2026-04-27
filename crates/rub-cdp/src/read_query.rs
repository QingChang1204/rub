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
    #[serde(default)]
    projection_error: Option<String>,
    match_count: usize,
    selected_count: usize,
    value: Option<serde_json::Value>,
    values: Vec<serde_json::Value>,
}

fn read_query_invalid_locator_suggestion(locator: &LiveLocator) -> &'static str {
    match locator.as_canonical() {
        CanonicalLocator::Selector { .. } => {
            "Check the CSS selector syntax, or switch to --role/--label/--testid. Run 'rub observe' to see available elements"
        }
        _ => "Check the locator value, or run 'rub observe' to see available elements",
    }
}

fn read_query_not_found_suggestion(locator: &LiveLocator) -> &'static str {
    match locator.as_canonical() {
        CanonicalLocator::Selector { .. } => {
            "Verify the selector in the current frame, or switch to --role/--label/--testid for a more stable locator. Run 'rub observe' to see available elements"
        }
        _ => {
            "Run 'rub observe' to see available elements, then refine the locator or add --first/--last/--nth if multiple matches are expected"
        }
    }
}

fn read_query_selection_out_of_range_suggestion(locator: &LiveLocator) -> &'static str {
    match locator.as_canonical() {
        CanonicalLocator::Selector { .. } => {
            "Verify the selector in the current frame, or remove --first/--last/--nth to inspect all matches before choosing one"
        }
        _ => {
            "Remove --first/--last/--nth to inspect all matches, or choose a selection that exists within the current live match set"
        }
    }
}

fn selection_out_of_range_read_error(
    locator: &LiveLocator,
    kind: ReadQueryKind,
    frame_id: &str,
    match_count: usize,
) -> RubError {
    RubError::domain_with_context_and_suggestion(
        ErrorCode::InvalidInput,
        format!(
            "Read-only {} query selection is out of range for the current live match set",
            kind.as_str()
        ),
        serde_json::json!({
            "kind": kind.as_str(),
            "locator": locator,
            "match_count": match_count,
            "selection": locator.selection(),
            "frame_id": frame_id,
        }),
        read_query_selection_out_of_range_suggestion(locator),
    )
}

fn read_query_projection_error(
    locator: &LiveLocator,
    kind: ReadQueryKind,
    frame_id: &str,
    match_count: usize,
    selected_count: usize,
    projection_error: String,
) -> RubError {
    if let Some(reason) = crate::targeting::top_level_geometry_error_reason(&projection_error) {
        return RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            "Top-level frame geometry authority is unavailable for the selected frame",
            serde_json::json!({
                "reason": reason,
                "authority": "top_level_frame_geometry",
                "kind": kind.as_str(),
                "locator": locator,
                "match_count": match_count,
                "selected_count": selected_count,
                "frame_id": frame_id,
                "projection_error": projection_error,
            }),
            "Reacquire frame/tab authority with 'rub state' or use a non-geometry read when cross-frame coordinates are unavailable",
        );
    }
    RubError::domain_with_context_and_suggestion(
        ErrorCode::InternalError,
        format!(
            "Read-only {} query matched a live DOM element but could not project the requested value: {projection_error}",
            kind.as_str()
        ),
        serde_json::json!({
            "reason": "read_query_projection_authority_unavailable",
            "kind": kind.as_str(),
            "locator": locator,
            "match_count": match_count,
            "selected_count": selected_count,
            "frame_id": frame_id,
            "projection_error": projection_error,
        }),
        "Reacquire frame/tab authority with 'rub state' or use a non-geometry read when cross-frame coordinates are unavailable",
    )
}

fn read_query_singular_contract_error(
    payload: &ReadQueryPayload,
    locator: &LiveLocator,
    kind: ReadQueryKind,
    frame_id: &str,
) -> Option<RubError> {
    if let Some(locator_error) = payload.locator_error.as_ref() {
        return Some(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "Invalid locator for {} query: {locator_error}",
                kind.as_str()
            ),
            serde_json::json!({
                "kind": kind.as_str(),
                "locator": locator,
                "frame_id": frame_id,
            }),
            read_query_invalid_locator_suggestion(locator),
        ));
    }

    if payload.selected_count == 0 {
        if payload.match_count > 0 && locator.selection().is_some() {
            return Some(selection_out_of_range_read_error(
                locator,
                kind,
                frame_id,
                payload.match_count,
            ));
        }
        return Some(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!(
                "Read-only {} query did not resolve to any live DOM element",
                kind.as_str()
            ),
            serde_json::json!({
                "kind": kind.as_str(),
                "locator": locator,
                "match_count": payload.match_count,
                "frame_id": frame_id,
            }),
            read_query_not_found_suggestion(locator),
        ));
    }

    if payload.match_count > 1 && locator.selection().is_none() {
        let context = serde_json::json!({
            "kind": kind.as_str(),
            "locator": locator,
            "match_count": payload.match_count,
            "frame_id": frame_id,
        });
        return Some(RubError::Domain(
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

    payload.projection_error.as_ref().map(|projection_error| {
        read_query_projection_error(
            locator,
            kind,
            frame_id,
            payload.match_count,
            payload.selected_count,
            projection_error.clone(),
        )
    })
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
    let raw = execute_read_query_with_document_fence(
        page,
        frame_context.execution_context_id,
        frame_context.frame.frame_id.as_str(),
        kind,
        &script,
    )
    .await?;
    let payload: ReadQueryPayload = serde_json::from_str(&raw)
        .map_err(|error| RubError::Internal(format!("Parse read query payload failed: {error}")))?;

    if let Some(error) = read_query_singular_contract_error(
        &payload,
        locator,
        kind,
        frame_context.frame.frame_id.as_str(),
    ) {
        return Err(error);
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
    let raw = execute_read_query_with_document_fence(
        page,
        frame_context.execution_context_id,
        frame_context.frame.frame_id.as_str(),
        kind,
        &script,
    )
    .await?;
    let payload: ReadQueryPayload = serde_json::from_str(&raw)
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
    if let Some(projection_error) = payload.projection_error {
        return Err(read_query_projection_error(
            locator,
            kind,
            frame_context.frame.frame_id.as_str(),
            payload.match_count,
            payload.selected_count,
            projection_error,
        ));
    }

    if payload.selected_count == 0 {
        if payload.match_count > 0 && locator.selection().is_some() {
            return Err(selection_out_of_range_read_error(
                locator,
                kind,
                frame_context.frame.frame_id.as_str(),
                payload.match_count,
            ));
        }
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

async fn execute_read_query_with_document_fence(
    page: &Arc<Page>,
    execution_context_id: Option<chromiumoxide::cdp::js_protocol::runtime::ExecutionContextId>,
    frame_id: &str,
    kind: ReadQueryKind,
    script: &str,
) -> Result<String, RubError> {
    let document_before =
        crate::runtime_state::probe_live_read_document_fence(page, execution_context_id).await;
    let raw =
        crate::js::evaluate_returning_string_in_context(page, execution_context_id, script).await?;
    let document_after =
        crate::runtime_state::probe_live_read_document_fence(page, execution_context_id).await;
    crate::runtime_state::ensure_live_read_document_fence(
        kind.as_str(),
        frame_id,
        document_before.as_ref(),
        document_after.as_ref(),
    )?;
    Ok(raw)
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
            {top_level_bbox_helpers}
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
                        return topLevelBoundingBox(el);
                    }}
                    default:
                        return null;
                }}
            }};

            try {{
                const matches = resolveLocatorMatches(locator);
                const selected = selectMatches(matches, locator.selection);
                try {{
                    const values = selected.map(readOne);
                    return {{
                        locator_error: null,
                        projection_error: null,
                        match_count: matches.length,
                        selected_count: selected.length,
                        value: selected.length ? values[0] : null,
                        values,
                    }};
                }} catch (error) {{
                    return {{
                        locator_error: null,
                        projection_error: String(error && error.message ? error.message : error),
                        match_count: matches.length,
                        selected_count: selected.length,
                        value: null,
                        values: [],
                    }};
                }}
            }} catch (error) {{
                return {{
                    locator_error: String(error && error.message ? error.message : error),
                    projection_error: null,
                    match_count: 0,
                    selected_count: 0,
                    value: null,
                    values: [],
                }};
            }}
        }})())"#,
        top_level_bbox_helpers = crate::targeting::TOP_LEVEL_HIT_TEST_HELPERS
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ReadQueryKind, ReadQueryPayload, read_query_projection_error, read_query_script,
        read_query_singular_contract_error, selection_out_of_range_read_error,
    };
    use rub_core::error::ErrorCode;
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
        assert!(script.contains("const values = selected.map(readOne)"));
        assert!(script.contains("projection_error"), "{script}");
    }

    #[test]
    fn read_query_bbox_uses_top_level_coordinate_projection() {
        let locator = LiveLocator::try_from(CanonicalLocator::Selector {
            css: ".card".to_string(),
            selection: Some(LocatorSelection::First),
        })
        .expect("selector should be a valid live locator");
        let script = read_query_script(&locator, ReadQueryKind::Bbox)
            .expect("read query script should serialize");

        assert!(script.contains("topLevelBoundingBox(el)"), "{script}");
        assert!(script.contains("current.frameElement"), "{script}");
        assert!(script.contains("current = current.parent"), "{script}");
        assert!(
            script.contains("top_level_bbox_parent_chain_unavailable"),
            "{script}"
        );
    }

    #[test]
    fn selection_out_of_range_read_error_is_invalid_input() {
        let locator = LiveLocator::try_from(CanonicalLocator::Label {
            label: "Search".to_string(),
            selection: Some(LocatorSelection::Nth(3)),
        })
        .expect("label should be a valid live locator");
        let envelope =
            selection_out_of_range_read_error(&locator, ReadQueryKind::Text, "frame-main", 2)
                .into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["match_count"].as_u64()),
            Some(2)
        );
    }

    #[test]
    fn read_query_projection_error_is_not_reported_as_invalid_locator() {
        let locator = LiveLocator::try_from(CanonicalLocator::Selector {
            css: ".card".to_string(),
            selection: Some(LocatorSelection::First),
        })
        .expect("selector should be a valid live locator");

        let envelope = read_query_projection_error(
            &locator,
            ReadQueryKind::Bbox,
            "frame-child",
            1,
            1,
            "top_level_bbox_parent_chain_unavailable".to_string(),
        )
        .into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["reason"].as_str()),
            Some("top_level_bbox_parent_chain_unavailable")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["match_count"].as_u64()),
            Some(1)
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["authority"].as_str()),
            Some("top_level_frame_geometry")
        );
    }

    #[test]
    fn singular_read_query_prioritizes_ambiguous_locator_over_projection_error() {
        let locator = LiveLocator::try_from(CanonicalLocator::Selector {
            css: ".card".to_string(),
            selection: None,
        })
        .expect("selector should be a valid live locator");
        let payload = ReadQueryPayload {
            locator_error: None,
            projection_error: Some("top_level_bbox_parent_chain_unavailable".to_string()),
            match_count: 2,
            selected_count: 2,
            value: None,
            values: Vec::new(),
        };

        let envelope = read_query_singular_contract_error(
            &payload,
            &locator,
            ReadQueryKind::Bbox,
            "frame-child",
        )
        .expect("ambiguous locator should fail before projection")
        .into_envelope();

        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(
            envelope.message.contains("matched 2 live DOM elements"),
            "unexpected message: {}",
            envelope.message
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["match_count"].as_u64()),
            Some(2)
        );
    }
}
