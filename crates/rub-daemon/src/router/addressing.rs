use super::snapshot::build_stable_snapshot;
use super::*;
use crate::locator_memo::LocatorMemoTarget;
use crate::router::element_semantics::{accessible_label, semantic_role, test_id};
use crate::router::request_args::{LocatorParseOptions, parse_canonical_locator};
use rub_core::locator::{CanonicalLocator, LocatorSelection};
use rub_core::model::{Element, Snapshot};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(super) struct ResolvedElement {
    pub element: Element,
    pub snapshot_id: String,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedElements {
    pub elements: Vec<Element>,
    pub snapshot_id: String,
}

#[derive(Debug, Clone)]
struct ParsedLocator {
    locator: CanonicalLocator,
}

impl ParsedLocator {
    fn memo_key(&self, snapshot: &Snapshot) -> Option<String> {
        let selection = self.locator.selection().map(selection_context);
        match &self.locator {
            CanonicalLocator::Index { .. } => None,
            CanonicalLocator::Ref { .. } => None,
            CanonicalLocator::Selector { css: selector, .. } => Some(
                serde_json::json!({
                    "url": snapshot.url,
                    "dom_epoch": snapshot.dom_epoch,
                    "frame_id": snapshot.frame_context.frame_id,
                    "kind": "selector",
                    "value": selector.trim(),
                    "selection": selection,
                })
                .to_string(),
            ),
            CanonicalLocator::TargetText { text, .. } => Some(
                serde_json::json!({
                    "url": snapshot.url,
                    "dom_epoch": snapshot.dom_epoch,
                    "frame_id": snapshot.frame_context.frame_id,
                    "kind": "target_text",
                    "value": normalize_locator_text(text),
                    "selection": selection,
                })
                .to_string(),
            ),
            CanonicalLocator::Role { .. }
            | CanonicalLocator::Label { .. }
            | CanonicalLocator::TestId { .. } => None,
        }
    }

    fn requires_a11y_snapshot(&self) -> bool {
        self.locator.requires_a11y_snapshot()
    }
}

pub(super) async fn resolve_element(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    command_name: &str,
) -> Result<ResolvedElement, RubError> {
    let resolved = resolve_elements(router, args, state, command_name).await?;
    match resolved.elements.as_slice() {
        [element] => Ok(ResolvedElement {
            element: element.clone(),
            snapshot_id: resolved.snapshot_id,
        }),
        [] => Err(RubError::domain(
            ErrorCode::ElementNotFound,
            format!("{command_name} did not resolve to any interactive snapshot element"),
        )),
        elements => Err(ambiguous_locator_error(command_name, args, elements)),
    }
}

pub(super) async fn resolve_elements(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    command_name: &str,
) -> Result<ResolvedElements, RubError> {
    let locator = parse_locator(args)?;
    let snapshot = load_snapshot(router, args, state, locator.requires_a11y_snapshot()).await?;
    if let Some(elements) = restore_memoized_elements(state, &snapshot, &locator).await? {
        return Ok(ResolvedElements {
            elements,
            snapshot_id: snapshot.snapshot_id.clone(),
        });
    }

    let elements = resolve_elements_against_locator(router, &snapshot, &locator.locator).await?;
    let elements = apply_disambiguation(elements, &locator, args)?;
    record_memoized_elements(state, &snapshot, &locator, &elements).await;

    if elements.is_empty() {
        return Err(RubError::domain(
            ErrorCode::ElementNotFound,
            format!("{command_name} did not resolve to any interactive snapshot element"),
        ));
    }

    Ok(ResolvedElements {
        elements,
        snapshot_id: snapshot.snapshot_id.clone(),
    })
}

pub(super) async fn resolve_elements_against_snapshot(
    router: &DaemonRouter,
    snapshot: &Snapshot,
    locator: &serde_json::Value,
    command_name: &str,
) -> Result<ResolvedElements, RubError> {
    let parsed = parse_locator(locator)?;
    let elements = resolve_elements_against_locator(router, snapshot, &parsed.locator).await?;
    let elements = apply_disambiguation(elements, &parsed, locator)?;
    if elements.is_empty() {
        return Err(RubError::domain(
            ErrorCode::ElementNotFound,
            format!("{command_name} did not resolve to any interactive snapshot element"),
        ));
    }
    Ok(ResolvedElements {
        elements,
        snapshot_id: snapshot.snapshot_id.clone(),
    })
}

fn parse_locator(args: &serde_json::Value) -> Result<ParsedLocator, RubError> {
    let locator =
        parse_canonical_locator(args, LocatorParseOptions::ELEMENT_ADDRESS)?.ok_or_else(|| {
            RubError::domain(ErrorCode::InvalidInput, "Failed to parse element locator")
        })?;
    Ok(ParsedLocator { locator })
}

async fn load_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    prefer_a11y: bool,
) -> Result<Arc<Snapshot>, RubError> {
    if let Some(snapshot_id) = args.get("snapshot_id").and_then(|value| value.as_str()) {
        crate::runtime_refresh::refresh_live_frame_runtime(&router.browser, state).await;
        let snapshot = state.get_snapshot(snapshot_id).await.ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!("Snapshot {snapshot_id} is unknown or evicted"),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "current_epoch": state.current_epoch(),
                }),
            )
        })?;

        let current_epoch = state.current_epoch();
        if snapshot.dom_epoch != current_epoch {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!(
                    "Snapshot {snapshot_id} is stale: snapshot epoch {} != current epoch {}",
                    snapshot.dom_epoch, current_epoch
                ),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "snapshot_epoch": snapshot.dom_epoch,
                    "current_epoch": current_epoch,
                }),
            ));
        }

        let frame_runtime = state.frame_runtime().await;
        if matches!(
            frame_runtime.status,
            rub_core::model::FrameContextStatus::Stale
        ) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!(
                    "Snapshot {snapshot_id} cannot be used because the selected frame context is stale"
                ),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "snapshot_frame_id": snapshot.frame_context.frame_id,
                    "frame_runtime": frame_runtime,
                }),
            ));
        }
        let current_frame_id = frame_runtime
            .current_frame
            .as_ref()
            .map(|frame| frame.frame_id.as_str());
        if current_frame_id != Some(snapshot.frame_context.frame_id.as_str()) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                format!(
                    "Snapshot {snapshot_id} belongs to frame '{}' but current frame context is '{}'",
                    snapshot.frame_context.frame_id,
                    current_frame_id.unwrap_or("unknown"),
                ),
                serde_json::json!({
                    "snapshot_id": snapshot_id,
                    "snapshot_frame_id": snapshot.frame_context.frame_id,
                    "current_frame_id": current_frame_id,
                    "frame_runtime": frame_runtime,
                }),
            ));
        }

        return Ok(snapshot);
    }

    let snapshot = build_stable_snapshot(router, args, state, None, prefer_a11y, false).await?;
    let snapshot = state.cache_snapshot(snapshot).await;
    Ok(snapshot)
}

