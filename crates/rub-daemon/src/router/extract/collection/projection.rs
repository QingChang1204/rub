use std::collections::BTreeMap;

use rub_core::error::{ErrorCode, RubError};

use super::super::{
    ExtractFieldSpec, ExtractMatchSurface, apply_field_postprocess, extract_multi_match_context,
    extract_multi_match_message, extract_multi_match_suggestion,
};
use super::{CollectionEntryPayload, ExtractCollectionSpec, ExtractEntrySpec};
use crate::router::extract_postprocess::resolve_missing_field;

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

pub(super) fn nested_collection_result(rows: Vec<serde_json::Value>) -> serde_json::Value {
    let item_count = rows.len();
    serde_json::json!({
        "items": rows,
        "item_count": item_count,
    })
}

pub(super) fn project_collection_entry(
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
