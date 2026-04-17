use super::{
    ExtractCommand, ExtractFieldSpec, ExtractKind, builder_locator_expression,
    explain_extract_spec_contract, extract_builder_field_examples,
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
    super::spec::normalize_extract_spec_shorthands_in_place(&mut value)
        .expect("top-level shorthand normalization should succeed");

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

#[test]
fn explain_extract_spec_contract_returns_normalized_spec_and_summaries() {
    let result = explain_extract_spec_contract(
        r#"{"title":"h1","items":{"collection":"li.item","fields":{"price":{"selector":".price","kind":"text","transform":"parse_float","type":"number"}}}}"#,
        std::path::Path::new("/tmp/nonexistent-rub-home-for-extract-explain"),
    )
    .expect("explain contract should succeed");

    assert_eq!(result["subject"]["kind"], "extract_explain");
    assert_eq!(result["result"]["normalized_spec"]["title"]["kind"], "text");
    assert_eq!(
        result["result"]["normalized_spec"]["items"]["fields"]["price"]["transform"],
        "parse_float"
    );
    assert_eq!(
        result["result"]["entry_summaries"][1]["path"],
        "items.price"
    );
    assert_eq!(
        result["result"]["guidance"]["schema_command"],
        "rub extract --schema"
    );
}

#[test]
fn explain_extract_spec_contract_enriches_parse_errors_with_guidance() {
    let error = explain_extract_spec_contract(
        r#"{"title":{"unknown_key":true}}"#,
        std::path::Path::new("/tmp/nonexistent-rub-home-for-extract-explain"),
    )
    .expect_err("invalid extract spec should fail");
    let envelope = error.into_envelope();
    let context = envelope.context.expect("context");
    assert_eq!(context["schema_command"], "rub extract --schema");
    assert_eq!(context["examples_command"], "rub extract --examples");
}
