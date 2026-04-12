use crate::router::element_semantics::{
    accessible_label, has_snapshot_visible_bbox, is_disabled_in_snapshot, semantic_role, test_id,
};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::locator::LocatorSelection;
use rub_core::model::Element;
use std::collections::HashMap;

pub(super) fn ambiguous_locator_error(
    command_name: &str,
    args: &serde_json::Value,
    matches: &[Element],
) -> RubError {
    let locator = locator_context(args);
    let ordering_policy = ordering_policy_from_args(args);

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
            "ordering_policy": ordering_policy,
            "candidates": project_locator_candidates(matches, ordering_policy),
            "disambiguation_hints": project_locator_disambiguation_hints(matches),
            "selection": selection_context_from_args(args),
        }))
        .with_suggestion(ambiguous_locator_suggestion(args, matches)),
    )
}

fn ambiguous_locator_suggestion(args: &serde_json::Value, matches: &[Element]) -> String {
    let mut guidance =
        "Refine the locator, or use --first, --last, or --nth to select a single match".to_string();

    let visible_requested = args
        .get("visible")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !visible_requested
        && matches
            .iter()
            .filter(|element| has_snapshot_visible_bbox(element))
            .count()
            == 1
    {
        guidance.push_str(
            ". `--visible` would keep only the single candidate with an authoritative visible bounding box",
        );
    }

    guidance
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

fn ordering_policy_from_args(args: &serde_json::Value) -> &'static str {
    match (
        args.get("visible")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        args.get("prefer_enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        args.get("topmost")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    ) {
        (true, true, true) => {
            "visible_filter_then_topmost_hit_test_then_prefer_enabled_then_snapshot_index"
        }
        (true, true, false) => "visible_filter_then_prefer_enabled_then_snapshot_index",
        (true, false, true) => "visible_filter_then_topmost_hit_test_then_snapshot_index",
        (true, false, false) => "visible_filter_then_snapshot_index",
        (false, true, true) => "topmost_hit_test_then_prefer_enabled_then_snapshot_index",
        (false, true, false) => "prefer_enabled_then_snapshot_index",
        (false, false, true) => "topmost_hit_test_then_snapshot_index",
        (false, false, false) => "snapshot_index",
    }
}

fn project_locator_candidates(
    matches: &[Element],
    ordering_policy: &str,
) -> Vec<serde_json::Value> {
    let frequencies = candidate_field_frequencies(matches);
    matches
        .iter()
        .take(5)
        .enumerate()
        .map(|(rank, element)| {
            let label = accessible_label(element);
            let role = semantic_role(element);
            let testid = test_id(element);
            let text = element.text.trim();
            serde_json::json!({
                "rank": rank,
                "index": element.index,
                "tag": element.tag,
                "text": element.text,
                "role": role,
                "label": label,
                "testid": testid,
                "element_ref": element.element_ref,
                "snapshot_visible": has_snapshot_visible_bbox(element),
                "enabled": !is_disabled_in_snapshot(element),
                "ranking_hints": {
                    "ordered_by": ordering_policy,
                    "unique_anchors": {
                        "label": unique_field_value(&frequencies.labels, &label),
                        "role": unique_field_value(&frequencies.roles, &role),
                        "testid": testid.filter(|value| unique_field_value(&frequencies.testids, value).is_some()),
                        "text": non_empty(text).filter(|value| unique_field_value(&frequencies.texts, value).is_some()),
                    }
                }
            })
        })
        .collect()
}

fn project_locator_disambiguation_hints(matches: &[Element]) -> serde_json::Value {
    let frequencies = candidate_field_frequencies(matches);
    serde_json::json!({
        "labels": unique_frequency_values(&frequencies.labels),
        "roles": unique_frequency_values(&frequencies.roles),
        "testids": unique_frequency_values(&frequencies.testids),
        "texts": unique_frequency_values(&frequencies.texts),
    })
}

struct CandidateFieldFrequencies {
    labels: HashMap<String, usize>,
    roles: HashMap<String, usize>,
    testids: HashMap<String, usize>,
    texts: HashMap<String, usize>,
}

fn candidate_field_frequencies(matches: &[Element]) -> CandidateFieldFrequencies {
    let mut frequencies = CandidateFieldFrequencies {
        labels: HashMap::new(),
        roles: HashMap::new(),
        testids: HashMap::new(),
        texts: HashMap::new(),
    };
    for element in matches {
        if let Some(label) = non_empty(&accessible_label(element)) {
            *frequencies.labels.entry(label.to_string()).or_insert(0) += 1;
        }
        if let Some(role) = non_empty(&semantic_role(element)) {
            *frequencies.roles.entry(role.to_string()).or_insert(0) += 1;
        }
        if let Some(testid) = test_id(element).and_then(non_empty) {
            *frequencies.testids.entry(testid.to_string()).or_insert(0) += 1;
        }
        if let Some(text) = non_empty(&element.text) {
            *frequencies.texts.entry(text.to_string()).or_insert(0) += 1;
        }
    }
    frequencies
}

fn unique_frequency_values(frequencies: &HashMap<String, usize>) -> Vec<String> {
    let mut values = frequencies
        .iter()
        .filter_map(|(value, count)| (*count == 1).then_some(value.clone()))
        .collect::<Vec<_>>();
    values.sort();
    if values.len() > 5 {
        values.truncate(5);
    }
    values
}

fn unique_field_value<'a>(frequencies: &HashMap<String, usize>, value: &'a str) -> Option<&'a str> {
    non_empty(value).filter(|candidate| frequencies.get(*candidate) == Some(&1))
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
