use super::*;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    BoundingBox, Element, ElementTag, InterferenceKind, InterferenceObservation,
    InterferenceRuntimeInfo, InterferenceRuntimeStatus, OverlayState, ReadinessInfo,
    ReadinessStatus, RouteStability,
};
use std::collections::HashMap;

#[test]
fn interactability_assessment_flags_disabled_and_overlay_blockers() {
    let mut attributes = HashMap::new();
    attributes.insert("disabled".to_string(), String::new());
    let element = Element {
        index: 3,
        tag: ElementTag::Button,
        text: "Consent".to_string(),
        attributes,
        element_ref: Some("main:33".to_string()),
        target_id: None,
        bounding_box: Some(BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 90.0,
            height: 24.0,
        }),
        ax_info: None,
        listeners: None,
        depth: Some(0),
    };
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Stable,
        loading_present: false,
        skeleton_present: false,
        overlay_state: OverlayState::UserBlocking,
        document_ready_state: Some("complete".to_string()),
        blocking_signals: vec!["overlay:user_blocking".to_string()],
        degraded_reason: None,
    };
    let interference = InterferenceRuntimeInfo {
        status: InterferenceRuntimeStatus::Active,
        current_interference: Some(InterferenceObservation {
            kind: InterferenceKind::OverlayInterference,
            summary: "overlay".to_string(),
            current_url: None,
            primary_url: None,
        }),
        ..InterferenceRuntimeInfo::default()
    };

    let assessment = interactability_assessment(&element, &readiness, &interference);
    assert_eq!(assessment["likely_interactable"], false);
    assert!(
        assessment["blockers"]
            .as_array()
            .expect("blockers array")
            .iter()
            .any(|value| value == "disabled_element")
    );
    assert!(
        assessment["blockers"]
            .as_array()
            .expect("blockers array")
            .iter()
            .any(|value| value == "overlay_present")
    );
    assert!(
        assessment["blocker_details"]
            .as_array()
            .expect("blocker details array")
            .iter()
            .any(|value| value["code"] == "disabled_element")
    );
    assert!(
        assessment["blocker_details"]
            .as_array()
            .expect("blocker details array")
            .iter()
            .any(|value| value["recommended_command"] == "rub runtime readiness")
    );
}

#[test]
fn interactability_assessment_reports_ready_targets_as_likely_interactable() {
    let element = Element {
        index: 1,
        tag: ElementTag::Button,
        text: "Continue".to_string(),
        attributes: HashMap::new(),
        element_ref: Some("main:11".to_string()),
        target_id: None,
        bounding_box: Some(BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 80.0,
            height: 20.0,
        }),
        ax_info: None,
        listeners: None,
        depth: Some(0),
    };

    let assessment = interactability_assessment(
        &element,
        &ReadinessInfo::default(),
        &InterferenceRuntimeInfo::default(),
    );
    assert_eq!(assessment["likely_interactable"], true);
    assert_eq!(
        assessment["blockers"]
            .as_array()
            .expect("blockers array")
            .len(),
        0
    );
}

#[test]
fn interactability_probe_suggests_direct_click_for_overlay_recovery_target() {
    let element = Element {
        index: 3,
        tag: ElementTag::Button,
        text: "Consent".to_string(),
        attributes: HashMap::new(),
        element_ref: Some("main:33".to_string()),
        target_id: None,
        bounding_box: Some(BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 90.0,
            height: 24.0,
        }),
        ax_info: None,
        listeners: None,
        depth: Some(0),
    };
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Transitioning,
        loading_present: false,
        skeleton_present: false,
        overlay_state: OverlayState::UserBlocking,
        document_ready_state: Some("complete".to_string()),
        blocking_signals: vec![
            "overlay:user_blocking".to_string(),
            "route_transitioning".to_string(),
        ],
        degraded_reason: None,
    };

    let payload = interactability_probe_payload(
        &element,
        "snap-1",
        &serde_json::json!({ "label": "Consent", "first": true }),
        &readiness,
        &InterferenceRuntimeInfo::default(),
    );
    assert_eq!(
        payload["result"]["assessment"]["next_command_hints"][0]["command"],
        "rub click --label \"Consent\" --first"
    );
}

