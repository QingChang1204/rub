use std::collections::BTreeMap;

use rub_cdp::live_dom_locator::LOCATOR_JS_HELPERS;
use rub_core::error::{ErrorCode, RubError};

use super::{
    ExtractFieldSpec, ExtractKind, ExtractMatchSurface, apply_field_postprocess,
    execute_json_payload_in_frame, extract_multi_match_context, extract_multi_match_message,
    extract_multi_match_suggestion,
};
use crate::router::DaemonRouter;
use crate::router::extract_postprocess::resolve_missing_field;

#[derive(Debug, serde::Deserialize)]
pub(super) struct CollectionExtractPayload {
    row_count: usize,
    rows: Vec<BTreeMap<String, CollectionEntryPayload>>,
    selector_error: Option<String>,
    row_scope_error: Option<String>,
    field_errors: BTreeMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "payload_kind", rename_all = "snake_case")]
pub(super) enum CollectionEntryPayload {
    Field {
        match_count: usize,
        values: Vec<serde_json::Value>,
    },
    Collection {
        row_count: usize,
        rows: Vec<BTreeMap<String, CollectionEntryPayload>>,
        selector_error: Option<String>,
        row_scope_error: Option<String>,
        field_errors: BTreeMap<String, String>,
    },
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtractCollectionSpec {
    pub(super) collection: Option<String>,
    pub(super) selector: Option<String>,
    pub(super) target_text: Option<String>,
    pub(super) role: Option<String>,
    pub(super) label: Option<String>,
    pub(super) testid: Option<String>,
    #[serde(default)]
    pub(super) row_scope_selector: Option<String>,
    #[serde(default)]
    pub(super) first: bool,
    #[serde(default)]
    pub(super) last: bool,
    pub(super) nth: Option<u32>,
    pub(super) fields: BTreeMap<String, ExtractEntrySpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub(super) enum ExtractEntrySpec {
    Collection(ExtractCollectionSpec),
    Field(ExtractFieldSpec),
}

struct CollectionProjectionContext<'a> {
    collection_name: &'a str,
    row_index: usize,
    field_name: &'a str,
}

struct NestedCollectionPayloadView<'a> {
    row_count: usize,
    rows: &'a [BTreeMap<String, CollectionEntryPayload>],
    selector_error: Option<&'a str>,
    row_scope_error: Option<&'a str>,
    field_errors: &'a BTreeMap<String, String>,
}

pub(super) async fn extract_collection(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    name: &str,
    collection: &ExtractCollectionSpec,
) -> Result<serde_json::Value, RubError> {
    let payload = execute_collection_extract(router, snapshot, collection).await?;
    if let Some(selector_error) = payload.selector_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid collection selector for '{name}': {selector_error}"),
            serde_json::json!({
                "locator": collection_locator_context(collection),
                "field": name,
            }),
        ));
    }
    if let Some(row_scope_error) = payload.row_scope_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid row scope selector for collection '{name}': {row_scope_error}"),
            serde_json::json!({
                "locator": collection_locator_context(collection),
                "row_scope_selector": collection.row_scope_selector,
                "field": name,
            }),
        ));
    }
    if !payload.field_errors.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid child selector in collection '{name}'"),
            serde_json::json!({
                "locator": collection_locator_context(collection),
                "field_errors": payload.field_errors,
            }),
        ));
    }

    let mut rows = Vec::with_capacity(payload.rows.len());
    for (row_index, row) in payload.rows.into_iter().enumerate() {
        let mut projected = serde_json::Map::new();
        for (field_name, entry_spec) in &collection.fields {
            let Some(entry_payload) = row.get(field_name) else {
                return Err(RubError::Internal(format!(
                    "collection payload missing child entry '{field_name}'"
                )));
            };
            let value =
                project_collection_entry(name, row_index, field_name, entry_spec, entry_payload)?;
            projected.insert(field_name.clone(), value);
        }
        rows.push(serde_json::Value::Object(projected));
    }

    if payload.row_count == 0 {
        return Ok(serde_json::Value::Array(Vec::new()));
    }

    Ok(serde_json::Value::Array(rows))
}

