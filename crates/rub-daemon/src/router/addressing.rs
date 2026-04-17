mod memo;
mod projection;
mod semantic;
mod snapshot;

use self::memo::{ParsedLocator, record_memoized_elements, restore_memoized_elements};
use self::projection::{ambiguous_locator_error, locator_context, selection_context};
#[cfg(test)]
use self::semantic::element_matches_text;
use self::semantic::{
    resolve_elements_by_label, resolve_elements_by_role, resolve_elements_by_testid,
    resolve_elements_by_text,
};
use self::snapshot::load_snapshot as load_addressed_snapshot;
use super::*;
use crate::router::element_semantics::{
    has_snapshot_visible_bbox, is_prefer_enabled_blocked_in_snapshot,
};
use crate::router::request_args::{LocatorParseOptions, parse_canonical_locator};
use rub_core::locator::{CanonicalLocator, LocatorSelection};
use rub_core::model::{Element, Snapshot};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LocatorRankingPolicy {
    pub(super) visible: bool,
    pub(super) prefer_enabled: bool,
    pub(super) topmost: bool,
}

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

pub(super) async fn resolve_element(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    command_name: &str,
) -> Result<ResolvedElement, RubError> {
    let resolved = resolve_elements(router, args, state, deadline, command_name).await?;
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

pub(super) async fn load_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    prefer_a11y: bool,
) -> Result<Arc<Snapshot>, RubError> {
    load_addressed_snapshot(router, args, state, deadline, prefer_a11y).await
}

pub(super) async fn resolve_elements(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    command_name: &str,
) -> Result<ResolvedElements, RubError> {
    let locator = parse_locator(args)?;
    let snapshot = load_snapshot(
        router,
        args,
        state,
        deadline,
        locator.requires_a11y_snapshot(),
    )
    .await?;
    let candidate_elements =
        if let Some(elements) = restore_memoized_elements(state, &snapshot, &locator).await? {
            elements
        } else {
            let elements =
                resolve_elements_against_locator(router, &snapshot, &locator.locator).await?;
            record_memoized_elements(state, &snapshot, &locator, &elements).await;
            elements
        };
    let elements = apply_live_ranking(router, &snapshot, &locator, candidate_elements).await?;
    let elements = apply_disambiguation(elements, &locator, args)?;

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
    let elements = apply_live_ranking(router, snapshot, &parsed, elements).await?;
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
    let ranking = LocatorRankingPolicy {
        visible: args
            .get("visible")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        prefer_enabled: args
            .get("prefer_enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        topmost: args
            .get("topmost")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    };
    Ok(ParsedLocator { locator, ranking })
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
            // Selector matching is a live, frame-scoped probe against the snapshot's frame.
            // We still project matches back onto snapshot element indices, so this is not a
            // pure in-memory lookup and intentionally does not participate in locator memo.
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

async fn apply_live_ranking(
    router: &DaemonRouter,
    snapshot: &Snapshot,
    locator: &ParsedLocator,
    elements: Vec<Element>,
) -> Result<Vec<Element>, RubError> {
    if locator.ranking.topmost {
        return router
            .browser
            .filter_snapshot_elements_by_hit_test(snapshot, &elements)
            .await;
    }
    Ok(elements)
}

fn apply_disambiguation(
    mut elements: Vec<Element>,
    locator: &ParsedLocator,
    args: &serde_json::Value,
) -> Result<Vec<Element>, RubError> {
    if locator.ranking.visible {
        elements.retain(has_snapshot_visible_bbox);
    }

    let Some(selection) = locator.locator.selection() else {
        if locator.ranking.prefer_enabled {
            elements.sort_by_key(|element| {
                (
                    is_prefer_enabled_blocked_in_snapshot(element),
                    element.index,
                )
            });
        }
        return Ok(elements);
    };
    if elements.is_empty() {
        return Ok(elements);
    }

    elements.sort_by_key(|element| {
        (
            locator.ranking.prefer_enabled && is_prefer_enabled_blocked_in_snapshot(element),
            element.index,
        )
    });
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
                "ranking_policy": {
                    "visible": locator.ranking.visible,
                    "prefer_enabled": locator.ranking.prefer_enabled,
                    "topmost": locator.ranking.topmost,
                },
                "selection": selection_context(selection),
            }),
        )
    })
}

#[cfg(test)]
mod tests;
