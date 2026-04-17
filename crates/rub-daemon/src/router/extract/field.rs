use serde::de::DeserializeOwned;

use super::ExtractAuthorityMode;
use super::spec::{ContentExtractPayload, ExtractFieldSpec, ExtractKind};
use crate::router::DaemonRouter;
use crate::router::addressing::resolve_elements_against_snapshot;
use crate::router::extract_postprocess::apply_postprocess;
use crate::router::request_args::{LocatorRequestArgs, locator_json};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};

#[derive(Clone, Copy)]
pub(super) enum ExtractMatchSurface<'a> {
    InteractiveField,
    ContentField {
        selector: &'a str,
    },
    CollectionRow {
        collection_name: &'a str,
        row_index: usize,
    },
}

pub(super) fn should_substitute_missing_field(field: &ExtractFieldSpec, error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(ErrorEnvelope {
            code: ErrorCode::ElementNotFound,
            ..
        })
    ) && (!field.required || field.default.is_some())
}

fn should_fallback_to_content(field: &ExtractFieldSpec, error: &RubError) -> bool {
    field.selector.is_some()
        && matches!(
            error,
            RubError::Domain(ErrorEnvelope {
                code: ErrorCode::ElementNotFound,
                ..
            })
        )
}

pub(super) fn apply_field_postprocess(
    name: &str,
    field: &ExtractFieldSpec,
    value: serde_json::Value,
) -> Result<serde_json::Value, RubError> {
    apply_postprocess(
        name,
        value,
        field.value_type,
        field.default.as_ref(),
        &field.map,
        field.transform,
    )
}

pub(super) async fn extract_field(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    field_name: &str,
    field: &ExtractFieldSpec,
    authority_mode: ExtractAuthorityMode,
) -> Result<serde_json::Value, RubError> {
    let locator_args = locator_json(LocatorRequestArgs {
        index: field.index,
        element_ref: field.element_ref.clone(),
        selector: field.selector.clone(),
        target_text: field.target_text.clone(),
        role: field.role.clone(),
        label: field.label.clone(),
        testid: field.testid.clone(),
        visible: false,
        prefer_enabled: false,
        topmost: false,
        first: field.first,
        last: field.last,
        nth: field.nth,
    });

    match resolve_elements_against_snapshot(router, snapshot, &locator_args, "extract").await {
        Ok(resolved) => {
            extract_field_value(
                field_name,
                router,
                &resolved.elements,
                field,
                ExtractMatchSurface::InteractiveField,
                authority_mode,
            )
            .await
        }
        Err(error)
            if authority_mode == ExtractAuthorityMode::LiveAllowed
                && should_fallback_to_content(field, &error) =>
        {
            let selector = field.selector.as_deref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::ElementNotFound,
                    "extract field did not resolve to any content element",
                )
            })?;
            extract_content_field_value(router, snapshot, field_name, selector, field).await
        }
        Err(error)
            if authority_mode == ExtractAuthorityMode::SnapshotOnly
                && should_fallback_to_content(field, &error) =>
        {
            let selector = field.selector.as_deref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::ElementNotFound,
                    "extract field did not resolve to any content element",
                )
            })?;
            Err(snapshot_extract_content_fallback_error(
                field_name, selector, field,
            ))
        }
        Err(error) => Err(error),
    }
}

async fn extract_field_value(
    field_name: &str,
    router: &DaemonRouter,
    elements: &[rub_core::model::Element],
    field: &ExtractFieldSpec,
    surface: ExtractMatchSurface<'_>,
    authority_mode: ExtractAuthorityMode,
) -> Result<serde_json::Value, RubError> {
    if !field.many && elements.len() > 1 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            extract_multi_match_message(field_name, elements.len(), surface),
            extract_multi_match_context(field_name, field, elements.len(), surface),
            extract_multi_match_suggestion(surface),
        ));
    }

    if field.many {
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(extract_single_value(router, element, field, authority_mode).await?);
        }
        return Ok(serde_json::Value::Array(values));
    }

    let element = elements.first().ok_or_else(|| {
        RubError::domain(
            ErrorCode::ElementNotFound,
            "extract field did not resolve to any interactive snapshot element",
        )
    })?;
    extract_single_value(router, element, field, authority_mode).await
}

