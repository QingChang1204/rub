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
use crate::router::request_args::{LocatorParseOptions, parse_canonical_locator};
use rub_core::locator::{CanonicalLocator, LocatorSelection};
use rub_core::model::{Element, Snapshot};
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

#[cfg(test)]
mod tests {
    use super::{
        ambiguous_locator_error, element_matches_text, parse_locator, resolve_elements_by_role,
        resolve_elements_by_testid,
    };
    use crate::locator_memo::LocatorMemoTarget;
    use crate::router::addressing::memo::{
        locator_supports_memo, rehydrate_memoized_elements, snapshot_supports_locator_memo,
    };
    use crate::router::addressing::semantic::normalize_locator_text;
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
    fn selector_locators_do_not_participate_in_locator_memo() {
        let locator = parse_locator(&serde_json::json!({ "selector": ".cta" }))
            .expect("selector locator should parse");
        assert!(!locator_supports_memo(&locator.locator));
        assert!(
            locator
                .memo_key(&snapshot(vec![element(0, "Save", None, Some("frame:10"))]))
                .is_none()
        );
    }

    #[test]
    fn snapshot_locator_memo_requires_verified_non_truncated_projection() {
        let mut unverified = snapshot(vec![element(0, "Save", None, Some("frame:10"))]);
        unverified.projection.verified = false;
        assert!(!snapshot_supports_locator_memo(&unverified));

        let mut truncated = snapshot(vec![element(0, "Save", None, Some("frame:10"))]);
        truncated.truncated = true;
        assert!(!snapshot_supports_locator_memo(&truncated));
    }

    #[test]
    fn target_text_memo_key_changes_when_frame_context_changes() {
        let locator = parse_locator(&serde_json::json!({ "target_text": "save draft" }))
            .expect("target_text locator should parse");
        let root_snapshot = snapshot(vec![element(0, "Save draft", None, Some("main:10"))]);
        let mut child_snapshot = root_snapshot.clone();
        child_snapshot.frame_context.frame_id = "child".to_string();
        child_snapshot.frame_lineage = vec!["main".to_string(), "child".to_string()];

        let root_key = locator
            .memo_key(&root_snapshot)
            .expect("root snapshot should support memo");
        let child_key = locator
            .memo_key(&child_snapshot)
            .expect("child snapshot should support memo");

        assert_ne!(
            root_key, child_key,
            "memo keys must change when frame continuity changes"
        );
    }

    #[test]
    fn target_text_memo_key_changes_when_projection_fence_changes() {
        let locator = parse_locator(&serde_json::json!({ "target_text": "save draft" }))
            .expect("target_text locator should parse");
        let baseline = snapshot(vec![element(0, "Save draft", None, Some("main:10"))]);
        let mut changed = baseline.clone();
        changed.projection.backend_traversal_count = 99;

        let baseline_key = locator
            .memo_key(&baseline)
            .expect("baseline snapshot should support memo");
        let changed_key = locator
            .memo_key(&changed)
            .expect("changed snapshot should support memo");

        assert_ne!(
            baseline_key, changed_key,
            "memo keys must track projection fence changes that can invalidate reuse"
        );
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
