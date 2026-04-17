use super::ExtractCollectionSpec;
use super::projection::nested_collection_result;

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
