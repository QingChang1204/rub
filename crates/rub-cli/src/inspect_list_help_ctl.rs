use rub_core::error::RubError;
use serde_json::{Value, json};

const FIELD_KIND_VALUES: &[&str] = &["text", "html", "value", "bbox", "attributes", "attribute"];
const LOCATOR_FORMS: &[&str] = &[
    ".selector",
    "role:heading",
    "label:Email",
    "testid:hero-image",
    "target_text:Read more",
];
const MATCH_SUFFIXES: &[&str] = &["@first", "@last", "@many", "@nth(0)"];

pub(crate) fn project_inspect_list_builder_help() -> Result<Value, RubError> {
    Ok(json!({
        "subject": {
            "kind": "inspect_list_help",
            "surface": "built_in",
        },
        "result": {
            "kind": "builder_help",
            "collection_builder": {
                "required_flags": ["--collection", "--field"],
                "optional_flags": [
                    "--row-scope",
                    "--snapshot",
                    "--wait-field",
                    "--wait-contains",
                    "--wait-timeout",
                    "--scan-until",
                    "--scan-key",
                    "--max-scrolls",
                    "--scroll-amount",
                    "--settle-ms",
                    "--stall-limit"
                ],
                "notes": [
                    "the builder compiles to the same extract/list JSON spec shape used by inspect runtime",
                    "use JSON or --file when the list shape no longer fits one collection plus repeated --field shorthand",
                    "wait mode and scan mode are separate product surfaces in the current CLI"
                ]
            },
            "field_shorthand": {
                "root_text": "name",
                "locator_only": "name=.selector",
                "kind_then_locator": [
                    "name=text:.selector",
                    "name=html:.selector",
                    "name=value:.selector",
                    "name=bbox:.selector",
                    "name=attributes:.selector"
                ],
                "attribute_examples": [
                    "name=attribute:href:a.main",
                    "hero=attribute:src:testid:hero-image"
                ],
                "locator_forms": LOCATOR_FORMS,
                "kind_values": FIELD_KIND_VALUES,
                "match_suffixes": MATCH_SUFFIXES,
                "notes": [
                    "a bare field like `title` means root text for each row",
                    "attribute:NAME:LOCATOR automatically maps to kind=attribute and attribute=NAME",
                    "append one match suffix to resolve repeated matches inside each row"
                ]
            },
            "examples": [
                {
                    "title": "basic list builder",
                    "command": "rub inspect list --collection '.mail-row' --field 'subject=text:.subject' --field 'from=text:.from'",
                    "equivalent_spec": {
                        "items": {
                            "collection": ".mail-row",
                            "fields": {
                                "subject": { "kind": "text", "selector": ".subject" },
                                "from": { "kind": "text", "selector": ".from" }
                            }
                        }
                    }
                },
                {
                    "title": "row scope and attribute extraction",
                    "command": "rub inspect list --collection 'article.card' --row-scope '.card-body' --field 'title=.title' --field 'href=attribute:href:a.cta'",
                    "equivalent_spec": {
                        "items": {
                            "collection": "article.card",
                            "row_scope_selector": ".card-body",
                            "fields": {
                                "title": { "kind": "text", "selector": ".title" },
                                "href": { "kind": "attribute", "attribute": "href", "selector": "a.cta" }
                            }
                        }
                    }
                },
                {
                    "title": "wait for a new matching item",
                    "command": "rub inspect list --collection '.mail-row' --field 'subject=text:.subject' --wait-field subject --wait-contains 'Confirm your new account' --wait-timeout 5000",
                    "notes": [
                        "wait mode reuses the same compiled builder spec",
                        "the wait condition applies to one projected field path"
                    ]
                }
            ],
            "next_safe_actions": [
                "rub inspect list --builder-help",
                "rub extract --schema",
                "rub extract --examples collection"
            ]
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::project_inspect_list_builder_help;

    #[test]
    fn builder_help_surfaces_collection_and_field_contract() {
        let result = project_inspect_list_builder_help().expect("builder help should project");
        assert_eq!(result["result"]["kind"], "builder_help");
        assert_eq!(
            result["result"]["collection_builder"]["required_flags"],
            serde_json::json!(["--collection", "--field"])
        );
        assert_eq!(
            result["result"]["field_shorthand"]["match_suffixes"],
            serde_json::json!(["@first", "@last", "@many", "@nth(0)"])
        );
    }

    #[test]
    fn builder_help_examples_show_compiled_spec_shape() {
        let result = project_inspect_list_builder_help().expect("builder help should project");
        let examples = result["result"]["examples"]
            .as_array()
            .expect("examples array");
        assert_eq!(examples.len(), 3);
        assert_eq!(
            examples[0]["equivalent_spec"]["items"]["collection"],
            ".mail-row"
        );
        assert_eq!(
            examples[1]["equivalent_spec"]["items"]["fields"]["href"]["attribute"],
            "href"
        );
    }
}