async fn extract_single_value(
    router: &DaemonRouter,
    element: &rub_core::model::Element,
    field: &ExtractFieldSpec,
    authority_mode: ExtractAuthorityMode,
) -> Result<serde_json::Value, RubError> {
    if authority_mode == ExtractAuthorityMode::SnapshotOnly {
        return extract_single_snapshot_value(element, field);
    }
    match field.kind {
        ExtractKind::Text => Ok(serde_json::json!(router.browser.get_text(element).await?)),
        ExtractKind::Value => Ok(serde_json::json!(router.browser.get_value(element).await?)),
        ExtractKind::Html => Ok(serde_json::json!(
            router.browser.get_outer_html(element).await?
        )),
        ExtractKind::Bbox => {
            serde_json::to_value(router.browser.get_bbox(element).await?).map_err(RubError::from)
        }
        ExtractKind::Attributes => {
            serde_json::to_value(router.browser.get_attributes(element).await?)
                .map_err(RubError::from)
        }
        ExtractKind::Attribute => {
            let attribute_name = field.attribute.as_deref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "extract field kind 'attribute' requires an 'attribute' name",
                )
            })?;
            let attributes = router.browser.get_attributes(element).await?;
            Ok(match attributes.get(attribute_name) {
                Some(value) => serde_json::json!(value),
                None => serde_json::Value::Null,
            })
        }
    }
}

fn extract_single_snapshot_value(
    element: &rub_core::model::Element,
    field: &ExtractFieldSpec,
) -> Result<serde_json::Value, RubError> {
    match field.kind {
        ExtractKind::Text => Ok(serde_json::json!(element.text)),
        ExtractKind::Value => Ok(serde_json::json!(
            element.attributes.get("value").cloned().unwrap_or_default()
        )),
        ExtractKind::Html => Err(snapshot_extract_live_only_kind_error(
            field.kind,
            element.index,
        )),
        ExtractKind::Bbox => element
            .bounding_box
            .map(serde_json::to_value)
            .transpose()
            .map_err(RubError::from)?
            .ok_or_else(|| snapshot_bbox_unavailable_error(element.index)),
        ExtractKind::Attributes => {
            serde_json::to_value(&element.attributes).map_err(RubError::from)
        }
        ExtractKind::Attribute => {
            let attribute_name = field.attribute.as_deref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "extract field kind 'attribute' requires an 'attribute' name",
                )
            })?;
            Ok(match element.attributes.get(attribute_name) {
                Some(value) => serde_json::json!(value),
                None => serde_json::Value::Null,
            })
        }
    }
}

fn snapshot_extract_content_fallback_error(
    field_name: &str,
    selector: &str,
    field: &ExtractFieldSpec,
) -> RubError {
    RubError::domain_with_context_and_suggestion(
        ErrorCode::ElementNotFound,
        format!(
            "snapshot-addressed extract cannot resolve field '{field_name}' because it would need a live content fallback outside snapshot authority"
        ),
        serde_json::json!({
            "field": field_name,
            "selector": selector,
            "kind": field.kind.as_str(),
            "authority_state": "snapshot_extract_live_content_fallback_required",
        }),
        "Remove --snapshot-id to allow live content extraction, or use index/ref or a locator that resolves directly to an interactive snapshot element",
    )
}

fn snapshot_extract_live_only_kind_error(kind: ExtractKind, element_index: u32) -> RubError {
    RubError::domain_with_context_and_suggestion(
        ErrorCode::InvalidInput,
        format!(
            "snapshot-addressed extract cannot read {} for element {element_index} because that field kind requires live DOM authority",
            kind.as_str()
        ),
        serde_json::json!({
            "element_index": element_index,
            "kind": kind.as_str(),
            "authority_state": "snapshot_extract_live_only_field_kind",
        }),
        "Remove --snapshot-id to allow a live DOM read for this field kind",
    )
}

fn snapshot_bbox_unavailable_error(element_index: u32) -> RubError {
    RubError::domain_with_context_and_suggestion(
        ErrorCode::InvalidInput,
        format!(
            "snapshot-addressed extract cannot read bbox for element {element_index} because the cached snapshot does not carry a bounding box for that element"
        ),
        serde_json::json!({
            "element_index": element_index,
            "authority_state": "snapshot_extract_bbox_unavailable",
        }),
        "Remove --snapshot-id to allow a live bbox read, or refresh state and target an element with snapshot bbox projection",
    )
}

