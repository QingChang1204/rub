use super::{
    ExtractedElementsPayload, highlight_overlay_js, normalize_snapshot_limit,
    scripts::extract_elements_script,
};
use rub_core::model::{Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot};
use rub_core::port::DEFAULT_SNAPSHOT_LIMIT;
use std::collections::HashMap;

/// Contract test: the JSON payload emitted by EXTRACT_ELEMENTS_JS_TEMPLATE must
/// deserialize cleanly into ExtractedElementsPayload with all three top-level fields:
/// `elements`, `title`, and `scroll`.
///
/// This test does NOT require a browser — it exercises the serde contract directly.
/// If the JS script's output shape or the Rust struct diverge, this test will catch it.
#[test]
fn extracted_elements_payload_deserializes_all_three_fields() {
    let json = serde_json::json!({
        "elements": [
            {
                "index": 0,
                "dom_index": 0,
                "tag": "BUTTON",
                "text": "Click me",
                "attributes": {},
                "bounding_box": null,
                "listeners": null,
                "depth": 0
            }
        ],
        "traversal_count": 1,
        "title": "Test Page Title",
        "scroll": {
            "x": 0.0,
            "y": 42.5,
            "at_bottom": false
        }
    });

    let payload: ExtractedElementsPayload = serde_json::from_value(json)
        .expect("ExtractedElementsPayload must deserialize the unified JS payload shape");

    assert_eq!(payload.elements.len(), 1, "elements must be present");
    assert_eq!(payload.traversal_count, 1);
    assert_eq!(
        payload.title, "Test Page Title",
        "title must be extracted from the same JS context as elements"
    );
    let scroll = payload
        .scroll
        .expect("scroll must be present when JS returns it");
    assert_eq!(scroll.y, 42.5, "scroll.y must round-trip through serde");
    assert!(!scroll.at_bottom);
}

/// Backward-compatibility contract: payloads without `title` or `scroll` (e.g., from
/// an older rub-cdp or a mocked frame context) must still deserialize successfully.
/// The #[serde(default)] attributes must provide safe fallback values.
#[test]
fn extracted_elements_payload_handles_missing_optional_fields_via_serde_default() {
    let json_without_title_scroll = serde_json::json!({
        "elements": [],
        "traversal_count": 0
        // no "title", no "scroll" — simulates pre-merge payload shape
    });

    let payload: ExtractedElementsPayload = serde_json::from_value(json_without_title_scroll)
        .expect("ExtractedElementsPayload must not fail when title/scroll are absent");

    assert_eq!(
        payload.title, "",
        "title must default to empty string when absent (serde default)"
    );
    assert!(
        payload.scroll.is_none(),
        "scroll must default to None when absent (serde default)"
    );
}

#[test]
fn extract_script_tracks_dom_index_without_dom_mutation() {
    let extract_script = extract_elements_script(true);
    assert!(extract_script.contains("listeners.length > 0"));
    assert!(extract_script.contains("dom_index"));
    assert!(extract_script.contains("traversal_count"));
    assert!(!extract_script.contains("setAttribute"));
}

#[test]
fn plain_snapshot_resolution_uses_same_dom_index_model() {
    let extract_script = extract_elements_script(false);
    assert!(extract_script.contains("const includeListeners = false"));
    assert!(extract_script.contains("domIndex++"));
}

#[test]
fn extract_script_preserves_boolean_disabled_attribute() {
    let extract_script = extract_elements_script(false);
    assert!(extract_script.contains("hasAttribute('disabled')"));
    assert!(extract_script.contains("attrs.disabled = ''"));
}

#[test]
fn extract_script_marks_contenteditable_targets_as_interactive() {
    let extract_script = extract_elements_script(false);
    assert!(extract_script.contains("if (el.isContentEditable) return true;"));
    assert!(extract_script.contains("attrs.contenteditable = 'true';"));
}

#[test]
fn extract_script_preserves_readonly_attribute_for_write_diagnostics() {
    let extract_script = extract_elements_script(false);
    assert!(extract_script.contains("hasAttribute('readonly')"));
    assert!(extract_script.contains("attrs.readonly = '';"));
}

#[test]
fn highlight_overlay_script_avoids_inner_html_assignment() {
    let snapshot = Snapshot {
        snapshot_id: "snap-1".to_string(),
        dom_epoch: 1,
        frame_context: FrameContextInfo {
            frame_id: "main".to_string(),
            name: None,
            parent_frame_id: None,
            target_id: Some("target-1".to_string()),
            url: Some("https://example.com".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        },
        frame_lineage: vec!["main".to_string()],
        url: "https://example.com".to_string(),
        title: "Example".to_string(),
        elements: vec![Element {
            index: 0,
            tag: ElementTag::Button,
            text: "Example".to_string(),
            attributes: HashMap::new(),
            element_ref: None,
            bounding_box: Some(rub_core::model::BoundingBox {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0,
            }),
            ax_info: None,
            listeners: Some(Vec::new()),
            depth: Some(0),
        }],
        total_count: 1,
        truncated: false,
        scroll: ScrollPosition {
            x: 0.0,
            y: 0.0,
            at_bottom: false,
        },
        timestamp: "2026-03-29T00:00:00Z".to_string(),
        projection: rub_core::model::SnapshotProjection {
            verified: true,
            js_traversal_count: 1,
            backend_traversal_count: 1,
            resolved_ref_count: 1,
            warning: None,
        },
        viewport_filtered: None,
        viewport_count: None,
    };

    let script = highlight_overlay_js(&snapshot).unwrap();
    assert!(!script.contains("innerHTML"));
    assert!(script.contains("while (shadow.firstChild)"));
    assert!(script.contains("document.createElement('div')"));
}

#[test]
fn snapshot_limit_defaults_to_documented_default() {
    assert_eq!(
        normalize_snapshot_limit(None),
        DEFAULT_SNAPSHOT_LIMIT as usize
    );
}

#[test]
fn snapshot_limit_zero_remains_unbounded() {
    assert_eq!(normalize_snapshot_limit(Some(0)), usize::MAX);
}

#[test]
fn snapshot_limit_preserves_explicit_positive_value() {
    assert_eq!(normalize_snapshot_limit(Some(17)), 17);
}
