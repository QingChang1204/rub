use super::{
    parse_wait_condition, post_wait_timeout_error, wait_kind_uses_selected_frame,
    wait_outcome_summary, wait_probe_context, wait_subject_state,
};
use rub_core::error::ErrorCode;
use rub_core::locator::CanonicalLocator;
use rub_core::model::{WaitCondition, WaitKind, WaitState};
use rub_core::{DEFAULT_WAIT_AFTER_TIMEOUT_MS, DEFAULT_WAIT_TIMEOUT_MS};

#[test]
fn explicit_wait_uses_long_default_timeout() {
    let parsed = parse_wait_condition(
        &serde_json::json!({
            "selector": "#ready",
        }),
        DEFAULT_WAIT_TIMEOUT_MS,
    )
    .expect("wait condition should parse");

    assert_eq!(parsed.condition.timeout_ms, DEFAULT_WAIT_TIMEOUT_MS);
}

#[test]
fn post_wait_uses_bounded_default_timeout() {
    let parsed = parse_wait_condition(
        &serde_json::json!({
            "selector": "#ready",
        }),
        DEFAULT_WAIT_AFTER_TIMEOUT_MS,
    )
    .expect("post wait condition should parse");

    assert_eq!(parsed.condition.timeout_ms, DEFAULT_WAIT_AFTER_TIMEOUT_MS);
}

#[test]
fn post_wait_timeout_error_uses_bounded_default_in_context() {
    let err = post_wait_timeout_error(
        "open",
        &serde_json::json!({ "selector": "#ready" }),
        None,
        None,
    );
    let envelope = err.into_envelope();
    assert_eq!(envelope.code, ErrorCode::WaitTimeout);
    let context = envelope.context.expect("context");
    assert_eq!(
        context["transaction_timeout_ms"].as_u64(),
        Some(DEFAULT_WAIT_AFTER_TIMEOUT_MS)
    );
    assert_eq!(
        context["requested_wait_after_timeout_ms"].as_u64(),
        Some(DEFAULT_WAIT_AFTER_TIMEOUT_MS)
    );
}

#[test]
fn post_wait_timeout_error_records_effective_budget_when_clamped() {
    let err = post_wait_timeout_error(
        "click",
        &serde_json::json!({ "selector": "#save", "timeout_ms": 1_500 }),
        Some(900),
        Some(250),
    );
    let envelope = err.into_envelope();
    assert_eq!(envelope.code, ErrorCode::WaitTimeout);
    let context = envelope.context.expect("context");
    assert_eq!(context["transaction_timeout_ms"], serde_json::json!(900));
    assert_eq!(
        context["effective_wait_after_timeout_ms"],
        serde_json::json!(250)
    );
    assert_eq!(
        context["requested_wait_after_timeout_ms"],
        serde_json::json!(1_500)
    );
}

#[test]
fn url_contains_wait_parses_as_page_level_probe() {
    let parsed = parse_wait_condition(
        &serde_json::json!({
            "url_contains": "/activate",
        }),
        DEFAULT_WAIT_TIMEOUT_MS,
    )
    .expect("url wait should parse");

    match parsed.condition.kind {
        WaitKind::UrlContains { value } => assert_eq!(value, "/activate"),
        other => panic!("expected UrlContains wait kind, got {other:?}"),
    }
    assert_eq!(parsed.kind_name, "url_contains");
    assert_eq!(parsed.probe_value, "/activate");
}

#[test]
fn title_contains_wait_rejects_locator_selection_flags() {
    let error = parse_wait_condition(
        &serde_json::json!({
            "title_contains": "Confirm your account",
            "first": true,
        }),
        DEFAULT_WAIT_TIMEOUT_MS,
    )
    .expect_err("page-level wait should reject locator selection flags")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("page-level waits"), "{error}");
}

#[test]
fn wait_probe_rejects_unknown_fields_without_prefiltering_them_away() {
    let error = parse_wait_condition(
        &serde_json::json!({
            "selector": "#ready",
            "mystery": true,
        }),
        DEFAULT_WAIT_TIMEOUT_MS,
    )
    .expect_err("unknown wait fields must fail closed")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::InvalidInput);
}

#[test]
fn interactable_wait_state_parses_for_locator_waits() {
    let parsed = parse_wait_condition(
        &serde_json::json!({
            "label": "Compose",
            "state": "interactable",
        }),
        DEFAULT_WAIT_TIMEOUT_MS,
    )
    .expect("interactable wait should parse");

    match parsed.condition.kind {
        WaitKind::Locator { state, .. } => {
            assert_eq!(state, rub_core::model::WaitState::Interactable)
        }
        other => panic!("expected locator wait kind, got {other:?}"),
    }
}

