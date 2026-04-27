use super::{
    ambiguous_locator_error, apply_disambiguation, apply_pre_hit_test_ranking,
    element_matches_text, parse_locator, resolve_elements_against_snapshot,
    resolve_elements_by_role, resolve_elements_by_testid,
};
use crate::locator_memo::LocatorMemoTarget;
use crate::router::addressing::memo::{
    locator_supports_memo, rehydrate_memoized_elements, snapshot_supports_locator_memo,
};
use crate::router::addressing::semantic::normalize_locator_text;
use rub_core::error::ErrorCode;
use rub_core::locator::{CanonicalLocator, LocatorSelection};
use rub_core::model::{AXInfo, BoundingBox, Element, ElementTag};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

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
    let context = envelope.context.expect("ambiguous locator context");
    assert_eq!(context["ordering_policy"], "snapshot_index");
    assert_eq!(context["candidates"][0]["label"], "Primary");
    assert_eq!(
        context["candidates"][0]["ranking_hints"]["unique_anchors"]["label"],
        "Primary"
    );
}

#[test]
fn ambiguous_locator_error_reports_topmost_ordering_policy() {
    let err = ambiguous_locator_error(
        "click",
        &serde_json::json!({
            "label": "Consent",
            "topmost": true,
        }),
        &[
            element(1, "Consent", Some("Consent"), Some("main:1")),
            element(2, "Consent", Some("Consent"), Some("main:2")),
        ],
    );
    let envelope = err.into_envelope();
    let context = envelope.context.expect("ambiguous locator context");
    assert_eq!(
        context["ordering_policy"],
        "topmost_hit_test_then_snapshot_index"
    );
}

#[test]
fn ambiguous_locator_error_suggests_visible_when_one_candidate_is_visible() {
    let mut visible = element(1, "Username", Some("Username"), Some("main:1"));
    visible.bounding_box = Some(rub_core::model::BoundingBox {
        x: 10.0,
        y: 10.0,
        width: 80.0,
        height: 24.0,
    });
    let hidden = element(2, "username", Some("username"), Some("main:2"));

    let err = ambiguous_locator_error(
        "type",
        &serde_json::json!({ "label": "Username" }),
        &[visible, hidden],
    );
    let envelope = err.into_envelope();
    assert!(
        envelope.suggestion.contains("--visible"),
        "{}",
        envelope.suggestion
    );
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
fn topmost_target_text_locators_do_not_participate_in_locator_memo() {
    let locator = parse_locator(&serde_json::json!({
        "target_text": "save draft",
        "topmost": true
    }))
    .expect("topmost target_text locator should parse");
    assert!(
        locator
            .memo_key(&snapshot(vec![element(
                0,
                "Save draft",
                None,
                Some("frame:10")
            )]))
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
fn target_text_memo_key_changes_when_scroll_position_changes() {
    let locator = parse_locator(&serde_json::json!({ "target_text": "save draft" }))
        .expect("target_text locator should parse");
    let baseline = snapshot(vec![element(0, "Save draft", None, Some("main:10"))]);
    let mut changed = baseline.clone();
    changed.scroll.y = 600.0;
    changed.scroll.at_bottom = true;

    let baseline_key = locator
        .memo_key(&baseline)
        .expect("baseline snapshot should support memo");
    let changed_key = locator
        .memo_key(&changed)
        .expect("changed snapshot should support memo");

    assert_ne!(
        baseline_key, changed_key,
        "memo keys must change when scroll-driven drift changes candidate authority"
    );
}

#[test]
fn parse_locator_accepts_ref_alias() {
    let locator =
        parse_locator(&serde_json::json!({ "ref": "child:42" })).expect("ref locator should parse");
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

#[test]
fn visible_ranking_filters_out_candidates_without_visible_bbox() {
    let locator = parse_locator(&serde_json::json!({
        "label": "Continue",
        "visible": true,
        "first": true
    }))
    .expect("visible locator should parse");
    let mut hidden = element(0, "Continue", Some("Continue"), Some("main:10"));
    hidden.bounding_box = None;
    let mut visible = element(1, "Continue", Some("Continue"), Some("main:11"));
    visible.bounding_box = Some(rub_core::model::BoundingBox {
        x: 10.0,
        y: 20.0,
        width: 80.0,
        height: 24.0,
    });

    let selected = apply_disambiguation(
        vec![hidden, visible.clone()],
        &locator,
        &serde_json::json!({
            "label": "Continue",
            "visible": true,
            "first": true
        }),
    )
    .expect("visible disambiguation should succeed");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].index, visible.index);
}

#[test]
fn prefer_enabled_ranking_moves_disabled_candidates_after_enabled_ones() {
    let locator = parse_locator(&serde_json::json!({
        "label": "Consent",
        "prefer_enabled": true,
        "first": true
    }))
    .expect("prefer-enabled locator should parse");
    let mut disabled = element(0, "Consent", Some("Consent"), Some("main:10"));
    disabled
        .attributes
        .insert("disabled".to_string(), String::new());
    let enabled = element(1, "Consent", Some("Consent"), Some("main:11"));

    let selected = apply_disambiguation(
        vec![disabled, enabled.clone()],
        &locator,
        &serde_json::json!({
            "label": "Consent",
            "prefer_enabled": true,
            "first": true
        }),
    )
    .expect("prefer-enabled disambiguation should succeed");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].index, enabled.index);
}

