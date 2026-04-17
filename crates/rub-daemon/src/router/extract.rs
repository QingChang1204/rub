use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

mod collection;
mod field;
mod scan;
mod spec;

use super::projection::snapshot_entity;
use super::secret_resolution::{attach_secret_resolution_projection, redact_json_value};
use super::snapshot::build_stable_snapshot;
use super::*;
use crate::router::addressing::load_snapshot;
use crate::router::extract_postprocess::resolve_missing_field;
use collection::{ExtractEntrySpec, extract_collection};
use field::{
    ExtractMatchSurface, apply_field_postprocess, execute_json_payload_in_frame, extract_field,
    extract_multi_match_context, extract_multi_match_message, extract_multi_match_suggestion,
    should_substitute_missing_field,
};
#[cfg(test)]
use field::{builder_locator_expression, extract_builder_field_examples};
use rub_core::error::{ErrorCode, RubError};
use scan::{scan_collection, wait_for_collection_match};
use spec::{ExtractCommand, ExtractFieldSpec, ExtractKind, parse_extract_fields};

const DEFAULT_SCAN_MAX_SCROLLS: u32 = 100;
const DEFAULT_SCAN_SCROLL_AMOUNT: u32 = 1_800;
const DEFAULT_SCAN_SETTLE_MS: u64 = 1_200;
const DEFAULT_SCAN_STALL_LIMIT: u32 = 3;

