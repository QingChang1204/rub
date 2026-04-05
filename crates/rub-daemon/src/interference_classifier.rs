use crate::interference_policy::active_policies_for_mode;
use rub_core::model::{
    HumanVerificationHandoffInfo, HumanVerificationHandoffStatus, InterferenceKind,
    InterferenceObservation, InterferenceRuntimeInfo, InterferenceRuntimeStatus, OverlayState,
    ReadinessInfo, RuntimeObservatoryInfo, TabInfo,
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InterferenceBaseline {
    pub primary_target_id: Option<String>,
    pub primary_url: Option<String>,
    pub last_tab_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassifiedInterference {
    pub projection: InterferenceRuntimeInfo,
    pub baseline: InterferenceBaseline,
}

struct ClassifierInputs<'a> {
    current: &'a InterferenceRuntimeInfo,
    active_tab: Option<&'a TabInfo>,
    primary_target_id: Option<&'a str>,
    primary_url: Option<&'a str>,
    previous_tab_count: usize,
    observatory: &'a RuntimeObservatoryInfo,
    readiness: &'a ReadinessInfo,
    handoff: &'a HumanVerificationHandoffInfo,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ThirdPartyNoiseStats {
    failures: usize,
    requests: usize,
    unique_hosts: usize,
}

pub(crate) fn classify(
    current: &InterferenceRuntimeInfo,
    baseline: &InterferenceBaseline,
    tabs: &[TabInfo],
    observatory: &RuntimeObservatoryInfo,
    readiness: &ReadinessInfo,
    handoff: &HumanVerificationHandoffInfo,
) -> ClassifiedInterference {
    let active_tab = tabs.iter().find(|tab| tab.active);
    let active_url = active_tab.map(|tab| tab.url.as_str());
    let primary_target_id = baseline
        .primary_target_id
        .clone()
        .or_else(|| active_tab.map(|tab| tab.target_id.clone()));
    let primary_url = baseline
        .primary_url
        .clone()
        .or_else(|| active_url.map(ToOwned::to_owned));

    let next_observation = classify_observation(ClassifierInputs {
        current,
        active_tab,
        primary_target_id: primary_target_id.as_deref(),
        primary_url: primary_url.as_deref(),
        previous_tab_count: baseline.last_tab_count,
        observatory,
        readiness,
        handoff,
    });

    let mut projection = InterferenceRuntimeInfo {
        mode: current.mode,
        active_policies: active_policies_for_mode(current.mode),
        last_interference: current
            .current_interference
            .clone()
            .or_else(|| current.last_interference.clone()),
        last_recovery_action: current.last_recovery_action,
        last_recovery_result: current.last_recovery_result,
        recovery_in_progress: current.recovery_in_progress,
        handoff_required: matches!(
            next_observation.as_ref().map(|obs| obs.kind),
            Some(InterferenceKind::HumanVerificationRequired)
        ) || current.handoff_required,
        degraded_reason: None,
        ..InterferenceRuntimeInfo::default()
    };

    let next_baseline = if next_observation.is_none() {
        InterferenceBaseline {
            primary_target_id: active_tab.map(|tab| tab.target_id.clone()),
            primary_url: active_url.map(ToOwned::to_owned),
            last_tab_count: tabs.len(),
        }
    } else {
        InterferenceBaseline {
            primary_target_id,
            primary_url,
            last_tab_count: tabs.len(),
        }
    };

    match next_observation {
        Some(observation) => {
            projection.status = InterferenceRuntimeStatus::Active;
            projection.current_interference = Some(observation);
        }
        None => {
            projection.status = InterferenceRuntimeStatus::Inactive;
            projection.current_interference = None;
            projection.handoff_required = false;
        }
    }

    if let Some(current) = &projection.current_interference {
        projection.last_interference = Some(current.clone());
    }

    ClassifiedInterference {
        projection,
        baseline: next_baseline,
    }
}

fn classify_observation(inputs: ClassifierInputs<'_>) -> Option<InterferenceObservation> {
    let active_tab = inputs.active_tab?;
    let current_url = active_tab.url.as_str();
    let title = active_tab.title.as_str();

    if has_human_verification_hint(current_url, title, inputs.readiness, inputs.handoff) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::HumanVerificationRequired,
            summary: "human verification checkpoint detected".to_string(),
            current_url: Some(current_url.to_string()),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if is_popup_hijack(
        active_tab,
        inputs.primary_target_id,
        inputs.previous_tab_count,
    ) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::PopupHijack,
            summary: "unexpected active tab drift with additional tab(s) detected".to_string(),
            current_url: Some(current_url.to_string()),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_overlay_interference(inputs.readiness) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::OverlayInterference,
            summary: "overlay-related blocking signals detected".to_string(),
            current_url: Some(current_url.to_string()),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_interstitial_navigation_hint(current_url, title, inputs.primary_url) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::InterstitialNavigation,
            summary: "interstitial-like navigation drift detected".to_string(),
            current_url: Some(current_url.to_string()),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_third_party_noise(
        inputs.current,
        inputs.observatory,
        inputs.readiness,
        inputs.primary_url.or(Some(current_url)),
    ) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::ThirdPartyNoise,
            summary: "heavy unrelated third-party request noise detected".to_string(),
            current_url: Some(current_url.to_string()),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_unknown_navigation_drift(current_url, inputs.primary_url) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::UnknownNavigationDrift,
            summary: "unexpected navigation drift detected".to_string(),
            current_url: Some(current_url.to_string()),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    None
}

fn has_human_verification_hint(
    current_url: &str,
    title: &str,
    readiness: &ReadinessInfo,
    handoff: &HumanVerificationHandoffInfo,
) -> bool {
    if matches!(handoff.status, HumanVerificationHandoffStatus::Active) {
        return true;
    }
    let haystack = format!("{current_url} {title}").to_ascii_lowercase();
    [
        "captcha",
        "recaptcha",
        "hcaptcha",
        "turnstile",
        "cf-challenge",
        "verify you are human",
        "human verification",
        "are you human",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
        || readiness
            .blocking_signals
            .iter()
            .any(|signal| signal.contains("human_verification"))
}

fn is_popup_hijack(
    active_tab: &TabInfo,
    primary_target_id: Option<&str>,
    previous_tab_count: usize,
) -> bool {
    let Some(primary_target_id) = primary_target_id else {
        return false;
    };
    active_tab.target_id != primary_target_id && previous_tab_count > 0
}

fn has_overlay_interference(readiness: &ReadinessInfo) -> bool {
    !matches!(readiness.overlay_state, OverlayState::None)
        || readiness
            .blocking_signals
            .iter()
            .any(|signal| signal.starts_with("overlay:"))
}

fn has_interstitial_navigation_hint(
    current_url: &str,
    title: &str,
    primary_url: Option<&str>,
) -> bool {
    if primary_url.is_some_and(|primary| primary == current_url) {
        return false;
    }
    let haystack = format!("{current_url} {title}").to_ascii_lowercase();
    ["interstitial", "vignette", "redirect notice"]
        .iter()
        .any(|needle| haystack.contains(needle))
}

fn has_third_party_noise(
    current: &InterferenceRuntimeInfo,
    observatory: &RuntimeObservatoryInfo,
    readiness: &ReadinessInfo,
    primary_url: Option<&str>,
) -> bool {
    let stats = third_party_noise_stats(observatory, primary_url);
    if !is_readiness_stable_for_noise(readiness) {
        return false;
    }

    let prior_noise = matches!(
        current
            .current_interference
            .as_ref()
            .map(|value| value.kind),
        Some(InterferenceKind::ThirdPartyNoise)
    ) || matches!(
        current.last_interference.as_ref().map(|value| value.kind),
        Some(InterferenceKind::ThirdPartyNoise)
    );

    if stats.failures >= 5 && stats.unique_hosts >= 4 {
        return true;
    }

    prior_noise
        && ((stats.failures >= 2 && stats.unique_hosts >= 2)
            || (stats.requests >= 8 && stats.unique_hosts >= 4))
}

fn third_party_noise_stats(
    observatory: &RuntimeObservatoryInfo,
    primary_url: Option<&str>,
) -> ThirdPartyNoiseStats {
    let Some(primary_host) = primary_url.and_then(web_host_of) else {
        return ThirdPartyNoiseStats::default();
    };

    let mut unique_hosts = BTreeSet::new();
    let mut failures = 0usize;
    let mut requests = 0usize;

    for event in &observatory.recent_network_failures {
        if let Some(host) = web_host_of(&event.url)
            && host != primary_host
        {
            failures += 1;
            unique_hosts.insert(host.to_string());
        }
    }

    for event in &observatory.recent_requests {
        if let Some(host) = web_host_of(&event.url)
            && host != primary_host
        {
            requests += 1;
            unique_hosts.insert(host.to_string());
        }
    }

    ThirdPartyNoiseStats {
        failures,
        requests,
        unique_hosts: unique_hosts.len(),
    }
}

fn is_readiness_stable_for_noise(readiness: &ReadinessInfo) -> bool {
    matches!(readiness.overlay_state, OverlayState::None)
        && readiness.blocking_signals.is_empty()
        && readiness
            .document_ready_state
            .as_deref()
            .is_none_or(|value| value.eq_ignore_ascii_case("complete"))
}

fn has_unknown_navigation_drift(current_url: &str, primary_url: Option<&str>) -> bool {
    let Some(primary_url) = primary_url else {
        return false;
    };
    if current_url == primary_url {
        return false;
    }
    let current = current_url.to_ascii_lowercase();
    current.starts_with("about:blank")
        || current.starts_with("chrome-error://")
        || current.starts_with("about:srcdoc")
        || current.starts_with("chrome://newtab")
}

fn web_host_of(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let host = rest.split('/').next()?;
    let host = host.split('@').next_back()?;
    let host = host.split(':').next()?;
    if host.is_empty() { None } else { Some(host) }
}

#[cfg(test)]
mod tests {
    use super::{InterferenceBaseline, classify};
    use rub_core::model::{
        HumanVerificationHandoffInfo, HumanVerificationHandoffStatus, InterferenceKind,
        InterferenceMode, InterferenceObservation, InterferenceRuntimeInfo,
        InterferenceRuntimeStatus, NetworkFailureEvent, OverlayState, ReadinessInfo,
        ReadinessStatus, RequestSummaryEvent, RuntimeObservatoryInfo, RuntimeObservatoryStatus,
        TabInfo,
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
}