fn nested_collection_result(rows: Vec<serde_json::Value>) -> serde_json::Value {
    let item_count = rows.len();
    serde_json::json!({
        "items": rows,
        "item_count": item_count,
    })
}

fn project_collection_entry(
    collection_name: &str,
    row_index: usize,
    field_name: &str,
    entry_spec: &ExtractEntrySpec,
    payload: &CollectionEntryPayload,
) -> Result<serde_json::Value, RubError> {
    match (entry_spec, payload) {
        (
            ExtractEntrySpec::Field(field_spec),
            CollectionEntryPayload::Field {
                match_count,
                values,
            },
        ) => project_collection_field(
            collection_name,
            row_index,
            field_name,
            field_spec,
            *match_count,
            values,
        ),
        (
            ExtractEntrySpec::Collection(collection_spec),
            CollectionEntryPayload::Collection {
                row_count,
                rows,
                selector_error,
                row_scope_error,
                field_errors,
            },
        ) => {
            let nested = NestedCollectionPayloadView {
                row_count: *row_count,
                rows,
                selector_error: selector_error.as_deref(),
                row_scope_error: row_scope_error.as_deref(),
                field_errors,
            };
            project_nested_collection(
                CollectionProjectionContext {
                    collection_name,
                    row_index,
                    field_name,
                },
                collection_spec,
                &nested,
            )
        }
        (ExtractEntrySpec::Field(_), CollectionEntryPayload::Collection { .. }) => {
            Err(RubError::Internal(format!(
                "collection payload for '{field_name}' returned nested rows for a scalar field"
            )))
        }
        (ExtractEntrySpec::Collection(_), CollectionEntryPayload::Field { .. }) => {
            Err(RubError::Internal(format!(
                "collection payload for '{field_name}' returned scalar values for a nested collection"
            )))
        }
    }
}

fn project_collection_field(
    collection_name: &str,
    row_index: usize,
    field_name: &str,
    field_spec: &ExtractFieldSpec,
    match_count: usize,
    values: &[serde_json::Value],
) -> Result<serde_json::Value, RubError> {
    if match_count == 0 {
        if !field_spec.required || field_spec.default.is_some() {
            return resolve_missing_field(
                field_name,
                field_spec.required,
                field_spec.default.as_ref(),
            );
        }
        return Err(RubError::domain_with_context(
            ErrorCode::ElementNotFound,
            format!(
                "collection field '{field_name}' did not resolve within row {row_index} of '{collection_name}'"
            ),
            serde_json::json!({
                "collection": collection_name,
                "row_index": row_index,
                "selector": field_spec.selector,
            }),
        ));
    }

    if !field_spec.many && match_count > 1 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            extract_multi_match_message(
                field_name,
                match_count,
                ExtractMatchSurface::CollectionRow {
                    collection_name,
                    row_index,
                },
            ),
            extract_multi_match_context(
                field_name,
                field_spec,
                match_count,
                ExtractMatchSurface::CollectionRow {
                    collection_name,
                    row_index,
                },
            ),
            extract_multi_match_suggestion(ExtractMatchSurface::CollectionRow {
                collection_name,
                row_index,
            }),
        ));
    }

    let raw_value = if field_spec.many {
        serde_json::Value::Array(values.to_vec())
    } else {
        values.first().cloned().ok_or_else(|| {
            RubError::Internal("collection payload missing first value".to_string())
        })?
    };
    apply_field_postprocess(field_name, field_spec, raw_value)
}

