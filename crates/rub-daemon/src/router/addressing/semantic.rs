use crate::router::element_semantics::{accessible_label, semantic_role, test_id};
use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::CanonicalLocator;
use rub_core::model::{Element, Snapshot};

pub(super) fn resolve_elements_by_text(
    snapshot: &Snapshot,
    query: &str,
) -> Result<Vec<Element>, RubError> {
    let normalized_query = normalize_locator_text(query);
    let exact = text_matches(snapshot, &normalized_query, true);
    if !exact.is_empty() {
        return Ok(exact);
    }

    let contains = text_matches(snapshot, &normalized_query, false);
    if contains.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!("No interactive snapshot element matched text '{query}'"),
            serde_json::json!({
                "target_text": query,
            }),
            "Run 'rub observe' to see all interactive elements, or use --selector for content-level matching",
        ));
    }

    Ok(contains)
}

pub(super) fn resolve_elements_by_role(
    snapshot: &Snapshot,
    query: &str,
) -> Result<Vec<Element>, RubError> {
    let normalized_query = normalize_locator_text(query);
    let mut matches = snapshot
        .elements
        .iter()
        .filter(|element| normalize_locator_text(&semantic_role(element)) == normalized_query)
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by_key(|element| element.index);
    if matches.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!("No interactive snapshot element matched role '{query}'"),
            serde_json::json!({
                "role": query,
            }),
            "Run 'rub observe' to see all interactive elements and their roles",
        ));
    }
    Ok(matches)
}

pub(super) fn resolve_elements_by_label(
    snapshot: &Snapshot,
    query: &str,
) -> Result<Vec<Element>, RubError> {
    let normalized_query = normalize_locator_text(query);
    let mut exact = snapshot
        .elements
        .iter()
        .filter(|element| normalize_locator_text(&accessible_label(element)) == normalized_query)
        .cloned()
        .collect::<Vec<_>>();
    exact.sort_by_key(|element| element.index);
    if !exact.is_empty() {
        return Ok(exact);
    }

    let mut contains = snapshot
        .elements
        .iter()
        .filter(|element| {
            let label = accessible_label(element);
            !label.is_empty() && normalize_locator_text(&label).contains(&normalized_query)
        })
        .cloned()
        .collect::<Vec<_>>();
    contains.sort_by_key(|element| element.index);
    if contains.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!("No interactive snapshot element matched label '{query}'"),
            serde_json::json!({
                "label": query,
            }),
            "Run 'rub observe' to see all interactive elements and their labels",
        ));
    }
    Ok(contains)
}

pub(super) fn resolve_elements_by_testid(
    snapshot: &Snapshot,
    query: &str,
) -> Result<Vec<Element>, RubError> {
    let normalized_query = normalize_locator_text(query);
    let mut matches = snapshot
        .elements
        .iter()
        .filter(|element| {
            test_id(element)
                .map(normalize_locator_text)
                .is_some_and(|value| value == normalized_query)
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by_key(|element| element.index);
    if matches.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!("No interactive snapshot element matched test id '{query}'"),
            serde_json::json!({
                "testid": query,
            }),
            "Run 'rub observe' to see all interactive elements and their test IDs",
        ));
    }
    Ok(matches)
}

pub(super) fn validate_memoized_elements(locator: &CanonicalLocator, elements: &[Element]) -> bool {
    match locator {
        CanonicalLocator::Index { index } => {
            matches!(elements, [element] if element.index == *index)
        }
        CanonicalLocator::Ref { element_ref } => {
            matches!(elements, [element] if element.element_ref.as_deref() == Some(element_ref.as_str()))
        }
        CanonicalLocator::Selector { .. } => false,
        CanonicalLocator::TargetText { text: query, .. } => {
            let normalized_query = normalize_locator_text(query);
            elements
                .iter()
                .all(|element| element_matches_text(element, &normalized_query, true))
                || elements
                    .iter()
                    .all(|element| element_matches_text(element, &normalized_query, false))
        }
        CanonicalLocator::Role { role, .. } => {
            let normalized_query = normalize_locator_text(role);
            elements
                .iter()
                .all(|element| normalize_locator_text(&semantic_role(element)) == normalized_query)
        }
        CanonicalLocator::Label { label, .. } => {
            let normalized_query = normalize_locator_text(label);
            elements.iter().all(|element| {
                let candidate = accessible_label(element);
                !candidate.is_empty()
                    && normalize_locator_text(&candidate).contains(&normalized_query)
            })
        }
        CanonicalLocator::TestId { testid, .. } => {
            let normalized_query = normalize_locator_text(testid);
            elements.iter().all(|element| {
                test_id(element)
                    .map(normalize_locator_text)
                    .is_some_and(|value| value == normalized_query)
            })
        }
    }
}

pub(super) fn normalize_locator_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn text_matches(snapshot: &Snapshot, query: &str, exact: bool) -> Vec<Element> {
    let mut matches = snapshot
        .elements
        .iter()
        .filter(|element| element_matches_text(element, query, exact))
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by_key(|element| element.index);
    matches
}

pub(super) fn element_matches_text(element: &Element, query: &str, exact: bool) -> bool {
    let mut candidates = Vec::with_capacity(1 + element.attributes.len());
    if !element.text.trim().is_empty() {
        candidates.push(element.text.as_str());
    }
    for key in ["aria-label", "placeholder", "title", "alt", "value", "name"] {
        if let Some(value) = element.attributes.get(key) {
            candidates.push(value.as_str());
        }
    }

    candidates.into_iter().any(|candidate| {
        let normalized_candidate = normalize_locator_text(candidate);
        if exact {
            normalized_candidate == query
        } else {
            normalized_candidate.contains(query)
        }
    })
}