pub(super) async fn cmd_extract(
    router: &DaemonRouter,
    args: &serde_json::Value,
    sub_override: Option<&str>,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed_args = ExtractCommand::parse(args, sub_override)?;
    let parsed = parse_extract_fields(parsed_args.spec(), &state.rub_home)?;
    let fields = parsed.value;
    let metadata = parsed.metadata;
    let scan = parsed_args.scan_config()?;
    let wait = parsed_args.wait_config()?;
    let is_inspect_list = matches!(&parsed_args, ExtractCommand::List(_));
    let source_kind = parsed_args.source_kind();

    if scan.is_some() && parsed_args.snapshot_id().is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect list scan cannot reuse --snapshot; scanning requires live snapshots across scroll steps",
        ));
    }
    if wait.is_some() && parsed_args.snapshot_id().is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect list wait cannot reuse --snapshot; waiting for new matches requires live snapshots across poll passes",
        ));
    }
    if scan.is_some() && wait.is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect list wait cannot be combined with scan in the current product surface",
        ));
    }

    if let Some(scan) = scan {
        let (collection_name, collection) =
            resolve_single_collection(&fields, "inspect list scan")?;
        let outcome = scan_collection(
            router,
            args,
            state,
            deadline,
            collection_name,
            collection,
            &scan,
        )
        .await?;
        let mut extracted = serde_json::Map::new();
        extracted.insert(
            collection_name.to_string(),
            serde_json::Value::Array(outcome.rows),
        );
        let mut data = if is_inspect_list {
            extract_payload(
                serde_json::json!({
                    "kind": "collection_extract",
                    "source": "live_page",
                    "collection": collection_name,
                    "scan_requested": true,
                }),
                serde_json::json!({
                    "items": extracted
                        .get(collection_name)
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                    "item_count": outcome.returned_count,
                    "scan": {
                        "complete": outcome.complete,
                        "stop_reason": outcome.stop_reason,
                        "returned_count": outcome.returned_count,
                        "unique_count": outcome.unique_count,
                        "target_count": scan.until_count,
                        "pass_count": outcome.pass_count,
                        "scroll_count": outcome.scroll_count,
                        "scan_key": scan.dedupe_key,
                    },
                }),
            )
        } else {
            extract_payload(
                serde_json::json!({
                    "kind": "extract_query",
                    "source": "live_page",
                }),
                serde_json::json!({
                    "fields": extracted,
                    "field_count": 1,
                    "scan": {
                        "complete": outcome.complete,
                        "stop_reason": outcome.stop_reason,
                        "returned_count": outcome.returned_count,
                        "unique_count": outcome.unique_count,
                        "target_count": scan.until_count,
                        "pass_count": outcome.pass_count,
                        "scroll_count": outcome.scroll_count,
                        "scan_key": scan.dedupe_key,
                    },
                }),
            )
        };
        attach_secret_resolution_projection(&mut data, &metadata);
        redact_json_value(&mut data, &metadata);
        return Ok(data);
    }

    if let Some(wait) = wait {
        let (collection_name, collection) =
            resolve_single_collection(&fields, "inspect list wait")?;
        let outcome = wait_for_collection_match(
            router,
            args,
            state,
            deadline,
            collection_name,
            collection,
            &wait,
        )
        .await?;
        let mut data = extract_payload(
            serde_json::json!({
                "kind": "collection_extract",
                "source": "live_page",
                "collection": collection_name,
                "wait_requested": true,
            }),
            serde_json::json!({
                "items": outcome.rows,
                "item_count": outcome.item_count,
                "matched_item": outcome.matched_item,
                "wait": {
                    "matched": true,
                    "field_path": wait.field_path,
                    "contains": wait.contains,
                    "elapsed_ms": outcome.elapsed_ms,
                },
                "outcome_summary": {
                    "class": "confirmed_new_item_observed",
                    "authoritative": true,
                    "summary": "A new matching projected item was observed in the current list surface.",
                },
            }),
        );
        attach_secret_resolution_projection(&mut data, &metadata);
        redact_json_value(&mut data, &metadata);
        return Ok(data);
    }

    let snapshot = if parsed_args.snapshot_id().is_some() {
        load_snapshot(router, args, state, deadline, false).await?
    } else {
        let snapshot =
            build_stable_snapshot(router, args, state, deadline, Some(0), false, false).await?;
        state.cache_snapshot(snapshot).await
    };

    let mut data = if is_inspect_list {
        let (collection_name, collection) = resolve_single_collection(&fields, "inspect list")?;
        let items = extract_collection(router, &snapshot, collection_name, collection).await?;
        let item_count = items.as_array().map(|value| value.len()).unwrap_or(0);
        extract_payload(
            serde_json::json!({
                "kind": "collection_extract",
                "source": source_kind,
                "collection": collection_name,
                "scan_requested": false,
            }),
            serde_json::json!({
                "snapshot": snapshot_entity(&snapshot),
                "items": items,
                "item_count": item_count,
            }),
        )
    } else {
        let mut extracted = serde_json::Map::new();
        for (name, entry) in fields {
            let value = match entry {
                ExtractEntrySpec::Field(field) => {
                    match extract_field(router, &snapshot, &name, &field).await {
                        Ok(value) => apply_field_postprocess(&name, &field, value)?,
                        Err(error) if should_substitute_missing_field(&field, &error) => {
                            resolve_missing_field(&name, field.required, field.default.as_ref())?
                        }
                        Err(error) => return Err(error),
                    }
                }
                ExtractEntrySpec::Collection(collection) => {
                    extract_collection(router, &snapshot, &name, &collection).await?
                }
            };
            extracted.insert(name, value);
        }
        let field_count = extracted.len();
        extract_payload(
            serde_json::json!({
                "kind": "extract_query",
                "source": source_kind,
            }),
            serde_json::json!({
                "snapshot": snapshot_entity(&snapshot),
                "fields": extracted,
                "field_count": field_count,
            }),
        )
    };
    attach_secret_resolution_projection(&mut data, &metadata);
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