fn project_nested_collection(
    context: CollectionProjectionContext<'_>,
    collection_spec: &ExtractCollectionSpec,
    payload: &NestedCollectionPayloadView<'_>,
) -> Result<serde_json::Value, RubError> {
    if let Some(selector_error) = payload.selector_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Invalid nested collection selector for '{field_name}' in row {row_index} of '{collection_name}': {selector_error}",
                field_name = context.field_name,
                row_index = context.row_index,
                collection_name = context.collection_name,
            ),
            serde_json::json!({
                    "collection": context.collection_name,
                    "row_index": context.row_index,
                    "field": context.field_name,
            }),
        ));
    }

    if let Some(row_scope_error) = payload.row_scope_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Invalid nested row scope selector for '{field_name}' in row {row_index} of '{collection_name}': {row_scope_error}",
                field_name = context.field_name,
                row_index = context.row_index,
                collection_name = context.collection_name,
            ),
            serde_json::json!({
                "collection": context.collection_name,
                "row_index": context.row_index,
                "field": context.field_name,
                "row_scope_selector": collection_spec.row_scope_selector,
            }),
        ));
    }

    if !payload.field_errors.is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Invalid nested child selector in '{field_name}' for row {row_index} of '{collection_name}'",
                field_name = context.field_name,
                row_index = context.row_index,
                collection_name = context.collection_name,
            ),
            serde_json::json!({
                "collection": context.collection_name,
                "row_index": context.row_index,
                "field": context.field_name,
                "field_errors": payload.field_errors,
            }),
        ));
    }

    if payload.row_count == 0 {
        return Ok(nested_collection_result(Vec::new()));
    }

    let mut nested_rows = Vec::with_capacity(payload.rows.len());
    for (nested_row_index, nested_row) in payload.rows.iter().enumerate() {
        let mut projected = serde_json::Map::new();
        for (nested_field_name, nested_entry_spec) in &collection_spec.fields {
            let Some(nested_entry_payload) = nested_row.get(nested_field_name) else {
                return Err(RubError::Internal(format!(
                    "nested collection payload missing child entry '{nested_field_name}'"
                )));
            };
            let value = project_collection_entry(
                context.field_name,
                nested_row_index,
                nested_field_name,
                nested_entry_spec,
                nested_entry_payload,
            )?;
            projected.insert(nested_field_name.clone(), value);
        }
        nested_rows.push(serde_json::Value::Object(projected));
    }
    Ok(nested_collection_result(nested_rows))
}

async fn execute_collection_extract(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    collection: &ExtractCollectionSpec,
) -> Result<CollectionExtractPayload, RubError> {
    let script = build_collection_extract_script(collection)?;
    execute_json_payload_in_frame(router, snapshot, &script, "collection").await
}