async fn resolve_elements_against_locator(
    router: &DaemonRouter,
    snapshot: &Snapshot,
    locator: &CanonicalLocator,
) -> Result<Vec<Element>, RubError> {
    match locator {
        CanonicalLocator::Index { index } => {
            let element = snapshot
                .elements
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::ElementNotFound,
                        format!("Element at index {index} not found"),
                    )
                })?;
            Ok(vec![element])
        }
        CanonicalLocator::Ref { element_ref } => Ok(snapshot
            .elements
            .iter()
            .filter(|element| element.element_ref.as_deref() == Some(element_ref.as_str()))
            .cloned()
            .collect::<Vec<_>>()),
        CanonicalLocator::Selector { css: selector, .. } => {
            router
                .browser
                .find_snapshot_elements_by_selector(snapshot, selector)
                .await
        }
        CanonicalLocator::TargetText {
            text: target_text, ..
        } => resolve_elements_by_text(snapshot, target_text),
        CanonicalLocator::Role { role, .. } => resolve_elements_by_role(snapshot, role),
        CanonicalLocator::Label { label, .. } => resolve_elements_by_label(snapshot, label),
        CanonicalLocator::TestId { testid, .. } => resolve_elements_by_testid(snapshot, testid),
    }
}

fn apply_disambiguation(
    mut elements: Vec<Element>,
    locator: &ParsedLocator,
    args: &serde_json::Value,
) -> Result<Vec<Element>, RubError> {
    let Some(selection) = locator.locator.selection() else {
        return Ok(elements);
    };
    if elements.is_empty() {
        return Ok(elements);
    }

    elements.sort_by_key(|element| element.index);
    let selected = match selection {
        LocatorSelection::First => elements.into_iter().next(),
        LocatorSelection::Last => elements.into_iter().next_back(),
        LocatorSelection::Nth(nth) => elements.into_iter().nth(nth as usize),
    };

    selected.map(|element| vec![element]).ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::ElementNotFound,
            "Locator disambiguation selected an out-of-range match",
            serde_json::json!({
                "locator": locator_context(args),
                "selection": selection_context(selection),
            }),
        )
    })
}

