use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::locator::LocatorSelection;
use rub_core::model::Element;

pub(super) fn ambiguous_locator_error(
    command_name: &str,
    args: &serde_json::Value,
    matches: &[Element],
) -> RubError {
    let locator = locator_context(args);

    RubError::Domain(
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            format!(
                "{command_name} locator matched {} interactive snapshot elements; refine the locator",
                matches.len()
            ),
        )
        .with_context(serde_json::json!({
            "locator": locator,
            "candidates": matches
                .iter()
                .take(5)
                .map(|element| serde_json::json!({
                    "index": element.index,
                    "tag": element.tag,
                    "text": element.text,
                }))
                .collect::<Vec<_>>(),
            "selection": selection_context_from_args(args),
        }))
        .with_suggestion(
            "Refine the locator, or use --first, --last, or --nth to select a single match",
        ),
    )
}

pub(super) fn locator_context(args: &serde_json::Value) -> serde_json::Value {
    for (key, alias) in [
        ("element_ref", "ref"),
        ("ref", "ref"),
        ("selector", "selector"),
        ("target_text", "target_text"),
        ("role", "role"),
        ("label", "label"),
        ("testid", "testid"),
    ] {
        if let Some(value) = args.get(key).and_then(|value| value.as_str()) {
            return serde_json::json!({ alias: value });
        }
    }
    serde_json::json!({ "index": args.get("index") })
}

pub(super) fn selection_context(selection: LocatorSelection) -> serde_json::Value {
    match selection {
        LocatorSelection::First => serde_json::json!({ "first": true }),
        LocatorSelection::Last => serde_json::json!({ "last": true }),
        LocatorSelection::Nth(nth) => serde_json::json!({ "nth": nth }),
    }
}

fn selection_context_from_args(args: &serde_json::Value) -> serde_json::Value {
    if args.get("first").and_then(|value| value.as_bool()) == Some(true) {
        return serde_json::json!({ "first": true });
    }
    if args.get("last").and_then(|value| value.as_bool()) == Some(true) {
        return serde_json::json!({ "last": true });
    }
    if let Some(nth) = args.get("nth").and_then(|value| value.as_u64()) {
        return serde_json::json!({ "nth": nth });
    }
    serde_json::Value::Null
}
