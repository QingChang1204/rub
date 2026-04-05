use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::{CanonicalLocator, LiveLocator, LocatorSelection};

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct LocatorRequestArgs {
    pub index: Option<u32>,
    #[serde(alias = "ref")]
    pub element_ref: Option<String>,
    pub selector: Option<String>,
    pub target_text: Option<String>,
    pub role: Option<String>,
    pub label: Option<String>,
    pub testid: Option<String>,
    pub first: bool,
    pub last: bool,
    pub nth: Option<u32>,
}

impl LocatorRequestArgs {
    pub(super) fn is_requested(&self) -> bool {
        self.index.is_some()
            || self
                .element_ref
                .as_deref()
                .is_some_and(|element_ref| !element_ref.trim().is_empty())
            || self
                .selector
                .as_deref()
                .is_some_and(|selector| !selector.trim().is_empty())
            || self
                .target_text
                .as_deref()
                .is_some_and(|target_text| !target_text.trim().is_empty())
            || self
                .role
                .as_deref()
                .is_some_and(|role| !role.trim().is_empty())
            || self
                .label
                .as_deref()
                .is_some_and(|label| !label.trim().is_empty())
            || self
                .testid
                .as_deref()
                .is_some_and(|testid| !testid.trim().is_empty())
    }

    pub(super) fn has_selection(&self) -> bool {
        self.first || self.last || self.nth.is_some()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LocatorParseOptions {
    pub(crate) require_locator: bool,
    pub(crate) allow_index: bool,
    pub(crate) allow_ref: bool,
}

impl LocatorParseOptions {
    pub(super) const ELEMENT_ADDRESS: Self = Self {
        require_locator: true,
        allow_index: true,
        allow_ref: true,
    };

    pub(super) const OPTIONAL_WAIT: Self = Self {
        require_locator: false,
        allow_index: false,
        allow_ref: false,
    };

    pub(super) const OPTIONAL_ELEMENT_ADDRESS: Self = Self {
        require_locator: false,
        allow_index: true,
        allow_ref: true,
    };

    pub(crate) const LIVE_ONLY: Self = Self {
        require_locator: true,
        allow_index: false,
        allow_ref: false,
    };
}

pub(super) fn locator_json(locator: LocatorRequestArgs) -> serde_json::Value {
    serde_json::json!({
        "index": locator.index,
        "element_ref": locator.element_ref,
        "selector": locator.selector,
        "target_text": locator.target_text,
        "role": locator.role,
        "label": locator.label,
        "testid": locator.testid,
        "first": locator.first,
        "last": locator.last,
        "nth": locator.nth,
    })
}

pub(super) fn canonical_locator_json(locator: &CanonicalLocator) -> serde_json::Value {
    match locator {
        CanonicalLocator::Index { index } => serde_json::json!({ "index": index }),
        CanonicalLocator::Ref { element_ref } => {
            serde_json::json!({ "element_ref": element_ref })
        }
        CanonicalLocator::Selector { css, selection } => {
            attach_locator_selection(serde_json::json!({ "selector": css }), *selection)
        }
        CanonicalLocator::TargetText { text, selection } => {
            attach_locator_selection(serde_json::json!({ "target_text": text }), *selection)
        }
        CanonicalLocator::Role { role, selection } => {
            attach_locator_selection(serde_json::json!({ "role": role }), *selection)
        }
        CanonicalLocator::Label { label, selection } => {
            attach_locator_selection(serde_json::json!({ "label": label }), *selection)
        }
        CanonicalLocator::TestId { testid, selection } => {
            attach_locator_selection(serde_json::json!({ "testid": testid }), *selection)
        }
    }
}

pub(super) fn reject_unknown_fields(
    args: &serde_json::Value,
    allowed_fields: &[&str],
    surface: &str,
) -> Result<(), RubError> {
    let Some(object) = args.as_object() else {
        return Ok(());
    };
    let unknown = object
        .keys()
        .filter(|key| !allowed_fields.iter().any(|allowed| key == allowed))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(RubError::domain(
        ErrorCode::InvalidInput,
        format!("Unknown field(s) for {surface}: {}", unknown.join(", ")),
    ))
}

pub(super) fn parse_canonical_locator(
    args: &serde_json::Value,
    options: LocatorParseOptions,
) -> Result<Option<CanonicalLocator>, RubError> {
    parse_canonical_locator_from_value(args, options)
}

pub(crate) fn parse_canonical_locator_from_value(
    args: &serde_json::Value,
    options: LocatorParseOptions,
) -> Result<Option<CanonicalLocator>, RubError> {
    let index = parse_optional_u32_arg(args, "index")?;
    let element_ref = args
        .get("element_ref")
        .or_else(|| args.get("ref"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let selector = args
        .get("selector")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let target_text = args
        .get("target_text")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let role = args
        .get("role")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let label = args
        .get("label")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let testid = args
        .get("testid")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let selection = parse_locator_selection(args)?;

    let raw_configured = index.is_some() as u8
        + element_ref.is_some() as u8
        + selector.is_some() as u8
        + target_text.is_some() as u8
        + role.is_some() as u8
        + label.is_some() as u8
        + testid.is_some() as u8;
    let configured = (index.is_some() && options.allow_index) as u8
        + (element_ref.is_some() && options.allow_ref) as u8
        + selector.is_some() as u8
        + target_text.is_some() as u8
        + role.is_some() as u8
        + label.is_some() as u8
        + testid.is_some() as u8;

    if raw_configured > 0 && raw_configured != configured {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            disallowed_locator_message(options),
        ));
    }

    if configured == 0 {
        if options.require_locator {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                missing_locator_message(options),
            ));
        }
        return Ok(None);
    }
    if configured > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            ambiguous_locator_message(options),
        ));
    }

    let locator = if let Some(index) = index {
        if selection.is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Match selection cannot be combined with index addressing",
            ));
        }
        CanonicalLocator::Index { index }
    } else if let Some(element_ref) = element_ref {
        if selection.is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Match selection cannot be combined with ref addressing",
            ));
        }
        CanonicalLocator::Ref { element_ref }
    } else if let Some(selector) = selector {
        CanonicalLocator::Selector {
            css: selector,
            selection,
        }
    } else if let Some(target_text) = target_text {
        CanonicalLocator::TargetText {
            text: target_text,
            selection,
        }
    } else if let Some(role) = role {
        CanonicalLocator::Role { role, selection }
    } else if let Some(label) = label {
        CanonicalLocator::Label { label, selection }
    } else if let Some(testid) = testid {
        CanonicalLocator::TestId { testid, selection }
    } else {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            missing_locator_message(options),
        ));
    };

    Ok(Some(locator))
}

