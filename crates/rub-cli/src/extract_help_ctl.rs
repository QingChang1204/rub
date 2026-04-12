use rub_core::error::{ErrorCode, RubError};
use serde_json::{Value, json};

const EXAMPLE_TOPICS: &[&str] = &["all", "basic", "attribute", "collection", "validation"];
const EXTRACT_KIND_VALUES: &[&str] = &["text", "value", "html", "bbox", "attributes", "attribute"];
const EXTRACT_TYPE_VALUES: &[&str] = &["string", "number", "boolean", "array", "object"];
const EXTRACT_TRANSFORM_VALUES: &[&str] = &[
    "trim",
    "lowercase",
    "uppercase",
    "parse_int",
    "parse_float",
    "parse_bool",
];

pub(crate) fn project_extract_help(
    examples: Option<&str>,
    schema: bool,
) -> Result<serde_json::Value, RubError> {
    if schema {
        return Ok(project_extract_schema());
    }
    if let Some(topic) = examples {
        return project_extract_examples(topic);
    }

    Err(RubError::domain(
        ErrorCode::InternalError,
        "extract built-in help requires --schema or --examples",
    ))
}

fn project_extract_schema() -> Value {
    json!({
        "subject": {
            "kind": "extract_help",
            "surface": "built_in",
        },
        "result": {
            "kind": "schema",
            "root_shape": {
                "type": "object",
                "field_names": "each top-level key becomes one output field",
                "string_shorthand": r#"{"title":"h1"} -> {"title":{"selector":"h1","kind":"text"}}"#,
            },
            "field_descriptor": {
                "locator_keys": ["index", "ref", "selector", "target_text", "role", "label", "testid"],
                "multi_match_keys": ["first", "last", "nth", "many"],
                "value_keys": ["kind", "type", "attr", "attribute", "required", "default", "map", "transform"],
                "kind_values": EXTRACT_KIND_VALUES,
                "type_values": EXTRACT_TYPE_VALUES,
                "transform_values": EXTRACT_TRANSFORM_VALUES,
                "notes": [
                    "kind defaults to text when omitted",
                    "kind defaults to attribute when attr/attribute is present",
                    "type validates the final projected JSON value after map/transform",
                    "many:true returns an array instead of enforcing a single match"
                ],
            },
            "collection_descriptor": {
                "keys": ["collection", "selector", "target_text", "role", "label", "testid", "row_scope_selector", "first", "last", "nth", "fields"],
                "fields_value_shape": "nested object of child field or child collection specs",
                "notes": [
                    "collection returns an array of row objects",
                    "selector and collection are aliases for the row root selector",
                    "row_scope_selector narrows child extraction within each row"
                ],
            },
        }
    })
}

fn project_extract_examples(topic: &str) -> Result<Value, RubError> {
    let normalized = topic.trim().to_ascii_lowercase();
    let examples = match normalized.as_str() {
        "all" => vec![
            example_basic(),
            example_attribute(),
            example_collection(),
            example_validation(),
        ],
        "basic" => vec![example_basic()],
        "attribute" => vec![example_attribute()],
        "collection" => vec![example_collection()],
        "validation" => vec![example_validation()],
        _ => {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("unknown extract examples topic '{topic}'"),
                json!({
                    "available_topics": EXAMPLE_TOPICS,
                    "suggestion": "use `rub extract --examples` to print all built-in examples",
                }),
            ));
        }
    };

    Ok(json!({
        "subject": {
            "kind": "extract_help",
            "surface": "built_in",
        },
        "result": {
            "kind": "examples",
            "topic": normalized,
            "available_topics": EXAMPLE_TOPICS,
            "examples": examples,
        }
    }))
}

fn example_basic() -> Value {
    json!({
        "title": "basic field extraction",
        "command": r#"rub extract '{"title":"h1","price":".price"}'"#,
        "spec": {
            "title": "h1",
            "price": ".price",
        },
        "notes": [
            "string shorthand expands to {selector, kind:\"text\"}",
            "use this when text content is the final value"
        ],
    })
}

fn example_attribute() -> Value {
    json!({
        "title": "attribute extraction",
        "command": r#"rub extract '{"link":{"selector":"a.main","kind":"attribute","attr":"href"}}'"#,
        "spec": {
            "link": {
                "selector": "a.main",
                "kind": "attribute",
                "attr": "href",
            }
        },
        "notes": [
            "attr and attribute are aliases",
            "attribute kind is inferred automatically when attr is present"
        ],
    })
}

fn example_collection() -> Value {
    json!({
        "title": "collection extraction",
        "command": r#"rub extract '{"items":{"collection":"li.item","fields":{"name":{"kind":"text"},"price":{"selector":".price"}}}}'"#,
        "spec": {
            "items": {
                "collection": "li.item",
                "fields": {
                    "name": {
                        "kind": "text",
                    },
                    "price": {
                        "selector": ".price",
                    }
                }
            }
        },
        "notes": [
            "collection returns an array of row objects",
            "nested fields are resolved relative to each row"
        ],
    })
}

fn example_validation() -> Value {
    json!({
        "title": "post-processing and type validation",
        "command": r#"rub extract '{"total":{"selector":".total","kind":"text","transform":"parse_float","type":"number","required":true}}'"#,
        "spec": {
            "total": {
                "selector": ".total",
                "kind": "text",
                "transform": "parse_float",
                "type": "number",
                "required": true,
            }
        },
        "notes": [
            "transform runs before type validation",
            "required:false allows null when no match is found",
            "default provides a fallback value before type validation"
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::project_extract_help;
    use rub_core::error::ErrorCode;

    #[test]
    fn schema_surface_lists_supported_field_shapes() {
        let result = project_extract_help(None, true).expect("schema should project");
        assert_eq!(result["result"]["kind"], "schema");
        assert_eq!(
            result["result"]["field_descriptor"]["kind_values"],
            serde_json::json!(["text", "value", "html", "bbox", "attributes", "attribute"])
        );
        assert_eq!(
            result["result"]["collection_descriptor"]["keys"],
            serde_json::json!([
                "collection",
                "selector",
                "target_text",
                "role",
                "label",
                "testid",
                "row_scope_selector",
                "first",
                "last",
                "nth",
                "fields"
            ])
        );
    }

    #[test]
    fn examples_surface_accepts_topic_aliases() {
        let result =
            project_extract_help(Some("attribute"), false).expect("examples should project");
        assert_eq!(result["result"]["kind"], "examples");
        assert_eq!(result["result"]["topic"], "attribute");
        assert_eq!(result["result"]["examples"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn examples_surface_rejects_unknown_topics_with_guidance() {
        let error =
            project_extract_help(Some("unknown"), false).expect_err("unknown topic should fail");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("examples topic context");
        assert_eq!(
            context["available_topics"],
            serde_json::json!(["all", "basic", "attribute", "collection", "validation"])
        );
    }
}
