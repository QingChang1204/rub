use super::LocatorRankingPolicy;
use super::projection::selection_context;
use super::semantic::{normalize_locator_text, validate_memoized_elements};
use crate::locator_memo::LocatorMemoTarget;
use crate::session::SessionState;
use rub_core::locator::CanonicalLocator;
use rub_core::model::{Element, Snapshot};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

thread_local! {
    static MEMO_ELEMENT_REF_LOOKUPS: Cell<u64> = const { Cell::new(0) };
    static MEMO_ELEMENT_REF_INDEX_BUILD_STEPS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct MemoRehydrateMetrics {
    pub(super) element_ref_lookups: u64,
    pub(super) element_ref_index_build_steps: u64,
}

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
                    "scroll": snapshot.scroll,
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

    let element_ref_index = targets
        .iter()
        .any(|target| matches!(target, LocatorMemoTarget::ElementRef(_)))
        .then(|| build_element_ref_index(snapshot));
    let mut seen = HashSet::new();
    let mut elements = Vec::with_capacity(targets.len());
    for target in targets {
        let element = match target {
            LocatorMemoTarget::ElementRef(element_ref) => {
                find_element_by_ref(element_ref_index.as_ref()?, element_ref.as_str())
            }
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

fn build_element_ref_index(snapshot: &Snapshot) -> HashMap<&str, &Element> {
    MEMO_ELEMENT_REF_INDEX_BUILD_STEPS.with(|count| {
        count.set(
            count
                .get()
                .saturating_add(snapshot.elements.len().try_into().unwrap_or(u64::MAX)),
        )
    });
    snapshot
        .elements
        .iter()
        .filter_map(|element| element.element_ref.as_deref().map(|key| (key, element)))
        .collect()
}

fn find_element_by_ref(
    element_ref_index: &HashMap<&str, &Element>,
    element_ref: &str,
) -> Option<Element> {
    MEMO_ELEMENT_REF_LOOKUPS.with(|count| count.set(count.get().saturating_add(1)));
    element_ref_index.get(element_ref).cloned().cloned()
}

#[cfg(test)]
fn memo_rehydrate_metrics_snapshot() -> MemoRehydrateMetrics {
    MemoRehydrateMetrics {
        element_ref_lookups: MEMO_ELEMENT_REF_LOOKUPS.with(Cell::get),
        element_ref_index_build_steps: MEMO_ELEMENT_REF_INDEX_BUILD_STEPS.with(Cell::get),
    }
}

#[cfg(test)]
fn reset_memo_rehydrate_metrics() {
    MEMO_ELEMENT_REF_LOOKUPS.with(|count| count.set(0));
    MEMO_ELEMENT_REF_INDEX_BUILD_STEPS.with(|count| count.set(0));
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

#[cfg(test)]
mod tests {
    use super::{MemoRehydrateMetrics, rehydrate_memoized_elements, reset_memo_rehydrate_metrics};
    use crate::locator_memo::LocatorMemoTarget;
    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot, SnapshotProjection,
    };
    use std::collections::HashMap;

    #[test]
    fn rehydrate_element_ref_metrics_capture_scan_baseline() {
        reset_memo_rehydrate_metrics();
        let snapshot = sample_snapshot();
        let elements = rehydrate_memoized_elements(
            &snapshot,
            &CanonicalLocator::TargetText {
                text: "Save".to_string(),
                selection: None,
            },
            &[
                LocatorMemoTarget::ElementRef("frame:node-3".to_string()),
                LocatorMemoTarget::ElementRef("frame:node-2".to_string()),
            ],
        )
        .expect("rehydrate should succeed");

        assert_eq!(elements.len(), 2);
        assert_eq!(
            super::memo_rehydrate_metrics_snapshot(),
            MemoRehydrateMetrics {
                element_ref_lookups: 2,
                element_ref_index_build_steps: 3,
            }
        );
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            snapshot_id: "snap-1".to_string(),
            dom_epoch: 1,
            frame_context: FrameContextInfo {
                frame_id: "frame-1".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            frame_lineage: vec!["frame-1".to_string()],
            url: "https://example.test".to_string(),
            title: "Example".to_string(),
            elements: vec![
                sample_element(0, "frame:node-1", "Cancel"),
                sample_element(1, "frame:node-2", "Save"),
                sample_element(2, "frame:node-3", "Save"),
            ],
            total_count: 3,
            truncated: false,
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-04-14T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 3,
                backend_traversal_count: 3,
                resolved_ref_count: 3,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: None,
        }
    }

    fn sample_element(index: u32, element_ref: &str, text: &str) -> Element {
        Element {
            index,
            tag: ElementTag::Button,
            text: text.to_string(),
            attributes: HashMap::new(),
            element_ref: Some(element_ref.to_string()),
            target_id: None,
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: Some(0),
        }
    }
}