fn extract_payload(subject: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

pub(crate) fn explain_extract_spec_contract(
    raw: &str,
    rub_home: &Path,
) -> Result<serde_json::Value, RubError> {
    let spec = rub_core::json_spec::NormalizedJsonSpec::from_raw_str(raw, "extract")
        .map_err(enrich_extract_explain_error)?;
    let parsed = parse_extract_fields(&spec, rub_home).map_err(enrich_extract_explain_error)?;
    let mut normalized_spec = serde_json::Value::Object(
        parsed
            .value
            .iter()
            .map(|(name, entry)| (name.clone(), render_extract_entry_spec(entry)))
            .collect(),
    );
    redact_json_value(&mut normalized_spec, &parsed.metadata);

    let mut summaries = Vec::new();
    for (name, entry) in &parsed.value {
        collect_entry_summaries(name, entry, &mut summaries);
    }
    let mut summary_value = serde_json::Value::Array(summaries);
    redact_json_value(&mut summary_value, &parsed.metadata);

    Ok(serde_json::json!({
        "subject": {
            "kind": "extract_explain",
            "surface": "local_contract",
        },
        "result": {
            "normalized_spec": normalized_spec,
            "entry_summaries": summary_value,
            "guidance": {
                "schema_command": "rub extract --schema",
                "examples_command": "rub extract --examples",
                "example_topics": ["all", "basic", "attribute", "collection", "validation"],
            }
        }
    }))
}

fn enrich_extract_explain_error(error: RubError) -> RubError {
    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = context.as_object_mut() {
        object.insert(
            "schema_command".to_string(),
            serde_json::json!("rub extract --schema"),
        );
        object.insert(
            "examples_command".to_string(),
            serde_json::json!("rub extract --examples"),
        );
        object.insert(
            "example_topics".to_string(),
            serde_json::json!(["all", "basic", "attribute", "collection", "validation"]),
        );
    }
    envelope.context = Some(context);
    if envelope.suggestion.is_empty() {
        envelope.suggestion =
            "Try `rub extract --schema` for the canonical field contract or `rub extract --examples` for working shapes.".to_string();
    }
    RubError::Domain(envelope)
}

fn render_extract_entry_spec(entry: &ExtractEntrySpec) -> serde_json::Value {
    match entry {
        ExtractEntrySpec::Field(field) => render_extract_field_spec(field),
        ExtractEntrySpec::Collection(collection) => render_extract_collection_spec(collection),
    }
}

fn render_extract_field_spec(field: &ExtractFieldSpec) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    insert_locator_keys(&mut object, field);
    object.insert("kind".to_string(), serde_json::json!(field.kind.as_str()));
    if let Some(attribute) = &field.attribute {
        object.insert("attribute".to_string(), serde_json::json!(attribute));
    }
    if field.many {
        object.insert("many".to_string(), serde_json::json!(true));
    }
    if !field.required {
        object.insert("required".to_string(), serde_json::json!(false));
    }
    if let Some(default) = &field.default {
        object.insert("default".to_string(), default.clone());
    }
    if !field.map.is_empty() {
        object.insert("map".to_string(), serde_json::json!(field.map));
    }
    if let Some(transform) = field.transform {
        object.insert(
            "transform".to_string(),
            serde_json::json!(transform.as_str()),
        );
    }
    if let Some(value_type) = field.value_type {
        object.insert("type".to_string(), serde_json::json!(value_type.as_str()));
    }
    serde_json::Value::Object(object)
}

