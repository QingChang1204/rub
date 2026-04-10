use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::{CanonicalLocator, LiveLocator, LocatorSelection};

use super::json::parse_optional_u32_arg;

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LocatorRequestArgs {
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
    pub(crate) fn is_requested(&self) -> bool {
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

    pub(crate) fn has_selection(&self) -> bool {
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
    pub(crate) const ELEMENT_ADDRESS: Self = Self {
        require_locator: true,
        allow_index: true,
        allow_ref: true,
    };

    pub(crate) const OPTIONAL_WAIT: Self = Self {
        require_locator: false,
        allow_index: false,
        allow_ref: false,
    };

    pub(crate) const OPTIONAL_ELEMENT_ADDRESS: Self = Self {
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

pub(crate) fn locator_json(locator: LocatorRequestArgs) -> serde_json::Value {
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

pub(crate) fn canonical_locator_json(locator: &CanonicalLocator) -> serde_json::Value {
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

pub(crate) fn parse_canonical_locator(
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
