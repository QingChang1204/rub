use std::collections::BTreeMap;

use rub_core::error::{ErrorCode, RubError};

mod projection;
mod schema;
#[cfg(test)]
mod tests;

use self::projection::project_collection_entry;
use self::schema::{build_collection_extract_script, collection_locator_context};
use super::{ExtractAuthorityMode, ExtractFieldSpec, execute_json_payload_in_frame};
use crate::router::DaemonRouter;

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

pub(super) async fn extract_collection(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    name: &str,
    collection: &ExtractCollectionSpec,
    authority_mode: ExtractAuthorityMode,
) -> Result<serde_json::Value, RubError> {
    if authority_mode == ExtractAuthorityMode::SnapshotOnly {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!(
                "snapshot-addressed extract cannot read collection '{name}' because collection extraction requires live DOM evaluation"
            ),
            serde_json::json!({
                "field": name,
                "authority_state": "snapshot_extract_live_collection_unsupported",
            }),
            "Remove --snapshot-id to allow live collection extraction, or switch to snapshot-addressed field reads only",
        ));
    }
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

async fn execute_collection_extract(
    router: &DaemonRouter,
    snapshot: &rub_core::model::Snapshot,
    collection: &ExtractCollectionSpec,
) -> Result<CollectionExtractPayload, RubError> {
    let script = build_collection_extract_script(collection)?;
    execute_json_payload_in_frame(router, snapshot, &script, "collection").await
}