fn build_collection_extract_script(collection: &ExtractCollectionSpec) -> Result<String, RubError> {
    let collection_root = build_collection_root_schema(collection)?;
    let collection_root = serde_json::to_string(&collection_root).map_err(|error| {
        RubError::Internal(format!("collection root serialization failed: {error}"))
    })?;
    let fields = build_collection_entry_schemas(&collection.fields)?;
    let fields = serde_json::to_string(&fields).map_err(|error| {
        RubError::Internal(format!("collection field serialization failed: {error}"))
    })?;

    Ok(format!(
        r#"(function() {{
            const collectionRoot = {collection_root};
            const fields = {fields};
            try {{
                {LOCATOR_JS_HELPERS}
                const normalizeText = normalize;
                const testId = testingId;

                const candidateValues = (el) => {{
                    const values = [];
                    const text = normalizeText(el.textContent || '');
                    if (text) values.push(text);
                    for (const name of ['aria-label', 'placeholder', 'title', 'alt', 'value', 'name']) {{
                        const value = normalizeText(el.getAttribute(name) || '');
                        if (value) values.push(value);
                    }}
                    return values;
                }};

                const elementsInScope = (root) => {{
                    if (!root) {{
                        return [];
                    }}
                    const scopeRoot =
                        root.nodeType === Node.DOCUMENT_NODE
                            ? (root.documentElement || root.body)
                            : root;
                    if (!scopeRoot || typeof scopeRoot.querySelectorAll !== 'function') {{
                        return [];
                    }}
                    return [scopeRoot, ...Array.from(scopeRoot.querySelectorAll('*'))];
                }};

                const applySelection = (nodes, entry) => {{
                    if (!nodes.length) return nodes;
                    if (entry.first) return nodes.slice(0, 1);
                    if (entry.last) return nodes.slice(-1);
                    if (entry.nth !== null && entry.nth !== undefined) {{
                        const selected = nodes[entry.nth];
                        return selected ? [selected] : [];
                    }}
                    return nodes;
                }};

                const resolveNodes = (root, entry, options = {{ allowDefaultRoot: false, defaultRootOverride: undefined }}) => {{
                    const configured = [
                        entry.selector,
                        entry.target_text,
                        entry.role,
                        entry.label,
                        entry.testid,
                    ].filter((value) => value !== null && value !== undefined && value !== '').length;

                    if (configured === 0) {{
                        if (!options.allowDefaultRoot) return [];
                        const defaultRoot =
                            options.defaultRootOverride !== undefined
                                ? options.defaultRootOverride
                                : root;
                        if (defaultRoot === null || defaultRoot === undefined) return [];
                        if (defaultRoot && defaultRoot.nodeType === Node.DOCUMENT_NODE) {{
                            const scopeRoot = defaultRoot.documentElement || defaultRoot.body;
                            return scopeRoot ? [scopeRoot] : [];
                        }}
                        return [defaultRoot];
                    }}

                    let nodes;
                    if (entry.selector) {{
                        try {{
                            nodes = Array.from(root.querySelectorAll(entry.selector));
                        }} catch (error) {{
                            throw new Error(String(error && error.message ? error.message : error));
                        }}
                        return applySelection(nodes, entry);
                    }}

                    const candidates = elementsInScope(root);
                    if (entry.target_text) {{
                        const query = normalizeText(entry.target_text);
                        const exact = candidates.filter((candidate) =>
                            candidateValues(candidate).some((value) => value === query)
                        );
                        nodes = exact.length
                            ? exact
                            : candidates.filter((candidate) =>
                                  candidateValues(candidate).some((value) => value.includes(query))
                              );
                        return applySelection(nodes, entry);
                    }}

                    if (entry.role) {{
                        const query = normalizeText(entry.role);
                        nodes = candidates.filter((candidate) => semanticRole(candidate) === query);
                        return applySelection(nodes, entry);
                    }}

                    if (entry.label) {{
                        const query = normalizeText(entry.label);
                        const exact = candidates.filter((candidate) => accessibleLabel(candidate) === query);
                        nodes = exact.length
                            ? exact
                            : candidates.filter((candidate) => {{
                                  const value = accessibleLabel(candidate);
                                  return value && value.includes(query);
                              }});
                        return applySelection(nodes, entry);
                    }}

                    if (entry.testid) {{
                        const query = normalizeText(entry.testid);
                        nodes = candidates.filter((candidate) => testId(candidate) === query);
                        return applySelection(nodes, entry);
                    }}

                    return [];
                }};

                const validateRowScopeSelector = (selector) => {{
                    if (!selector) return null;
                    try {{
                        const probe = document.documentElement || document.body;
                        if (!probe || typeof probe.matches !== 'function') return null;
                        probe.matches(selector);
                        return null;
                    }} catch (error) {{
                        return String(error && error.message ? error.message : error);
                    }}
                }};

                const resolveProjectionRoot = (rowRoot, collection) => {{
                    if (!collection.row_scope_selector) return rowRoot;
                    if (!rowRoot || typeof rowRoot.closest !== 'function') return null;
                    return rowRoot.closest(collection.row_scope_selector);
                }};

                const readOne = (el, kind, attribute) => {{
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

                const projectEntry = (root, entry, defaultRoot = root) => {{
                    if (entry.entry_kind === 'field') {{
                        let nodes;
                        try {{
                            nodes = resolveNodes(root, entry, {{
                                allowDefaultRoot: true,
                                defaultRootOverride: defaultRoot,
                            }});
                        }} catch (error) {{
                            throw {{
                                __rub_field_error: true,
                                field: entry.name,
                                message: String(error && error.message ? error.message : error),
                            }};
                        }}
                        return {{
                            payload_kind: 'field',
                            match_count: nodes.length,
                            values: nodes.map(node => readOne(node, entry.kind, entry.attribute)),
                        }};
                    }}
                    return projectCollection(root, entry);
                }};

                const projectCollection = (root, collection) => {{
                    let roots;
                    try {{
                        roots = resolveNodes(root, collection, {{ allowDefaultRoot: false }});
                    }} catch (error) {{
                        return {{
                            payload_kind: 'collection',
                            row_count: 0,
                            rows: [],
                            selector_error: String(error && error.message ? error.message : error),
                            row_scope_error: null,
                            field_errors: {{}},
                        }};
                    }}

                    const rowScopeError = validateRowScopeSelector(collection.row_scope_selector);
                    if (rowScopeError) {{
                        return {{
                            payload_kind: 'collection',
                            row_count: 0,
                            rows: [],
                            selector_error: null,
                            row_scope_error: rowScopeError,
                            field_errors: {{}},
                        }};
                    }}

                    const fieldErrors = {{}};
                    const rows = roots.map(rowRoot => {{
                        const projectionRoot = resolveProjectionRoot(rowRoot, collection);
                        const row = {{}};
                        for (const field of collection.fields) {{
                            try {{
                                row[field.name] = projectEntry(projectionRoot, field, rowRoot);
                            }} catch (error) {{
                                if (error && error.__rub_field_error) {{
                                    fieldErrors[error.field] = error.message;
                                    row[field.name] = {{
                                        payload_kind: 'field',
                                        match_count: 0,
                                        values: [],
                                    }};
                                    continue;
                                }}
                                throw error;
                            }}
                        }}
                        return row;
                    }});

                    return {{
                        payload_kind: 'collection',
                        row_count: roots.length,
                        rows,
                        selector_error: null,
                        row_scope_error: null,
                        field_errors: fieldErrors,
                    }};
                }};

                return projectCollection(document, {{
                    ...collectionRoot,
                    fields,
                }});
            }} catch (error) {{
                return {{
                    payload_kind: 'collection',
                    row_count: 0,
                    rows: [],
                    selector_error: String(error && error.message ? error.message : error),
                    row_scope_error: null,
                    field_errors: {{}},
                }};
            }}
        }})()"#
    ))
}