pub(crate) fn require_live_locator(
    locator: CanonicalLocator,
    context: serde_json::Value,
    message: impl Into<String>,
    suggestion: impl Into<String>,
) -> Result<LiveLocator, RubError> {
    LiveLocator::try_from(locator.clone()).map_err(|invalid| {
        RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            message,
            merge_locator_context(context, &invalid),
            suggestion,
        )
    })
}

fn merge_locator_context(
    context: serde_json::Value,
    locator: &CanonicalLocator,
) -> serde_json::Value {
    match context {
        serde_json::Value::Object(mut object) => {
            object.insert("locator".to_string(), canonical_locator_json(locator));
            serde_json::Value::Object(object)
        }
        other => serde_json::json!({
            "context": other,
            "locator": canonical_locator_json(locator),
        }),
    }
}

pub(super) fn parse_json_spec<T>(raw: &str, command: &str) -> Result<T, RubError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(raw).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid JSON spec for '{command}': {error}"),
        )
    })
}

pub(super) fn parse_json_args<T>(args: &serde_json::Value, command: &str) -> Result<T, RubError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid {command} payload: {error}"),
        )
    })
}

pub(super) fn required_string_arg(
    args: &serde_json::Value,
    name: &str,
) -> Result<String, RubError> {
    args.get(name)
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Missing required argument: '{name}'"),
            )
        })
}

pub(super) fn subcommand_arg<'a>(args: &'a serde_json::Value, default: &'a str) -> &'a str {
    optional_string_arg(args, "sub").unwrap_or(default)
}

pub(super) fn optional_string_arg<'a>(args: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(|value| value.as_str())
}

pub(crate) fn parse_optional_u32_arg(
    args: &serde_json::Value,
    name: &str,
) -> Result<Option<u32>, RubError> {
    let Some(value) = args.get(name).and_then(|value| value.as_u64()) else {
        return Ok(None);
    };
    let parsed = u32::try_from(value).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Argument '{name}' is too large; expected a 32-bit unsigned integer"),
        )
    })?;
    Ok(Some(parsed))
}

