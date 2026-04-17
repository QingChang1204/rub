use super::{
    TOP_LEVEL_BOUNDING_BOX_FUNCTION, TOP_LEVEL_HIT_TEST_HELPERS, allow_global_read_fallback,
    ambiguous_read_target_error, bounding_box_center_distance, bounding_box_match_score,
    bounding_box_shape_matches, candidate_points, frame_scoped_read_target_error,
    parse_backend_node_id, parse_element_ref_frame_id, snapshot_candidate_match_rank, tag_matches,
    unverified_write_target_error,
};
use rub_core::error::ErrorCode;
use rub_core::model::{BoundingBox, Element, ElementTag};
use std::collections::HashMap;

#[test]
fn candidate_points_prioritize_center_and_insets() {
    let points = candidate_points(&[0.0, 0.0, 20.0, 0.0, 20.0, 10.0, 0.0, 10.0]);
    assert_eq!(points.len(), 5);
    assert_eq!(points[0].x, 10.0);
    assert_eq!(points[0].y, 5.0);
    assert!(points.iter().any(|p| p.x < 10.0 && p.y < 5.0));
    assert!(points.iter().any(|p| p.x > 10.0 && p.y > 5.0));
}

#[test]
fn parse_backend_node_id_reads_backend_suffix() {
    let parsed = parse_backend_node_id(Some("dom:42")).expect("backend id");
    assert_eq!(*parsed.inner(), 42);
}

#[test]
fn parse_element_ref_frame_id_reads_frame_prefix() {
    assert_eq!(
        parse_element_ref_frame_id(Some("frame-1:42")),
        Some("frame-1")
    );
    assert_eq!(parse_element_ref_frame_id(Some(":42")), None);
}

#[test]
fn global_read_fallback_is_disabled_for_frame_scoped_reads() {
    assert!(!allow_global_read_fallback(Some("frame-1")));
    assert!(allow_global_read_fallback(None));
}

#[test]
fn top_level_bounding_box_function_accumulates_frame_offsets() {
    assert!(
        TOP_LEVEL_BOUNDING_BOX_FUNCTION.contains("current.frameElement"),
        "{TOP_LEVEL_BOUNDING_BOX_FUNCTION}"
    );
    assert!(
        TOP_LEVEL_BOUNDING_BOX_FUNCTION.contains("current = current.parent"),
        "{TOP_LEVEL_BOUNDING_BOX_FUNCTION}"
    );
}

#[test]
fn top_level_hit_test_helpers_descend_through_nested_iframes() {
    assert!(
        TOP_LEVEL_HIT_TEST_HELPERS.contains("hit.contentDocument.elementFromPoint"),
        "{TOP_LEVEL_HIT_TEST_HELPERS}"
    );
    assert!(
        TOP_LEVEL_HIT_TEST_HELPERS.contains("topLevelHitPointMatches"),
        "{TOP_LEVEL_HIT_TEST_HELPERS}"
    );
}

#[test]
fn tag_matches_distinguishes_checkbox_and_radio_inputs() {
    assert!(tag_matches(ElementTag::Checkbox, "input", Some("checkbox")));
    assert!(tag_matches(ElementTag::Radio, "input", Some("radio")));
    assert!(tag_matches(ElementTag::Input, "input", Some("text")));
    assert!(!tag_matches(ElementTag::Input, "input", Some("checkbox")));
    assert!(!tag_matches(ElementTag::Checkbox, "input", Some("radio")));
}

#[test]
fn bounding_box_shape_matches_allows_small_drift() {
    let expected = BoundingBox {
        x: 10.0,
        y: 20.0,
        width: 120.0,
        height: 40.0,
    };
    let actual = BoundingBox {
        x: 100.0,
        y: 200.0,
        width: 126.0,
        height: 44.0,
    };
    assert!(bounding_box_shape_matches(expected, actual));
}

#[test]
fn bounding_box_shape_matches_rejects_shape_mismatch() {
    let expected = BoundingBox {
        x: 10.0,
        y: 20.0,
        width: 120.0,
        height: 40.0,
    };
    let actual = BoundingBox {
        x: 12.0,
        y: 18.0,
        width: 220.0,
        height: 72.0,
    };
    assert!(!bounding_box_shape_matches(expected, actual));
}

#[test]
fn bounding_box_match_score_prefers_nearest_center_for_same_shape() {
    let expected = BoundingBox {
        x: 40.0,
        y: 80.0,
        width: 120.0,
        height: 40.0,
    };
    let near = BoundingBox {
        x: 44.0,
        y: 84.0,
        width: 122.0,
        height: 42.0,
    };
    let far = BoundingBox {
        x: 240.0,
        y: 300.0,
        width: 118.0,
        height: 38.0,
    };

    let near_score = bounding_box_match_score(expected, near).expect("near score");
    let far_score = bounding_box_match_score(expected, far).expect("far score");

    assert!(near_score < far_score);
}

#[test]
fn bounding_box_center_distance_reflects_geometric_drift() {
    let expected = BoundingBox {
        x: 10.0,
        y: 20.0,
        width: 100.0,
        height: 40.0,
    };
    let shifted = BoundingBox {
        x: 40.0,
        y: 60.0,
        width: 100.0,
        height: 40.0,
    };

    assert_eq!(bounding_box_center_distance(expected, expected), 0.0);
    assert!(bounding_box_center_distance(expected, shifted) > 0.0);
}

#[test]
fn snapshot_candidate_match_rank_rejects_mismatched_attributes() {
    let expected = Element {
        index: 0,
        tag: ElementTag::Link,
        text: "Read more".to_string(),
        attributes: HashMap::from([("href".to_string(), "/detail".to_string())]),
        element_ref: Some("frame-1:10".to_string()),
        bounding_box: None,
        ax_info: None,
        listeners: None,
        depth: None,
    };
    let candidate = Element {
        attributes: HashMap::from([("href".to_string(), "/other".to_string())]),
        ..expected.clone()
    };
    assert!(snapshot_candidate_match_rank(&expected, &candidate).is_none());
}

#[test]
fn unverified_write_target_error_is_reported_as_stale_snapshot() {
    let error = unverified_write_target_error(
        &Element {
            index: 7,
            tag: ElementTag::Button,
            text: "Save".to_string(),
            attributes: HashMap::new(),
            element_ref: None,
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        },
        "snapshot element does not carry a verified backend node id",
    );

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["reason"].as_str()),
        Some("unverified_write_target")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["element_index"].as_u64()),
        Some(7)
    );
}

#[test]
fn frame_scoped_read_target_error_is_reported_as_stale_snapshot() {
    let error = frame_scoped_read_target_error(&Element {
        index: 11,
        tag: ElementTag::Input,
        text: String::new(),
        attributes: HashMap::new(),
        element_ref: Some("frame-1:42".to_string()),
        bounding_box: None,
        ax_info: None,
        listeners: None,
        depth: None,
    });

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["authority_state"].as_str()),
        Some("frame_scoped_read_target_stale")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["element_index"].as_u64()),
        Some(11)
    );
}

#[test]
fn ambiguous_read_target_error_is_reported_as_stale_snapshot() {
    let error = ambiguous_read_target_error(
        &Element {
            index: 13,
            tag: ElementTag::Link,
            text: "Read".to_string(),
            attributes: HashMap::new(),
            element_ref: Some("frame-1:77".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        },
        "global_read_fallback_ambiguous",
        2,
    );

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["authority_state"].as_str()),
        Some("global_read_fallback_ambiguous")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx["candidate_count"].as_u64()),
        Some(2)
    );
}