fn render_extract_collection_spec(
    collection: &collection::ExtractCollectionSpec,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    if let Some(selector) = &collection.collection {
        object.insert("collection".to_string(), serde_json::json!(selector));
    }
    if let Some(selector) = &collection.selector {
        object.insert("selector".to_string(), serde_json::json!(selector));
    }
    if let Some(target_text) = &collection.target_text {
        object.insert("target_text".to_string(), serde_json::json!(target_text));
    }
    if let Some(role) = &collection.role {
        object.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(label) = &collection.label {
        object.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(testid) = &collection.testid {
        object.insert("testid".to_string(), serde_json::json!(testid));
    }
    if let Some(row_scope_selector) = &collection.row_scope_selector {
        object.insert(
            "row_scope_selector".to_string(),
            serde_json::json!(row_scope_selector),
        );
    }
    if collection.first {
        object.insert("first".to_string(), serde_json::json!(true));
    }
    if collection.last {
        object.insert("last".to_string(), serde_json::json!(true));
    }
    if let Some(nth) = collection.nth {
        object.insert("nth".to_string(), serde_json::json!(nth));
    }
    let fields = collection
        .fields
        .iter()
        .map(|(name, entry)| (name.clone(), render_extract_entry_spec(entry)))
        .collect();
    object.insert("fields".to_string(), serde_json::Value::Object(fields));
    serde_json::Value::Object(object)
}

fn insert_locator_keys(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &ExtractFieldSpec,
) {
    if let Some(index) = field.index {
        object.insert("index".to_string(), serde_json::json!(index));
    }
    if let Some(element_ref) = &field.element_ref {
        object.insert("ref".to_string(), serde_json::json!(element_ref));
    }
    if let Some(selector) = &field.selector {
        object.insert("selector".to_string(), serde_json::json!(selector));
    }
    if let Some(target_text) = &field.target_text {
        object.insert("target_text".to_string(), serde_json::json!(target_text));
    }
    if let Some(role) = &field.role {
        object.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(label) = &field.label {
        object.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(testid) = &field.testid {
        object.insert("testid".to_string(), serde_json::json!(testid));
    }
    if field.first {
        object.insert("first".to_string(), serde_json::json!(true));
    }
    if field.last {
        object.insert("last".to_string(), serde_json::json!(true));
    }
    if let Some(nth) = field.nth {
        object.insert("nth".to_string(), serde_json::json!(nth));
    }
}

fn collect_entry_summaries(
    path: &str,
    entry: &ExtractEntrySpec,
    summaries: &mut Vec<serde_json::Value>,
) {
    match entry {
        ExtractEntrySpec::Field(field) => summaries.push(serde_json::json!({
            "path": path,
            "entry_kind": "field",
            "kind": field.kind.as_str(),
            "locator_keys_present": extract_locator_keys_present(field),
            "many": field.many,
            "required": field.required,
            "type": field.value_type.map(|value_type| value_type.as_str()),
            "transform": field.transform.map(|transform| transform.as_str()),
        })),
        ExtractEntrySpec::Collection(collection) => {
            summaries.push(serde_json::json!({
                "path": path,
                "entry_kind": "collection",
                "locator_keys_present": extract_collection_locator_keys_present(collection),
                "row_scope_selector": collection.row_scope_selector,
                "field_count": collection.fields.len(),
            }));
            for (field_name, nested) in &collection.fields {
                let nested_path = format!("{path}.{field_name}");
                collect_entry_summaries(&nested_path, nested, summaries);
            }
        }
    }
}

fn extract_locator_keys_present(field: &ExtractFieldSpec) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if field.index.is_some() {
        keys.push("index");
    }
    if field.element_ref.is_some() {
        keys.push("ref");
    }
    if field.selector.is_some() {
        keys.push("selector");
    }
    if field.target_text.is_some() {
        keys.push("target_text");
    }
    if field.role.is_some() {
        keys.push("role");
    }
    if field.label.is_some() {
        keys.push("label");
    }
    if field.testid.is_some() {
        keys.push("testid");
    }
    if field.first {
        keys.push("first");
    }
    if field.last {
        keys.push("last");
    }
    if field.nth.is_some() {
        keys.push("nth");
    }
    keys
}

fn extract_collection_locator_keys_present(
    collection: &collection::ExtractCollectionSpec,
) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if collection.collection.is_some() {
        keys.push("collection");
    }
    if collection.selector.is_some() {
        keys.push("selector");
    }
    if collection.target_text.is_some() {
        keys.push("target_text");
    }
    if collection.role.is_some() {
        keys.push("role");
    }
    if collection.label.is_some() {
        keys.push("label");
    }
    if collection.testid.is_some() {
        keys.push("testid");
    }
    if collection.row_scope_selector.is_some() {
        keys.push("row_scope_selector");
    }
    if collection.first {
        keys.push("first");
    }
    if collection.last {
        keys.push("last");
    }
    if collection.nth.is_some() {
        keys.push("nth");
    }
    keys
}

fn resolve_single_collection<'a>(
    fields: &'a BTreeMap<String, ExtractEntrySpec>,
    command_label: &str,
) -> Result<(&'a str, &'a collection::ExtractCollectionSpec), RubError> {
    let mut collections = fields.iter().filter_map(|(name, entry)| match entry {
        ExtractEntrySpec::Collection(collection) => Some((name.as_str(), collection)),
        ExtractEntrySpec::Field(_) => None,
    });
    let first = collections.next();
    let second = collections.next();
    match (first, second) {
        (Some((name, collection)), None) if fields.len() == 1 => Ok((name, collection)),
        (Some(_), Some(_)) | (Some(_), None) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command_label} currently requires exactly one top-level collection field"),
        )),
        (None, _) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command_label} requires a top-level collection spec"),
        )),
    }
}

#[cfg(test)]
mod tests;