fn build_collection_entry_schemas(
    fields: &BTreeMap<String, ExtractEntrySpec>,
) -> Result<Vec<serde_json::Value>, RubError> {
    fields
        .iter()
        .map(|(name, entry)| build_collection_entry_schema(name, entry))
        .collect()
}

fn build_collection_entry_schema(
    name: &str,
    entry: &ExtractEntrySpec,
) -> Result<serde_json::Value, RubError> {
    match entry {
        ExtractEntrySpec::Field(field) => {
            validate_collection_field_locator(field)?;
            if matches!(field.kind, ExtractKind::Attribute) && field.attribute.is_none() {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    "extract field kind 'attribute' requires an 'attribute' name",
                ));
            }
            Ok(serde_json::json!({
                "entry_kind": "field",
                "name": name,
                "selector": field.selector,
                "target_text": field.target_text,
                "role": field.role,
                "label": field.label,
                "testid": field.testid,
                "first": field.first,
                "last": field.last,
                "nth": field.nth,
                "kind": field.kind.as_str(),
                "attribute": field.attribute,
                "many": field.many,
            }))
        }
        ExtractEntrySpec::Collection(collection) => Ok(serde_json::json!({
            "entry_kind": "collection",
            "name": name,
            "selector": collection.collection.as_ref().or(collection.selector.as_ref()),
            "target_text": collection.target_text,
            "role": collection.role,
            "label": collection.label,
            "testid": collection.testid,
            "row_scope_selector": collection.row_scope_selector,
            "first": collection.first,
            "last": collection.last,
            "nth": collection.nth,
            "fields": build_collection_entry_schemas(&collection.fields)?,
        })),
    }
}

fn validate_collection_field_locator(field: &ExtractFieldSpec) -> Result<(), RubError> {
    if field.index.is_some() || field.element_ref.is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "nested collection fields do not support index/ref locators; use selector, target_text, role, label, or testid within the row scope",
        ));
    }
    validate_row_scoped_locator(RowScopedLocatorValidation {
        selector: field.selector.as_deref(),
        target_text: field.target_text.as_deref(),
        role: field.role.as_deref(),
        label: field.label.as_deref(),
        testid: field.testid.as_deref(),
        first: field.first,
        last: field.last,
        nth: field.nth,
        allow_default_root: true,
        scope_name: "nested collection field",
    })
}