#[test]
fn prefer_enabled_ranking_moves_readonly_text_targets_after_writable_ones() {
    let locator = parse_locator(&serde_json::json!({
        "label": "Email",
        "prefer_enabled": true,
        "first": true
    }))
    .expect("prefer-enabled locator should parse");
    let mut readonly = element(0, "", Some("Email"), Some("main:10"));
    readonly.tag = ElementTag::Input;
    readonly
        .attributes
        .insert("readonly".to_string(), String::new());
    let mut writable = element(1, "", Some("Email"), Some("main:11"));
    writable.tag = ElementTag::Input;

    let selected = apply_disambiguation(
        vec![readonly, writable.clone()],
        &locator,
        &serde_json::json!({
            "label": "Email",
            "prefer_enabled": true,
            "first": true
        }),
    )
    .expect("prefer-enabled disambiguation should succeed");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].index, writable.index);
}

#[test]
fn memoized_candidates_still_flow_through_prefer_enabled_disambiguation() {
    let locator = parse_locator(&serde_json::json!({
        "target_text": "Email",
        "prefer_enabled": true,
        "first": true
    }))
    .expect("prefer-enabled target_text locator should parse");
    let mut readonly = element(0, "", Some("Email"), Some("main:10"));
    readonly.tag = ElementTag::Input;
    readonly
        .attributes
        .insert("readonly".to_string(), String::new());
    let mut writable = element(1, "", Some("Email"), Some("main:11"));
    writable.tag = ElementTag::Input;
    let snapshot = snapshot(vec![readonly, writable.clone()]);

    let rehydrated = rehydrate_memoized_elements(
        &snapshot,
        &locator.locator,
        &[
            LocatorMemoTarget::ElementRef("main:10".to_string()),
            LocatorMemoTarget::ElementRef("main:11".to_string()),
        ],
    )
    .expect("memoized candidates should rehydrate");

    let selected = apply_disambiguation(
        rehydrated,
        &locator,
        &serde_json::json!({
            "target_text": "Email",
            "prefer_enabled": true,
            "first": true
        }),
    )
    .expect("disambiguation should still run after memo rehydration");

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].index, writable.index);
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

fn test_router() -> crate::router::DaemonRouter {
    let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        },
    ));
    let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        rub_cdp::humanize::HumanizeConfig {
            enabled: false,
            speed: rub_cdp::humanize::HumanizeSpeed::Normal,
        },
    ));
    crate::router::DaemonRouter::new(adapter)
}

fn element(index: u32, text: &str, aria_label: Option<&str>, element_ref: Option<&str>) -> Element {
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
        target_id: None,
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

#[tokio::test]
async fn index_locator_resolves_by_element_index_not_scoped_vector_position() {
    let scoped = snapshot(vec![
        element(3, "Third", None, Some("main:3")),
        element(9, "Ninth", None, Some("main:9")),
    ]);
    let resolved = resolve_elements_against_snapshot(
        &test_router(),
        &scoped,
        &serde_json::json!({ "index": 9 }),
        "inspect text",
    )
    .await
    .expect("index lookup should honor element.index");

    assert_eq!(resolved.elements.len(), 1);
    assert_eq!(resolved.elements[0].index, 9);
    assert_eq!(resolved.elements[0].text, "Ninth");
}

#[tokio::test]
async fn ref_locator_fails_closed_when_snapshot_ref_authority_is_unverified() {
    let mut scoped = snapshot(vec![element(9, "Ninth", None, None)]);
    scoped.projection.verified = false;
    scoped.projection.warning = Some("projection mismatch".to_string());

    let error = resolve_elements_against_snapshot(
        &test_router(),
        &scoped,
        &serde_json::json!({ "element_ref": "main:9" }),
        "inspect text",
    )
    .await
    .expect_err("ref lookup should fail closed when snapshot ref authority is unavailable");

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["reason"].as_str()),
        Some("snapshot_ref_authority_unavailable")
    );
}

#[test]
fn visible_ranking_applies_before_live_hit_test() {
    let locator = parse_locator(&serde_json::json!({
        "label": "Continue",
        "visible": true,
        "topmost": true,
    }))
    .expect("visible+topmost locator should parse");
    let hidden = element(0, "Continue", Some("Continue"), Some("main:10"));
    let mut visible = element(1, "Continue", Some("Continue"), Some("main:11"));
    visible.bounding_box = Some(BoundingBox {
        x: 10.0,
        y: 20.0,
        width: 80.0,
        height: 24.0,
    });

    let ranked = apply_pre_hit_test_ranking(&locator, vec![hidden, visible.clone()]);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].index, visible.index);
}