#[test]
fn description_contains_wait_parses_for_locator_probe() {
    let parsed = parse_wait_condition(
        &serde_json::json!({
            "label": "Email",
            "description_contains": "We will email you to confirm",
        }),
        DEFAULT_WAIT_TIMEOUT_MS,
    )
    .expect("description wait should parse");

    match parsed.condition.kind {
        WaitKind::LocatorDescriptionContains { locator, value } => {
            assert_eq!(
                locator,
                CanonicalLocator::Label {
                    label: "Email".to_string(),
                    selection: None,
                }
            );
            assert_eq!(value, "We will email you to confirm");
        }
        other => panic!("expected description wait kind, got {other:?}"),
    }
    assert_eq!(parsed.kind_name, "description_contains");
}

#[test]
fn wait_outcome_summary_marks_page_context_waits_as_confirmed_transition() {
    let summary = wait_outcome_summary(
        "url_contains",
        &WaitCondition {
            kind: WaitKind::UrlContains {
                value: "/activate".to_string(),
            },
            timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
            frame_id: None,
        },
    )
    .expect("page context waits should produce an outcome summary");
    assert_eq!(summary["class"], "confirmed_context_transition");
    assert_eq!(summary["authoritative"], true);

    assert!(
        wait_outcome_summary(
            "selector",
            &WaitCondition {
                kind: WaitKind::Locator {
                    locator: CanonicalLocator::Selector {
                        css: "#ready".to_string(),
                        selection: None,
                    },
                    state: WaitState::Visible,
                },
                timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
                frame_id: None,
            },
        )
        .is_none()
    );
}

#[test]
fn wait_outcome_summary_marks_interactable_waits_as_current_runtime_ready() {
    let summary = wait_outcome_summary(
        "label",
        &WaitCondition {
            kind: WaitKind::Locator {
                locator: CanonicalLocator::Label {
                    label: "Compose".to_string(),
                    selection: None,
                },
                state: WaitState::Interactable,
            },
            timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
            frame_id: None,
        },
    )
    .expect("interactable waits should produce an outcome summary");
    assert_eq!(summary["class"], "confirmed_interactable_target");
    assert_eq!(summary["authoritative"], true);
}

#[test]
fn wait_outcome_summary_marks_description_waits_as_confirmed_target_description() {
    let summary = wait_outcome_summary(
        "description_contains",
        &WaitCondition {
            kind: WaitKind::LocatorDescriptionContains {
                locator: CanonicalLocator::Label {
                    label: "Email".to_string(),
                    selection: None,
                },
                value: "We will email you to confirm".to_string(),
            },
            timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
            frame_id: None,
        },
    )
    .expect("description waits should produce an outcome summary");
    assert_eq!(summary["class"], "confirmed_target_description");
    assert_eq!(summary["authoritative"], true);
}

#[test]
fn wait_probe_context_includes_locator_wait_state() {
    let context = wait_probe_context(&serde_json::json!({
        "selector": "#composer",
        "state": "interactable",
    }));
    assert_eq!(context["state"], "interactable");
}

#[test]
fn wait_probe_context_projects_description_wait_probe() {
    let context = wait_probe_context(&serde_json::json!({
        "label": "Email",
        "description_contains": "We will email you to confirm",
    }));
    assert_eq!(context["kind"], "description_contains");
    assert_eq!(context["target_kind"], "label");
    assert_eq!(context["target_value"], "Email");
}

#[test]
fn wait_subject_state_projects_locator_state() {
    let subject_state = wait_subject_state(&WaitCondition {
        kind: WaitKind::Locator {
            locator: CanonicalLocator::Selector {
                css: "#composer".to_string(),
                selection: None,
            },
            state: WaitState::Interactable,
        },
        timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
        frame_id: None,
    });
    assert_eq!(subject_state, Some("interactable"));
}

#[test]
fn page_level_wait_kinds_do_not_inherit_selected_frame_authority() {
    assert!(!wait_kind_uses_selected_frame(&WaitKind::Text {
        text: "Ready".to_string(),
    }));
    assert!(!wait_kind_uses_selected_frame(&WaitKind::UrlContains {
        value: "/activate".to_string(),
    }));
    assert!(!wait_kind_uses_selected_frame(&WaitKind::TitleContains {
        value: "Confirm your account".to_string(),
    }));
}

#[test]
fn locator_wait_kinds_continue_to_use_selected_frame_authority() {
    assert!(wait_kind_uses_selected_frame(&WaitKind::Locator {
        locator: CanonicalLocator::Selector {
            css: "#ready".to_string(),
            selection: None,
        },
        state: WaitState::Visible,
    }));
    assert!(wait_kind_uses_selected_frame(
        &WaitKind::LocatorDescriptionContains {
            locator: CanonicalLocator::Label {
                label: "Email".to_string(),
                selection: None,
            },
            value: "We will email you".to_string(),
        }
    ));
}