async fn extract_content_field_value(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    field_name: &str,
    selector: &str,
    field: &ExtractFieldSpec,
) -> Result<serde_json::Value, RubError> {
    let script = build_content_extract_script(selector, field)?;
    let payload: ContentExtractPayload =
        execute_json_payload_in_frame(router, snapshot, &script, "content").await?;

    if let Some(selector_error) = payload.selector_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid selector for extract content path: {selector_error}"),
            serde_json::json!({
                "selector": selector,
                "kind": field.kind.as_str(),
            }),
        ));
    }

    if payload.selected_count == 0 {
        return Err(RubError::domain_with_context(
            ErrorCode::ElementNotFound,
            "extract field did not resolve to any content element",
            serde_json::json!({
                "selector": selector,
                "kind": field.kind.as_str(),
                "match_count": payload.match_count,
            }),
        ));
    }

    if !field.many && payload.match_count > 1 && !field_has_selection(field) {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            extract_multi_match_message(
                field_name,
                payload.match_count,
                ExtractMatchSurface::ContentField { selector },
            ),
            extract_multi_match_context(
                field_name,
                field,
                payload.match_count,
                ExtractMatchSurface::ContentField { selector },
            ),
            extract_multi_match_suggestion(ExtractMatchSurface::ContentField { selector }),
        ));
    }

    if field.many {
        return Ok(serde_json::Value::Array(payload.values));
    }

    payload.values.into_iter().next().ok_or_else(|| {
        RubError::domain(
            ErrorCode::ElementNotFound,
            "extract field did not resolve to any content element",
        )
    })
}

pub(super) fn extract_multi_match_message(
    field_name: &str,
    match_count: usize,
    surface: ExtractMatchSurface<'_>,
) -> String {
    match surface {
        ExtractMatchSurface::InteractiveField => format!(
            "extract field '{field_name}' matched {match_count} elements; add first/last/nth, set 'many: true', or narrow the locator"
        ),
        ExtractMatchSurface::ContentField { .. } => format!(
            "extract field '{field_name}' matched {match_count} content elements; add first/last/nth, set 'many: true', or narrow the selector"
        ),
        ExtractMatchSurface::CollectionRow {
            collection_name,
            row_index,
        } => format!(
            "collection field '{field_name}' matched {match_count} elements in row {row_index} of '{collection_name}'; add first/last/nth, set 'many: true', or narrow the row-scoped locator"
        ),
    }
}

