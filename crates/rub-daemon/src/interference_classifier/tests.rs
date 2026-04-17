use super::{InterferenceBaseline, classify};
use rub_core::model::{
    HumanVerificationHandoffInfo, HumanVerificationHandoffStatus, InterferenceKind,
    InterferenceMode, InterferenceObservation, InterferenceRuntimeInfo, InterferenceRuntimeStatus,
    NetworkFailureEvent, OverlayState, ReadinessInfo, ReadinessStatus, RequestSummaryEvent,
    RuntimeObservatoryInfo, RuntimeObservatoryStatus, TabInfo,
};

fn active_tab(target_id: &str, url: &str, title: &str) -> TabInfo {
    TabInfo {
        index: 0,
        target_id: target_id.to_string(),
        url: url.to_string(),
        title: title.to_string(),
        active: true,
    }
}

#[test]
fn classifier_stays_inactive_for_clean_primary_context() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline::default(),
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified.projection.status,
        rub_core::model::InterferenceRuntimeStatus::Inactive
    );
    assert!(classified.projection.current_interference.is_none());
    assert!(
        classified.baseline.primary_url.is_none(),
        "classifier must not implicitly author a new baseline"
    );
}

#[test]
fn classifier_preserves_existing_primary_context_when_clean() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified.projection.status,
        InterferenceRuntimeStatus::Inactive
    );
    assert_eq!(
        classified.baseline.primary_target_id.as_deref(),
        Some("target-1")
    );
    assert_eq!(
        classified.baseline.primary_url.as_deref(),
        Some("https://app.example.com/home")
    );
}

#[test]
fn classifier_detects_popup_hijack_when_active_target_drifts() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-2",
            "https://ads.example.net/popup",
            "Promo",
        )],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified
            .projection
            .current_interference
            .as_ref()
            .map(|obs| obs.kind),
        Some(InterferenceKind::PopupHijack)
    );
}

#[test]
fn classifier_detects_overlay_interference_from_readiness() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline::default(),
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo {
            status: ReadinessStatus::Active,
            overlay_state: OverlayState::Error,
            blocking_signals: vec!["overlay:error".to_string()],
            ..ReadinessInfo::default()
        },
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified
            .projection
            .current_interference
            .as_ref()
            .map(|obs| obs.kind),
        Some(InterferenceKind::OverlayInterference)
    );
}

#[test]
fn classifier_detects_interstitial_navigation_from_url_pattern() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-1",
            "https://ads.example.net/#vignette",
            "Interstitial",
        )],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified
            .projection
            .current_interference
            .as_ref()
            .map(|obs| obs.kind),
        Some(InterferenceKind::InterstitialNavigation)
    );
}