#[test]
fn interactability_probe_avoids_direct_click_for_disabled_targets() {
    let mut attributes = HashMap::new();
    attributes.insert("disabled".to_string(), String::new());
    let element = Element {
        index: 3,
        tag: ElementTag::Button,
        text: "Consent".to_string(),
        attributes,
        element_ref: Some("main:33".to_string()),
        target_id: None,
        bounding_box: Some(BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 90.0,
            height: 24.0,
        }),
        ax_info: None,
        listeners: None,
        depth: Some(0),
    };
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Transitioning,
        loading_present: false,
        skeleton_present: false,
        overlay_state: OverlayState::UserBlocking,
        document_ready_state: Some("complete".to_string()),
        blocking_signals: vec![
            "overlay:user_blocking".to_string(),
            "route_transitioning".to_string(),
        ],
        degraded_reason: None,
    };

    let payload = interactability_probe_payload(
        &element,
        "snap-1",
        &serde_json::json!({ "label": "Consent", "first": true }),
        &readiness,
        &InterferenceRuntimeInfo::default(),
    );
    assert_eq!(
        payload["result"]["assessment"]["next_command_hints"][0]["command"],
        "rub wait --label \"Consent\" --first --state interactable"
    );
}

#[test]
fn enriched_interactability_error_adds_assessment_and_guidance() {
    let mut attributes = HashMap::new();
    attributes.insert("disabled".to_string(), String::new());
    let element = Element {
        index: 5,
        tag: ElementTag::Button,
        text: "Consent".to_string(),
        attributes,
        element_ref: Some("main:55".to_string()),
        target_id: None,
        bounding_box: Some(BoundingBox {
            x: 1.0,
            y: 2.0,
            width: 50.0,
            height: 20.0,
        }),
        ax_info: None,
        listeners: None,
        depth: Some(0),
    };
    let readiness = ReadinessInfo {
        status: ReadinessStatus::Active,
        route_stability: RouteStability::Transitioning,
        loading_present: false,
        skeleton_present: false,
        overlay_state: OverlayState::UserBlocking,
        document_ready_state: Some("interactive".to_string()),
        blocking_signals: vec!["overlay:user_blocking".to_string()],
        degraded_reason: None,
    };
    let interference = InterferenceRuntimeInfo {
        status: InterferenceRuntimeStatus::Active,
        current_interference: Some(InterferenceObservation {
            kind: InterferenceKind::OverlayInterference,
            summary: "overlay".to_string(),
            current_url: None,
            primary_url: None,
        }),
        ..InterferenceRuntimeInfo::default()
    };

    let error = enrich_interactability_error_envelope(
        RubError::domain(ErrorCode::ElementNotInteractable, "Element is disabled").into_envelope(),
        "click",
        &element,
        "snap-1",
        &serde_json::json!({ "label": "Consent", "first": true }),
        &readiness,
        &interference,
    );

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::ElementNotInteractable);
    assert!(
        envelope
            .suggestion
            .contains("rub wait --label \"Consent\" --first --state interactable")
    );
    assert!(envelope.suggestion.contains("rub explain blockers"));
    let context = envelope.context.expect("enriched context");
    assert_eq!(context["surface"], "router.interaction.error_projection");
    assert_eq!(context["command"], "click");
    assert_eq!(context["snapshot_id"], "snap-1");
    assert_eq!(context["target"]["index"], 5);
    assert!(
        context["interactability"]["blocker_details"]
            .as_array()
            .expect("blocker details array")
            .iter()
            .any(|value| value["code"] == "disabled_element")
    );
    assert!(
        context["interactability"]["blocker_details"]
            .as_array()
            .expect("blocker details array")
            .iter()
            .any(|value| value["recommended_command"] == "rub runtime readiness")
    );
}

#[test]
fn non_interactability_error_enrichment_is_noop() {
    let element = Element {
        index: 1,
        tag: ElementTag::Button,
        text: "Continue".to_string(),
        attributes: HashMap::new(),
        element_ref: Some("main:11".to_string()),
        target_id: None,
        bounding_box: None,
        ax_info: None,
        listeners: None,
        depth: Some(0),
    };

    let error = enrich_interactability_error_envelope(
        RubError::domain(ErrorCode::ElementNotFound, "missing element").into_envelope(),
        "click",
        &element,
        "snap-1",
        &serde_json::json!({ "label": "Continue" }),
        &ReadinessInfo::default(),
        &InterferenceRuntimeInfo::default(),
    );

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::ElementNotFound);
    assert_eq!(envelope.message, "missing element");
    assert!(envelope.context.is_none());
}