fn build_collection_root_schema(
    collection: &ExtractCollectionSpec,
) -> Result<serde_json::Value, RubError> {
    if collection.collection.is_some() && collection.selector.is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "collection locator is ambiguous: provide either 'collection' or 'selector', not both",
        ));
    }
    let selector = collection
        .collection
        .as_deref()
        .or(collection.selector.as_deref());
    validate_row_scoped_locator(RowScopedLocatorValidation {
        selector,
        target_text: collection.target_text.as_deref(),
        role: collection.role.as_deref(),
        label: collection.label.as_deref(),
        testid: collection.testid.as_deref(),
        first: collection.first,
        last: collection.last,
        nth: collection.nth,
        allow_default_root: false,
        scope_name: "collection",
    })?;
    Ok(serde_json::json!({
        "selector": selector,
        "target_text": collection.target_text,
        "role": collection.role,
        "label": collection.label,
        "testid": collection.testid,
        "row_scope_selector": collection.row_scope_selector,
        "first": collection.first,
        "last": collection.last,
        "nth": collection.nth,
    }))
}

struct RowScopedLocatorValidation<'a> {
    selector: Option<&'a str>,
    target_text: Option<&'a str>,
    role: Option<&'a str>,
    label: Option<&'a str>,
    testid: Option<&'a str>,
    first: bool,
    last: bool,
    nth: Option<u32>,
    allow_default_root: bool,
    scope_name: &'a str,
}

fn validate_row_scoped_locator(validation: RowScopedLocatorValidation<'_>) -> Result<(), RubError> {
    let configured = validation.selector.is_some() as u8
        + validation.target_text.is_some() as u8
        + validation.role.is_some() as u8
        + validation.label.is_some() as u8
        + validation.testid.is_some() as u8;
    if configured == 0 && !validation.allow_default_root {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{} requires one locator: selector, target_text, role, label, or testid",
                validation.scope_name
            ),
        ));
    }
    if configured > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{} locator is ambiguous: provide exactly one of selector, target_text, role, label, or testid",
                validation.scope_name
            ),
        ));
    }

    let selection_count =
        validation.first as u8 + validation.last as u8 + validation.nth.is_some() as u8;
    if selection_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{} match selection is ambiguous: provide at most one of first, last, or nth",
                validation.scope_name
            ),
        ));
    }
    if configured == 0 && selection_count > 0 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{} match selection requires an explicit locator inside the row scope",
                validation.scope_name
            ),
        ));
    }
    Ok(())
}

fn collection_locator_context(collection: &ExtractCollectionSpec) -> serde_json::Value {
    serde_json::json!({
        "collection": collection.collection,
        "selector": collection.selector,
        "target_text": collection.target_text,
        "role": collection.role,
        "label": collection.label,
        "testid": collection.testid,
        "row_scope_selector": collection.row_scope_selector,
        "first": collection.first,
        "last": collection.last,
        "nth": collection.nth,
    })
}

#[cfg(test)]
mod tests {
    use super::{ExtractCollectionSpec, nested_collection_result};

    #[test]
    fn nested_collection_result_uses_canonical_batch_shape() {
        let result = nested_collection_result(vec![
            serde_json::json!({ "text": "automation" }),
            serde_json::json!({ "text": "rust" }),
        ]);
        assert_eq!(result["item_count"], 2);
        assert_eq!(result["items"][0]["text"], "automation");
        assert_eq!(result["items"][1]["text"], "rust");
    }

    #[test]
    fn collection_spec_rejects_unknown_fields() {
        let error = serde_json::from_value::<ExtractCollectionSpec>(serde_json::json!({
            "selector": ".item",
            "fields": [],
            "fieds": []
        }))
        .expect_err("unknown collection fields should fail closed");
        assert!(error.to_string().contains("unknown field"));
    }
}