pub(super) fn extract_multi_match_suggestion(surface: ExtractMatchSurface<'_>) -> &'static str {
    match surface {
        ExtractMatchSurface::InteractiveField => {
            "Use `many: true` to collect every match, add `first`, `last`, or `nth` to pick one, or narrow the locator to the specific repeated card/content you want"
        }
        ExtractMatchSurface::ContentField { .. } => {
            "Use `many: true` to collect every content match, add `first`, `last`, or `nth` to pick one, or narrow the selector to the specific repeated content you want"
        }
        ExtractMatchSurface::CollectionRow { .. } => {
            "Use `many: true` to collect every row-local match, add `first`, `last`, or `nth` to pick one, or narrow the row-scoped selector/role/label/testid inside the repeated card or list row"
        }
    }
}

pub(super) fn extract_multi_match_context(
    field_name: &str,
    field: &ExtractFieldSpec,
    match_count: usize,
    surface: ExtractMatchSurface<'_>,
) -> serde_json::Value {
    let mut context = serde_json::Map::from_iter([
        ("field".to_string(), serde_json::json!(field_name)),
        ("kind".to_string(), serde_json::json!(field.kind.as_str())),
        ("match_count".to_string(), serde_json::json!(match_count)),
        ("locator".to_string(), extract_field_locator_context(field)),
        (
            "resolution_examples".to_string(),
            serde_json::json!({
                "pick_first": { "first": true },
                "pick_last": { "last": true },
                "pick_nth": { "nth": 0 },
                "collect_all": { "many": true }
            }),
        ),
    ]);
    if let Some(builder_examples) = extract_builder_field_examples(field_name, field) {
        context.insert("builder_field_examples".to_string(), builder_examples);
    }

    match surface {
        ExtractMatchSurface::InteractiveField => {
            context.insert("surface".to_string(), serde_json::json!("interactive"));
        }
        ExtractMatchSurface::ContentField { selector } => {
            context.insert("surface".to_string(), serde_json::json!("content"));
            context.insert("selector".to_string(), serde_json::json!(selector));
        }
        ExtractMatchSurface::CollectionRow {
            collection_name,
            row_index,
        } => {
            context.insert("surface".to_string(), serde_json::json!("collection_row"));
            context.insert("collection".to_string(), serde_json::json!(collection_name));
            context.insert("row_index".to_string(), serde_json::json!(row_index));
        }
    }

    serde_json::Value::Object(context)
}

pub(super) fn extract_builder_field_examples(
    field_name: &str,
    field: &ExtractFieldSpec,
) -> Option<serde_json::Value> {
    let locator = builder_locator_expression(field)?;
    let kind = match field.kind {
        ExtractKind::Text => format!("text:{locator}"),
        ExtractKind::Html => format!("html:{locator}"),
        ExtractKind::Value => format!("value:{locator}"),
        ExtractKind::Attributes => format!("attributes:{locator}"),
        ExtractKind::Bbox => format!("bbox:{locator}"),
        ExtractKind::Attribute => format!("attribute:{}:{locator}", field.attribute.as_deref()?),
    };
    Some(serde_json::json!({
        "pick_first": format!("{field_name}={kind}@first"),
        "pick_last": format!("{field_name}={kind}@last"),
        "pick_nth": format!("{field_name}={kind}@nth(0)"),
        "collect_all": format!("{field_name}={kind}@many"),
    }))
}

pub(super) fn builder_locator_expression(field: &ExtractFieldSpec) -> Option<String> {
    if let Some(selector) = field.selector.as_deref().map(str::trim)
        && !selector.is_empty()
    {
        return Some(selector.to_string());
    }
    if let Some(target_text) = field.target_text.as_deref().map(str::trim)
        && !target_text.is_empty()
    {
        return Some(format!("target_text:{target_text}"));
    }
    if let Some(role) = field.role.as_deref().map(str::trim)
        && !role.is_empty()
    {
        return Some(format!("role:{role}"));
    }
    if let Some(label) = field.label.as_deref().map(str::trim)
        && !label.is_empty()
    {
        return Some(format!("label:{label}"));
    }
    if let Some(testid) = field.testid.as_deref().map(str::trim)
        && !testid.is_empty()
    {
        return Some(format!("testid:{testid}"));
    }
    None
}

fn extract_field_locator_context(field: &ExtractFieldSpec) -> serde_json::Value {
    serde_json::json!({
        "index": field.index,
        "ref": field.element_ref,
        "selector": field.selector,
        "target_text": field.target_text,
        "role": field.role,
        "label": field.label,
        "testid": field.testid,
        "first": field.first,
        "last": field.last,
        "nth": field.nth,
        "many": field.many,
    })
}

fn build_content_extract_script(
    selector: &str,
    field: &ExtractFieldSpec,
) -> Result<String, RubError> {
    if matches!(field.kind, ExtractKind::Attribute) && field.attribute.is_none() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "extract field kind 'attribute' requires an 'attribute' name",
        ));
    }

    let selector = serde_json::to_string(selector)
        .map_err(|error| RubError::Internal(format!("selector serialization failed: {error}")))?;
    let kind = serde_json::to_string(field.kind.as_str())
        .map_err(|error| RubError::Internal(format!("kind serialization failed: {error}")))?;
    let attribute = serde_json::to_string(&field.attribute)
        .map_err(|error| RubError::Internal(format!("attribute serialization failed: {error}")))?;
    let first = serde_json::to_string(&field.first)
        .map_err(|error| RubError::Internal(format!("first serialization failed: {error}")))?;
    let last = serde_json::to_string(&field.last)
        .map_err(|error| RubError::Internal(format!("last serialization failed: {error}")))?;
    let nth = serde_json::to_string(&field.nth)
        .map_err(|error| RubError::Internal(format!("nth serialization failed: {error}")))?;

    Ok(format!(
        r#"(function() {{
            const selector = {selector};
            const kind = {kind};
            const attribute = {attribute};
            const first = {first};
            const last = {last};
            const nth = {nth};
            try {{
                const nodes = Array.from(document.querySelectorAll(selector));
                const selectNodes = (values) => {{
                    if (first) return values.slice(0, 1);
                    if (last) return values.slice(-1);
                    if (nth !== null && nth !== undefined) {{
                        const selected = values[nth];
                        return selected ? [selected] : [];
                    }}
                    return values;
                }};
                const readOne = (el) => {{
                    switch (kind) {{
                        case 'text':
                            return String(el.textContent || '').replace(/\s+/g, ' ').trim();
                        case 'value':
                            return 'value' in el ? String(el.value ?? '') : null;
                        case 'html':
                            return el.outerHTML || null;
                        case 'bbox': {{
                            const rect = el.getBoundingClientRect();
                            return {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }};
                        }}
                        case 'attributes':
                            return Object.fromEntries(Array.from(el.attributes || []).map(attr => [attr.name, attr.value]));
                        case 'attribute':
                            return attribute ? el.getAttribute(attribute) : null;
                        default:
                            return null;
                    }}
                }};
                const selectedNodes = selectNodes(nodes);
                return {{
                    match_count: nodes.length,
                    selected_count: selectedNodes.length,
                    values: selectedNodes.map(readOne),
                    selector_error: null,
                }};
            }} catch (error) {{
                return {{
                    match_count: 0,
                    selected_count: 0,
                    values: [],
                    selector_error: String(error && error.message ? error.message : error),
                }};
            }}
        }})()"#
    ))
}