fn parse_locator_selection(args: &serde_json::Value) -> Result<Option<LocatorSelection>, RubError> {
    let first = args
        .get("first")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let last = args
        .get("last")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let nth = parse_optional_u32_arg(args, "nth")?;
    let selection_count = first as u8 + last as u8 + nth.is_some() as u8;
    if selection_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Match selection is ambiguous: provide at most one of --first, --last, or --nth",
        ));
    }
    Ok(if first {
        Some(LocatorSelection::First)
    } else if last {
        Some(LocatorSelection::Last)
    } else {
        nth.map(LocatorSelection::Nth)
    })
}

fn attach_locator_selection(
    mut object: serde_json::Value,
    selection: Option<LocatorSelection>,
) -> serde_json::Value {
    let Some(selection) = selection else {
        return object;
    };
    if let Some(map) = object.as_object_mut() {
        match selection {
            LocatorSelection::First => {
                map.insert("first".to_string(), serde_json::json!(true));
            }
            LocatorSelection::Last => {
                map.insert("last".to_string(), serde_json::json!(true));
            }
            LocatorSelection::Nth(nth) => {
                map.insert("nth".to_string(), serde_json::json!(nth));
            }
        }
    }
    object
}

fn missing_locator_message(options: LocatorParseOptions) -> &'static str {
    if options.allow_index && options.allow_ref {
        "Exactly one locator is required: index, --ref, --selector, --target-text, --role, --label, or --testid"
    } else {
        "Exactly one locator is required: --selector, --target-text, --role, --label, or --testid"
    }
}

fn ambiguous_locator_message(options: LocatorParseOptions) -> &'static str {
    if options.allow_index && options.allow_ref {
        "Locator is ambiguous: provide exactly one of index, --ref, --selector, --target-text, --role, --label, or --testid"
    } else {
        "Locator is ambiguous: provide exactly one of --selector, --target-text, --role, --label, or --testid"
    }
}

fn disallowed_locator_message(options: LocatorParseOptions) -> &'static str {
    if !options.allow_index && !options.allow_ref {
        "This command only supports semantic locators: --selector, --target-text, --role, --label, or --testid"
    } else if !options.allow_index {
        "This command does not support index-based addressing"
    } else {
        "This command does not support ref-based addressing"
    }
}

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
    }

    #[test]
    fn parse_canonical_locator_rejects_index_for_waits() {
        let error = parse_canonical_locator(
            &serde_json::json!({ "index": 1 }),
            LocatorParseOptions::OPTIONAL_WAIT,
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
    fn canonical_locator_json_emits_command_locator_shape() {
        let locator = canonical_locator_json(&CanonicalLocator::TestId {
            testid: "primary-cta".to_string(),
            selection: Some(LocatorSelection::First),
        });
        assert_eq!(locator["testid"], "primary-cta");
        assert_eq!(locator["first"], true);
    }

    #[test]
    fn locator_request_args_accept_ref_alias_and_mark_locator_present() {
        let locator: LocatorRequestArgs = serde_json::from_value(serde_json::json!({
            "ref": "frame:42",
            "first": true,
        }))
        .expect("ref alias should deserialize");
        assert_eq!(locator.element_ref.as_deref(), Some("frame:42"));
        assert!(locator.is_requested());
        assert!(locator.has_selection());
    }

    #[test]
    fn parse_json_spec_rejects_unknown_trigger_fields() {
        let error = parse_json_spec::<TriggerRegistrationSpec>(
            r#"{
                "source_tab": 0,
                "target_tab": 0,
                "condition": { "kind": "url_match", "url_pattern": "https://example.com", "methdo": "GET" },
                "action": { "kind": "browser_command", "command": "reload" }
            }"#,
            "trigger add",
        )
        .expect_err("unknown trigger fields should be rejected");
        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("trigger add"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_spec_rejects_unknown_orchestration_fields() {
        let error = parse_json_spec::<OrchestrationRegistrationSpec>(
            r#"{
                "source": { "session_id": "source" },
                "target": { "session_id": "target" },
                "condition": { "kind": "url_match", "url_pattern": "https://example.com" },
                "actions": [{ "kind": "browser_command", "command": "reload" }],
                "idempotency_keyy": "oops"
            }"#,
            "orchestration add",
        )
        .expect_err("unknown orchestration fields should be rejected");
        match error {
            RubError::Domain(domain) => {
                assert!(matches!(domain.code, ErrorCode::InvalidInput));
                assert!(domain.message.contains("orchestration add"));
            }
            other => panic!("expected domain error, got {other:?}"),
        }
    }
}
