use super::LocatorRankingPolicy;
use super::projection::selection_context;
use super::semantic::{normalize_locator_text, validate_memoized_elements};
use crate::locator_memo::LocatorMemoTarget;
use crate::session::SessionState;
use rub_core::locator::CanonicalLocator;
use rub_core::model::{Element, Snapshot};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(super) struct ParsedLocator {
    pub(super) locator: CanonicalLocator,
    pub(super) ranking: LocatorRankingPolicy,
}

impl ParsedLocator {
    pub(super) fn memo_key(&self, snapshot: &Snapshot) -> Option<String> {
        if self.ranking.topmost
            || !locator_supports_memo(&self.locator)
            || !snapshot_supports_locator_memo(snapshot)
        {
            return None;
        }
        let selection = self.locator.selection().map(selection_context);
        let ranking = serde_json::json!({
            "visible": self.ranking.visible,
            "prefer_enabled": self.ranking.prefer_enabled,
            "topmost": self.ranking.topmost,
        });
        match &self.locator {
            CanonicalLocator::Index { .. } => None,
            CanonicalLocator::Ref { .. } => None,
            CanonicalLocator::Selector { .. } => None,
            CanonicalLocator::TargetText { text, .. } => Some(
                serde_json::json!({
                    "url": snapshot.url,
                    "dom_epoch": snapshot.dom_epoch,
                    "frame_id": snapshot.frame_context.frame_id,
                    "frame_lineage": snapshot.frame_lineage,
                    "total_count": snapshot.total_count,
                    "truncated": snapshot.truncated,
                    "projection_verified": snapshot.projection.verified,
                    "js_traversal_count": snapshot.projection.js_traversal_count,
                    "backend_traversal_count": snapshot.projection.backend_traversal_count,
                    "kind": "target_text",
                    "value": normalize_locator_text(text),
                    "selection": selection,
                    "ranking": ranking,
                })
                .to_string(),
            ),
            CanonicalLocator::Role { .. }
            | CanonicalLocator::Label { .. }
            | CanonicalLocator::TestId { .. } => None,
        }
    }

    pub(super) fn requires_a11y_snapshot(&self) -> bool {
        self.locator.requires_a11y_snapshot()
    }
}

pub(super) fn locator_supports_memo(locator: &CanonicalLocator) -> bool {
    matches!(locator, CanonicalLocator::TargetText { .. })
}

pub(super) fn snapshot_supports_locator_memo(snapshot: &Snapshot) -> bool {
    snapshot.projection.verified && !snapshot.truncated
}

pub(super) async fn restore_memoized_elements(
    state: &Arc<SessionState>,
    snapshot: &Snapshot,
    locator: &ParsedLocator,
) -> Result<Option<Vec<Element>>, rub_core::error::RubError> {
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

pub(super) async fn record_memoized_elements(
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

pub(super) fn rehydrate_memoized_elements(
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
