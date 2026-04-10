use std::collections::BTreeMap;
use std::sync::Arc;

mod collection;
mod field;
mod scan;
mod spec;

use super::projection::snapshot_entity;
use super::secret_resolution::redact_json_value;
use super::snapshot::build_stable_snapshot;
use super::*;
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
use scan::scan_collection;
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
    let is_inspect_list = matches!(&parsed_args, ExtractCommand::List(_));
    let source_kind = parsed_args.source_kind();

    if scan.is_some() && parsed_args.snapshot_id().is_some() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect list scan cannot reuse --snapshot; scanning requires live snapshots across scroll steps",
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
        redact_json_value(&mut data, &metadata);
        return Ok(data);
    }

    let snapshot = if let Some(snapshot_id) = parsed_args.snapshot_id() {
        state.get_snapshot(snapshot_id).await.ok_or_else(|| {
            RubError::domain(
                ErrorCode::StaleSnapshot,
                format!("Snapshot '{snapshot_id}' not found in cache"),
            )
        })?
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
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

fn extract_payload(subject: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
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
mod tests {
    use super::{
        ExtractCommand, ExtractFieldSpec, ExtractKind, builder_locator_expression,
        extract_builder_field_examples,
    };
    use crate::router::extract_postprocess::ExtractValueType;
    use serde_json::json;

    #[test]
    fn extract_field_supports_type_shorthand_for_kind() {
        let field: ExtractFieldSpec = serde_json::from_value(serde_json::json!({
            "selector": "#headline",
            "type": "text"
        }))
        .expect("extract field shorthand should deserialize");

        assert_eq!(field.selector.as_deref(), Some("#headline"));
        assert!(matches!(field.kind, ExtractKind::Text));
        assert!(field.value_type.is_none());
    }

    #[test]
    fn extract_field_preserves_value_type_when_kind_is_explicit() {
        let field: ExtractFieldSpec = serde_json::from_value(serde_json::json!({
            "selector": "#count",
            "kind": "text",
            "transform": "parse_int",
            "type": "number"
        }))
        .expect("extract field with output type should deserialize");

        assert!(matches!(field.kind, ExtractKind::Text));
        assert!(matches!(field.value_type, Some(ExtractValueType::Number)));
    }

    #[test]
    fn builder_examples_support_semantic_locators() {
        let field: ExtractFieldSpec = serde_json::from_value(serde_json::json!({
            "kind": "attribute",
            "attribute": "src",
            "role": "img"
        }))
        .expect("semantic extract field should deserialize");

        assert_eq!(
            builder_locator_expression(&field).as_deref(),
            Some("role:img")
        );
        assert_eq!(
            extract_builder_field_examples("hero", &field),
            Some(serde_json::json!({
                "pick_first": "hero=attribute:src:role:img@first",
                "pick_last": "hero=attribute:src:role:img@last",
                "pick_nth": "hero=attribute:src:role:img@nth(0)",
                "collect_all": "hero=attribute:src:role:img@many",
            }))
        );
    }

    #[test]
    fn extract_field_rejects_unknown_fields() {
        let error = serde_json::from_value::<ExtractFieldSpec>(serde_json::json!({
            "selector": "#headline",
            "kind": "text",
            "knd": "text"
        }))
        .expect_err("unknown extract fields should fail closed");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn extract_command_defaults_to_query_mode() {
        let parsed = ExtractCommand::parse(
            &json!({
                "spec": "{\"title\":{\"kind\":\"text\",\"selector\":\"h1\"}}",
                "snapshot_id": "snap-1",
            }),
            None,
        )
        .expect("plain extract payload should parse");

        match parsed {
            ExtractCommand::Query(args) => {
                assert_eq!(args.snapshot_id.as_deref(), Some("snap-1"));
            }
            ExtractCommand::List(_) => panic!("expected query mode"),
        }
    }

    #[test]
    fn extract_command_parses_list_scan_payload() {
        let parsed = ExtractCommand::parse(
            &json!({
                "sub": "list",
                "spec": "{\"rows\":{\"collection\":{\"selector\":\"li\",\"fields\":{\"title\":{\"kind\":\"text\",\"selector\":\".title\"}}}}}",
                "scan_until": 25,
                "scan_key": "id",
                "max_scrolls": 4,
                "scroll_amount": 900,
                "settle_ms": 300,
                "stall_limit": 2,
            }),
            None,
        )
        .expect("inspect list payload should parse");

        let scan = parsed
            .scan_config()
            .expect("scan config should validate")
            .expect("scan config should be present");
        assert_eq!(scan.until_count, 25);
        assert_eq!(scan.dedupe_key.as_deref(), Some("id"));
        assert_eq!(scan.max_scrolls, 4);
        assert_eq!(scan.scroll_amount, 900);
        assert_eq!(scan.settle_ms, 300);
        assert_eq!(scan.stall_limit, 2);
    }

    #[test]
    fn normalize_shorthand_converts_string_values_to_selector_objects() {
        let mut value: serde_json::Value = serde_json::from_str(
            r#"{"title":"h1","price":".price","link":{"selector":"a","attr":"href"}}"#,
        )
        .expect("test JSON should parse");
        super::spec::normalize_extract_spec_shorthands_in_place(&mut value);

        // String values should be expanded to full objects
        assert_eq!(value["title"]["selector"], "h1");
        assert_eq!(value["title"]["kind"], "text");
        assert_eq!(value["price"]["selector"], ".price");
        assert_eq!(value["price"]["kind"], "text");

        // Object values should pass through unchanged
        assert_eq!(value["link"]["selector"], "a");
        assert_eq!(value["link"]["attr"], "href");
    }

    #[test]
    fn extract_field_defaults_to_text_kind_when_omitted() {
        let field: ExtractFieldSpec = serde_json::from_value(json!({
            "selector": "#headline"
        }))
        .expect("extract field without kind should default to text");

        assert_eq!(field.selector.as_deref(), Some("#headline"));
        assert!(matches!(field.kind, ExtractKind::Text));
    }

    #[test]
    fn extract_field_infers_attribute_kind_when_attr_present() {
        let field: ExtractFieldSpec = serde_json::from_value(json!({
            "selector": "a.main",
            "attr": "href"
        }))
        .expect("extract field with attr alias should infer attribute kind");

        assert!(matches!(field.kind, ExtractKind::Attribute));
        assert_eq!(field.attribute.as_deref(), Some("href"));
    }

    #[test]
    fn extract_field_accepts_attr_as_alias_for_attribute() {
        let field: ExtractFieldSpec = serde_json::from_value(json!({
            "selector": "img",
            "kind": "attribute",
            "attr": "src"
        }))
        .expect("attr alias should be accepted");

        assert_eq!(field.attribute.as_deref(), Some("src"));
        assert!(matches!(field.kind, ExtractKind::Attribute));
    }
}