async fn restore_memoized_elements(
    state: &Arc<SessionState>,
    snapshot: &Snapshot,
    locator: &ParsedLocator,
) -> Result<Option<Vec<Element>>, RubError> {
    let Some(key) = locator.memo_key(snapshot) else {
        return Ok(None);
    };
    let Some(targets) = state.lookup_locator_memo(&key).await else {
        return Ok(None);
    };
    Ok(rehydrate_memoized_elements(
        snapshot,
        &locator.locator,
        &targets,
    ))
}

async fn record_memoized_elements(
    state: &Arc<SessionState>,
    snapshot: &Snapshot,
    locator: &ParsedLocator,
    elements: &[Element],
) {
    let Some(key) = locator.memo_key(snapshot) else {
        return;
    };
    let targets = memo_targets(elements);
    if targets.is_empty() {
        return;
    }
    state.record_locator_memo(key, targets).await;
}

fn rehydrate_memoized_elements(
    snapshot: &Snapshot,
    locator: &CanonicalLocator,
    targets: &[LocatorMemoTarget],
) -> Option<Vec<Element>> {
    if targets.is_empty() {
        return None;
    }

    let mut seen = HashSet::new();
    let mut elements = Vec::with_capacity(targets.len());
    for target in targets {
        let element = match target {
            LocatorMemoTarget::ElementRef(element_ref) => snapshot
                .elements
                .iter()
                .find(|element| element.element_ref.as_deref() == Some(element_ref.as_str()))
                .cloned(),
            LocatorMemoTarget::Index(index) => snapshot
                .elements
                .get(*index as usize)
                .filter(|element| element.index == *index)
                .cloned(),
        }?;

        if !seen.insert(element.index) {
            return None;
        }
        elements.push(element);
    }

    elements.sort_by_key(|element| element.index);
    if validate_memoized_elements(locator, &elements) {
        Some(elements)
    } else {
        None
    }
}