fn field_has_selection(field: &ExtractFieldSpec) -> bool {
    field.first || field.last || field.nth.is_some()
}

pub(super) async fn execute_json_payload_in_frame<T: DeserializeOwned>(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    script: &str,
    payload_kind: &str,
) -> Result<T, RubError> {
    let wrapped_script = format!("JSON.stringify({script})");
    let value = router
        .browser
        .execute_js_in_frame(
            Some(snapshot.frame_context.frame_id.as_str()),
            &wrapped_script,
        )
        .await?;
    let payload_json = value.as_str().ok_or_else(|| {
        RubError::Internal(format!(
            "extract {payload_kind} payload returned non-string projection: {value}"
        ))
    })?;
    serde_json::from_str(payload_json).map_err(|error| {
        RubError::Internal(format!(
            "extract {payload_kind} payload parse failed: {error}; payload={payload_json}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        snapshot_bbox_unavailable_error, snapshot_extract_content_fallback_error,
        snapshot_extract_live_only_kind_error,
    };
    use crate::router::extract::spec::{ExtractFieldSpec, ExtractKind};
    use rub_core::error::ErrorCode;
    use rub_core::model::{BoundingBox, Element, ElementTag};
    use std::collections::HashMap;

    fn snapshot_element() -> Element {
        let mut attributes = HashMap::new();
        attributes.insert("value".to_string(), "42".to_string());
        attributes.insert("data-kind".to_string(), "answer".to_string());
        Element {
            index: 7,
            tag: ElementTag::Input,
            text: "Hello snapshot".to_string(),
            attributes,
            element_ref: Some("main:7".to_string()),
            bounding_box: Some(BoundingBox {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0,
            }),
            ax_info: None,
            listeners: None,
            depth: Some(1),
        }
    }

    fn field(kind: ExtractKind) -> ExtractFieldSpec {
        ExtractFieldSpec {
            index: None,
            element_ref: None,
            selector: None,
            target_text: None,
            role: None,
            label: None,
            testid: None,
            first: false,
            last: false,
            nth: None,
            kind,
            attribute: None,
            many: false,
            value_type: None,
            required: true,
            default: None,
            map: Default::default(),
            transform: None,
        }
    }

    #[test]
    fn snapshot_value_extract_uses_snapshot_projection() {
        let element = snapshot_element();
        let value = super::extract_single_snapshot_value(&element, &field(ExtractKind::Value))
            .expect("snapshot value read should use cached attributes");
        assert_eq!(value, serde_json::json!("42"));
    }

    #[test]
    fn snapshot_attributes_extract_uses_snapshot_projection() {
        let element = snapshot_element();
        let value = super::extract_single_snapshot_value(&element, &field(ExtractKind::Attributes))
            .expect("snapshot attributes read should use cached attributes");
        assert_eq!(value["data-kind"], "answer");
    }

    #[test]
    fn snapshot_bbox_extract_requires_snapshot_bbox_projection() {
        let mut element = snapshot_element();
        element.bounding_box = None;
        let error = super::extract_single_snapshot_value(&element, &field(ExtractKind::Bbox))
            .expect_err("snapshot bbox read must fail closed when bbox projection is absent");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope.context.expect("bbox context")["authority_state"],
            "snapshot_extract_bbox_unavailable"
        );
    }

    #[test]
    fn snapshot_html_extract_fails_closed() {
        let error = snapshot_extract_live_only_kind_error(ExtractKind::Html, 9).into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert_eq!(
            error.context.expect("html context")["authority_state"],
            "snapshot_extract_live_only_field_kind"
        );
    }

    #[test]
    fn snapshot_content_fallback_error_explains_live_authority_crossing() {
        let mut field = field(ExtractKind::Text);
        field.selector = Some(".headline".to_string());
        let envelope =
            snapshot_extract_content_fallback_error("title", ".headline", &field).into_envelope();
        assert_eq!(envelope.code, ErrorCode::ElementNotFound);
        assert_eq!(
            envelope.context.expect("fallback context")["authority_state"],
            "snapshot_extract_live_content_fallback_required"
        );
    }

    #[test]
    fn snapshot_bbox_error_projects_context() {
        let envelope = snapshot_bbox_unavailable_error(3).into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(envelope.context.expect("bbox context")["element_index"], 3);
    }
}
