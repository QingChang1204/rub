use std::collections::BTreeMap;

use rub_cdp::live_dom_locator::LOCATOR_JS_HELPERS;
use rub_core::error::{ErrorCode, RubError};

use super::super::{ExtractFieldSpec, ExtractKind};
use super::{ExtractCollectionSpec, ExtractEntrySpec};

pub(super) fn build_collection_extract_script(
    collection: &ExtractCollectionSpec,
) -> Result<String, RubError> {
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

pub(super) fn collection_locator_context(collection: &ExtractCollectionSpec) -> serde_json::Value {
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