fn validate_memoized_elements(locator: &CanonicalLocator, elements: &[Element]) -> bool {
    match locator {
        CanonicalLocator::Index { index } => {
            matches!(elements, [element] if element.index == *index)
        }
        CanonicalLocator::Ref { element_ref } => {
            matches!(elements, [element] if element.element_ref.as_deref() == Some(element_ref.as_str()))
        }
        CanonicalLocator::Selector { .. } => !elements.is_empty(),
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

fn memo_targets(elements: &[Element]) -> Vec<LocatorMemoTarget> {
    elements
        .iter()
        .filter_map(|element| {
            element
                .element_ref
                .as_ref()
                .map(|value| LocatorMemoTarget::ElementRef(value.clone()))
                .or(Some(LocatorMemoTarget::Index(element.index)))
        })
        .collect()
}

fn resolve_elements_by_text(snapshot: &Snapshot, query: &str) -> Result<Vec<Element>, RubError> {
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

fn resolve_elements_by_role(snapshot: &Snapshot, query: &str) -> Result<Vec<Element>, RubError> {
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

fn resolve_elements_by_label(snapshot: &Snapshot, query: &str) -> Result<Vec<Element>, RubError> {
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

fn resolve_elements_by_testid(snapshot: &Snapshot, query: &str) -> Result<Vec<Element>, RubError> {
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

fn element_matches_text(element: &Element, query: &str, exact: bool) -> bool {
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

fn normalize_locator_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn ambiguous_locator_error(
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

fn locator_context(args: &serde_json::Value) -> serde_json::Value {
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

fn selection_context(selection: LocatorSelection) -> serde_json::Value {
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

#[cfg(test)]
mod tests {
    use super::{
        LocatorMemoTarget, ambiguous_locator_error, element_matches_text, normalize_locator_text,
        parse_locator, rehydrate_memoized_elements, resolve_elements_by_role,
        resolve_elements_by_testid,
    };
    use rub_core::error::ErrorCode;
    use rub_core::locator::{CanonicalLocator, LocatorSelection};
    use rub_core::model::{AXInfo, Element, ElementTag};
    use std::collections::HashMap;

    #[test]
    fn normalize_locator_text_collapses_whitespace_and_case() {
        assert_eq!(normalize_locator_text("  Hello   World "), "hello world");
    }

    #[test]
    fn ambiguous_locator_error_uses_locator_specific_suggestion() {
        let err = ambiguous_locator_error(
            "click",
            &serde_json::json!({ "selector": ".cta" }),
            &[
                element(1, "Primary", None, Some("main:1")),
                element(2, "Secondary", None, Some("main:2")),
            ],
        );
        let envelope = err.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.suggestion.contains("--first"));
        assert!(envelope.suggestion.contains("--nth"));
    }

    #[test]
    fn element_matches_text_uses_text_and_accessible_attributes() {
        let mut attributes = HashMap::new();
        attributes.insert("aria-label".to_string(), "Save draft".to_string());
        let element = element(3, "", Some("Save draft"), None);

        assert!(element_matches_text(&element, "save draft", true));
        assert!(element_matches_text(&element, "draft", false));
    }

    #[test]
    fn rehydrate_memoized_text_targets_accepts_matching_elements() {
        let snapshot = snapshot(vec![
            element(0, "Cancel", None, Some("frame:10")),
            element(1, "Continue", None, Some("frame:11")),
        ]);

        let elements = rehydrate_memoized_elements(
            &snapshot,
            &CanonicalLocator::TargetText {
                text: "continue".to_string(),
                selection: None,
            },
            &[LocatorMemoTarget::ElementRef("frame:11".to_string())],
        )
        .expect("memoized element should rehydrate");

        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].index, 1);
    }

    #[test]
    fn rehydrate_memoized_text_targets_rejects_drifted_elements() {
        let snapshot = snapshot(vec![
            element(0, "Cancel", None, Some("frame:10")),
            element(1, "Proceed", None, Some("frame:11")),
        ]);

        assert!(
            rehydrate_memoized_elements(
                &snapshot,
                &CanonicalLocator::TargetText {
                    text: "continue".to_string(),
                    selection: None,
                },
                &[LocatorMemoTarget::ElementRef("frame:11".to_string())],
            )
            .is_none()
        );
    }

    #[test]
    fn rehydrate_memoized_element_ref_requires_exact_match() {
        let snapshot = snapshot(vec![
            element(0, "Cancel", None, Some("frame:10")),
            element(1, "Continue", None, Some("frame:11")),
        ]);

        let elements = rehydrate_memoized_elements(
            &snapshot,
            &CanonicalLocator::Ref {
                element_ref: "frame:11".to_string(),
            },
            &[LocatorMemoTarget::ElementRef("frame:11".to_string())],
        )
        .expect("memoized element ref should rehydrate");

        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].index, 1);
    }

    #[test]
    fn parse_locator_accepts_ref_alias() {
        let locator = parse_locator(&serde_json::json!({ "ref": "child:42" }))
            .expect("ref locator should parse");
        match locator.locator {
            CanonicalLocator::Ref { element_ref } => assert_eq!(element_ref, "child:42"),
            other => panic!("expected element-ref locator, got {other:?}"),
        }
    }

    #[test]
    fn parse_locator_accepts_role_locator_with_nth_selection() {
        let locator = parse_locator(&serde_json::json!({
            "role": "button",
            "nth": 1
        }))
        .expect("role locator should parse");
        match locator.locator {
            CanonicalLocator::Role { role, selection } => {
                assert_eq!(role, "button");
                assert_eq!(selection, Some(LocatorSelection::Nth(1)));
            }
            other => panic!("expected role locator, got {other:?}"),
        }
    }

    #[test]
    fn resolve_elements_by_role_uses_fallback_and_a11y_role() {
        let snapshot = snapshot(vec![
            element(0, "Primary", None, Some("main:10")),
            element_with_role(1, "Launch", "button", Some("Launch CTA"), Some("main:11")),
        ]);
        let matched = resolve_elements_by_role(&snapshot, "button").expect("role should match");
        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn resolve_elements_by_testid_matches_testing_attribute() {
        let snapshot = snapshot(vec![element_with_testid(0, "Save", "save-primary")]);
        let matched =
            resolve_elements_by_testid(&snapshot, "save-primary").expect("testid should match");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].index, 0);
    }

    fn snapshot(elements: Vec<Element>) -> rub_core::model::Snapshot {
        rub_core::model::Snapshot {
            snapshot_id: "snap-1".to_string(),
            dom_epoch: 7,
            frame_context: rub_core::model::FrameContextInfo {
                frame_id: "main".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            frame_lineage: vec!["main".to_string()],
            url: "https://example.test".to_string(),
            title: "Example".to_string(),
            elements,
            total_count: 2,
            truncated: false,
            scroll: rub_core::model::ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-03-30T00:00:00Z".to_string(),
            projection: rub_core::model::SnapshotProjection {
                verified: true,
                js_traversal_count: 2,
                backend_traversal_count: 2,
                resolved_ref_count: 2,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: None,
        }
    }

    fn element(
        index: u32,
        text: &str,
        aria_label: Option<&str>,
        element_ref: Option<&str>,
    ) -> Element {
        let mut attributes = HashMap::new();
        if let Some(label) = aria_label {
            attributes.insert("aria-label".to_string(), label.to_string());
        }
        Element {
            index,
            tag: ElementTag::Button,
            text: text.to_string(),
            attributes,
            element_ref: element_ref.map(ToOwned::to_owned),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: Some(0),
        }
    }

    fn element_with_role(
        index: u32,
        text: &str,
        role: &str,
        accessible_name: Option<&str>,
        element_ref: Option<&str>,
    ) -> Element {
        let mut element = element(index, text, None, element_ref);
        element.ax_info = Some(AXInfo {
            role: Some(role.to_string()),
            accessible_name: accessible_name.map(ToOwned::to_owned),
            accessible_description: None,
        });
        element
    }

    fn element_with_testid(index: u32, text: &str, testid: &str) -> Element {
        let mut element = element(index, text, None, Some("main:100"));
        element
            .attributes
            .insert("data-testid".to_string(), testid.to_string());
        element
    }
}