#[test]
fn classifier_detects_third_party_noise_from_repeated_failure_windows() {
    let observatory = RuntimeObservatoryInfo {
        status: RuntimeObservatoryStatus::Active,
        recent_network_failures: vec![
            NetworkFailureEvent {
                request_id: "fail-1".to_string(),
                url: "https://tracker-1.example.net/pixel".to_string(),
                method: "GET".to_string(),
                error_text: "net::ERR_BLOCKED_BY_CLIENT".to_string(),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            NetworkFailureEvent {
                request_id: "fail-2".to_string(),
                url: "https://tracker-2.example.net/pixel".to_string(),
                method: "GET".to_string(),
                error_text: "net::ERR_BLOCKED_BY_CLIENT".to_string(),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            NetworkFailureEvent {
                request_id: "fail-3".to_string(),
                url: "https://tracker-3.example.net/pixel".to_string(),
                method: "GET".to_string(),
                error_text: "net::ERR_BLOCKED_BY_CLIENT".to_string(),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
        ],
        recent_requests: vec![
            RequestSummaryEvent {
                request_id: "req-1".to_string(),
                url: "https://tracker-1.example.net/pixel".to_string(),
                method: "GET".to_string(),
                status: Some(200),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            RequestSummaryEvent {
                request_id: "req-2".to_string(),
                url: "https://tracker-2.example.net/pixel".to_string(),
                method: "GET".to_string(),
                status: Some(200),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            RequestSummaryEvent {
                request_id: "req-3".to_string(),
                url: "https://tracker-3.example.net/pixel".to_string(),
                method: "GET".to_string(),
                status: Some(200),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
        ],
        ..RuntimeObservatoryInfo::default()
    };

    let classified = classify(
        &InterferenceRuntimeInfo {
            mode: InterferenceMode::Normal,
            last_interference: Some(InterferenceObservation {
                kind: InterferenceKind::ThirdPartyNoise,
                summary: "prior".to_string(),
                current_url: Some("https://app.example.com/home".to_string()),
                primary_url: Some("https://app.example.com/home".to_string()),
            }),
            ..InterferenceRuntimeInfo::default()
        },
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &observatory,
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified
            .projection
            .current_interference
            .as_ref()
            .map(|obs| obs.kind),
        Some(InterferenceKind::ThirdPartyNoise)
    );
}

#[test]
fn classifier_ignores_single_failure_window_of_third_party_noise() {
    let observatory = RuntimeObservatoryInfo {
        status: RuntimeObservatoryStatus::Active,
        recent_network_failures: vec![
            NetworkFailureEvent {
                request_id: "fail-1".to_string(),
                url: "https://tracker-1.example.net/pixel".to_string(),
                method: "GET".to_string(),
                error_text: "net::ERR_BLOCKED_BY_CLIENT".to_string(),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            NetworkFailureEvent {
                request_id: "fail-2".to_string(),
                url: "https://tracker-2.example.net/pixel".to_string(),
                method: "GET".to_string(),
                error_text: "net::ERR_BLOCKED_BY_CLIENT".to_string(),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            NetworkFailureEvent {
                request_id: "fail-3".to_string(),
                url: "https://tracker-3.example.net/pixel".to_string(),
                method: "GET".to_string(),
                error_text: "net::ERR_BLOCKED_BY_CLIENT".to_string(),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
        ],
        recent_requests: vec![
            RequestSummaryEvent {
                request_id: "req-1".to_string(),
                url: "https://tracker-1.example.net/pixel".to_string(),
                method: "GET".to_string(),
                status: Some(200),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            RequestSummaryEvent {
                request_id: "req-2".to_string(),
                url: "https://tracker-2.example.net/pixel".to_string(),
                method: "GET".to_string(),
                status: Some(200),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
            RequestSummaryEvent {
                request_id: "req-3".to_string(),
                url: "https://tracker-3.example.net/pixel".to_string(),
                method: "GET".to_string(),
                status: Some(200),
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
            },
        ],
        ..RuntimeObservatoryInfo::default()
    };

    let classified = classify(
        &InterferenceRuntimeInfo {
            mode: InterferenceMode::Normal,
            ..InterferenceRuntimeInfo::default()
        },
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &observatory,
        &ReadinessInfo {
            status: ReadinessStatus::Active,
            document_ready_state: Some("complete".to_string()),
            ..ReadinessInfo::default()
        },
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified.projection.status,
        InterferenceRuntimeStatus::Inactive
    );
    assert!(classified.projection.current_interference.is_none());
}

#[test]
fn classifier_ignores_single_window_of_successful_third_party_assets() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &RuntimeObservatoryInfo {
            status: RuntimeObservatoryStatus::Active,
            recent_requests: vec![
                RequestSummaryEvent {
                    request_id: "req-4".to_string(),
                    url: "https://cdn-1.example.net/asset.js".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
                RequestSummaryEvent {
                    request_id: "req-5".to_string(),
                    url: "https://cdn-2.example.net/asset.js".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
                RequestSummaryEvent {
                    request_id: "req-6".to_string(),
                    url: "https://cdn-3.example.net/asset.js".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
                RequestSummaryEvent {
                    request_id: "req-7".to_string(),
                    url: "https://cdn-4.example.net/asset.js".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
                RequestSummaryEvent {
                    request_id: "req-8".to_string(),
                    url: "https://cdn-5.example.net/asset.js".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
                RequestSummaryEvent {
                    request_id: "req-9".to_string(),
                    url: "https://cdn-6.example.net/asset.js".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
            ],
            ..RuntimeObservatoryInfo::default()
        },
        &ReadinessInfo {
            status: ReadinessStatus::Active,
            document_ready_state: Some("complete".to_string()),
            ..ReadinessInfo::default()
        },
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified.projection.status,
        rub_core::model::InterferenceRuntimeStatus::Inactive
    );
    assert!(classified.projection.current_interference.is_none());
}

#[test]
fn classifier_ignores_non_web_internal_requests_for_noise() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab(
            "target-1",
            "https://app.example.com/home",
            "Home",
        )],
        &RuntimeObservatoryInfo {
            status: RuntimeObservatoryStatus::Active,
            recent_requests: vec![
                RequestSummaryEvent {
                    request_id: "req-10".to_string(),
                    url: "chrome-untrusted://new-tab-page/one-google-bar?paramsencoded="
                        .to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
                RequestSummaryEvent {
                    request_id: "req-11".to_string(),
                    url: "data:image/svg+xml,%3csvg%3e".to_string(),
                    method: "GET".to_string(),
                    status: Some(200),
                    original_url: None,
                    rewritten_url: None,
                    applied_rule_effects: Vec::new(),
                },
            ],
            ..RuntimeObservatoryInfo::default()
        },
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified.projection.status,
        rub_core::model::InterferenceRuntimeStatus::Inactive
    );
    assert!(classified.projection.current_interference.is_none());
}

#[test]
fn classifier_detects_human_verification_from_title_hint() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline::default(),
        &[active_tab(
            "target-1",
            "https://challenge.example.com/",
            "Verify you are human",
        )],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo {
            status: HumanVerificationHandoffStatus::Available,
            automation_paused: false,
            resume_supported: true,
            unavailable_reason: None,
        },
    );

    assert_eq!(
        classified
            .projection
            .current_interference
            .as_ref()
            .map(|obs| obs.kind),
        Some(InterferenceKind::HumanVerificationRequired)
    );
    assert!(classified.projection.handoff_required);
}

#[test]
fn classifier_detects_unknown_navigation_drift_for_blank_or_error_contexts() {
    let classified = classify(
        &InterferenceRuntimeInfo::default(),
        &InterferenceBaseline {
            primary_target_id: Some("target-1".to_string()),
            primary_url: Some("https://app.example.com/home".to_string()),
            last_tab_count: 1,
        },
        &[active_tab("target-1", "about:blank", "Blank")],
        &RuntimeObservatoryInfo::default(),
        &ReadinessInfo::default(),
        &HumanVerificationHandoffInfo::default(),
    );

    assert_eq!(
        classified
            .projection
            .current_interference
            .as_ref()
            .map(|obs| obs.kind),
        Some(InterferenceKind::UnknownNavigationDrift)
    );
}
