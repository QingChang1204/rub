mod json;
mod locator;

pub(crate) use self::json::{
    parse_json_args, parse_json_spec, reject_unknown_fields, required_string_arg,
};
pub(crate) use self::json::{parse_optional_u32_arg, subcommand_arg};
pub(crate) use self::locator::parse_canonical_locator;
pub(crate) use self::locator::{
    LocatorParseOptions, LocatorRequestArgs, canonical_locator_json, locator_json,
    parse_canonical_locator_from_value, require_live_locator,
};

#[cfg(test)]
mod tests {
    use super::{
        LocatorParseOptions, LocatorRequestArgs, canonical_locator_json, locator_json,
        parse_canonical_locator, parse_json_spec, required_string_arg,
    };
    use rub_core::error::ErrorCode;
    use rub_core::error::RubError;
    use rub_core::locator::{CanonicalLocator, LocatorSelection};
    use rub_core::model::{OrchestrationRegistrationSpec, TriggerRegistrationSpec};

    #[test]
    fn locator_json_emits_canonical_locator_shape() {
        let locator = locator_json(LocatorRequestArgs {
            index: Some(3),
            element_ref: Some("frame:42".to_string()),
            selector: Some("button.primary".to_string()),
            target_text: Some("Continue".to_string()),
            role: Some("button".to_string()),
            label: Some("Continue".to_string()),
            testid: Some("primary-cta".to_string()),
            first: true,
            last: false,
            nth: Some(2),
        });
        assert_eq!(locator["index"], 3);
        assert_eq!(locator["element_ref"], "frame:42");
        assert_eq!(locator["selector"], "button.primary");
        assert_eq!(locator["target_text"], "Continue");
        assert_eq!(locator["role"], "button");
        assert_eq!(locator["label"], "Continue");
        assert_eq!(locator["testid"], "primary-cta");
        assert_eq!(locator["first"], true);
        assert_eq!(locator["last"], false);
        assert_eq!(locator["nth"], 2);
    }

    #[test]
    fn parse_json_spec_reports_command_name_on_error() {
        let error = parse_json_spec::<Vec<String>>("{", "fill").unwrap_err();
        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("fill"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }

    #[test]
    fn required_string_arg_rejects_missing_fields() {
        let error = required_string_arg(&serde_json::json!({}), "spec").unwrap_err();
        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("spec"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }

    #[test]
    fn parse_canonical_locator_supports_semantic_locator_selection() {
        let locator = parse_canonical_locator(
            &serde_json::json!({
                "role": "button",
                "nth": 2
            }),
            LocatorParseOptions::ELEMENT_ADDRESS,
        )
        .expect("role locator should parse")
        .expect("locator should exist");

        assert_eq!(
            locator,
            CanonicalLocator::Role {
                role: "button".to_string(),
                selection: Some(LocatorSelection::Nth(2)),
            }
        );
        assert_eq!(
            canonical_locator_json(&locator),
            serde_json::json!({
                "role": "button",
                "nth": 2
            })
        );
    }

    #[test]
    fn parse_canonical_locator_rejects_ambiguous_selection() {
        let error = parse_canonical_locator(
            &serde_json::json!({
                "selector": ".item",
                "first": true,
                "last": true
            }),
            LocatorParseOptions::ELEMENT_ADDRESS,
        )
        .unwrap_err();

        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("ambiguous"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }

    #[test]
    fn parse_canonical_locator_rejects_selector_and_ref_mix() {
        let error = parse_canonical_locator(
            &serde_json::json!({
                "selector": ".item",
                "ref": "frame:0"
            }),
            LocatorParseOptions::ELEMENT_ADDRESS,
        )
        .unwrap_err();

        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("ambiguous"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }

    #[test]
    fn parse_canonical_locator_rejects_index_when_live_only() {
        let error = parse_canonical_locator(
            &serde_json::json!({
                "index": 1
            }),
            LocatorParseOptions::LIVE_ONLY,
        )
        .unwrap_err();

        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("semantic locators"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_spec_preserves_orchestration_metadata() {
        let spec = parse_json_spec::<OrchestrationRegistrationSpec>(
            r#"{
                "source": { "session_id": "source" },
                "target": { "session_id": "target" },
                "condition": { "kind": "text_present", "text": "Ready" },
                "actions": [
                    {
                        "kind": "browser_command",
                        "command": "pipe",
                        "payload": {
                            "steps": [],
                            "_orchestration": {
                                "correlation_key": "corr-123",
                                "idempotency_key": "idem-123"
                            }
                        }
                    }
                ]
            }"#,
            "orchestration add",
        )
        .expect("registration spec should parse");

        let args = spec.actions[0]
            .payload
            .as_ref()
            .expect("pipe action should preserve args");
        assert_eq!(args["_orchestration"]["correlation_key"], "corr-123");
        assert_eq!(args["_orchestration"]["idempotency_key"], "idem-123");
    }

    #[test]
    fn parse_json_spec_preserves_trigger_metadata() {
        let spec = parse_json_spec::<TriggerRegistrationSpec>(
            r#"{
                "source_tab": 0,
                "target_tab": 1,
                "condition": { "kind": "text_present", "text": "Ready" },
                "action": {
                    "kind": "browser_command",
                    "command": "pipe",
                    "payload": {
                        "steps": [],
                        "_trigger": {
                            "trigger_id": "trg-123",
                            "watch_revision": 7
                        }
                    }
                }
            }"#,
            "trigger add",
        )
        .expect("trigger spec should parse");

        let args = spec
            .action
            .payload
            .as_ref()
            .expect("pipe action should preserve args");
        assert_eq!(args["_trigger"]["trigger_id"], "trg-123");
        assert_eq!(args["_trigger"]["watch_revision"], 7);
    }
}
