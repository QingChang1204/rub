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
    let primary_target_id = baseline.primary_target_id.clone();
    let primary_url = baseline.primary_url.clone();

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

    let next_baseline = InterferenceBaseline {
        primary_target_id,
        primary_url,
        last_tab_count: tabs.len(),
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
    let current_url = active_tab
        .page_identity_authoritative()
        .then_some(active_tab.url.as_str());
    let title = active_tab
        .page_identity_authoritative()
        .then_some(active_tab.title.as_str());

    if has_human_verification_hint(current_url, title, inputs.readiness, inputs.handoff) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::HumanVerificationRequired,
            summary: "human verification checkpoint detected".to_string(),
            current_url: current_url.map(ToOwned::to_owned),
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
            current_url: current_url.map(ToOwned::to_owned),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_overlay_interference(inputs.readiness) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::OverlayInterference,
            summary: "overlay-related blocking signals detected".to_string(),
            current_url: current_url.map(ToOwned::to_owned),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_interstitial_navigation_hint(current_url, title, inputs.primary_url) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::InterstitialNavigation,
            summary: "interstitial-like navigation drift detected".to_string(),
            current_url: current_url.map(ToOwned::to_owned),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_third_party_noise(
        inputs.current,
        inputs.observatory,
        inputs.readiness,
        inputs.primary_url.or(current_url),
    ) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::ThirdPartyNoise,
            summary: "heavy unrelated third-party request noise detected".to_string(),
            current_url: current_url.map(ToOwned::to_owned),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    if has_unknown_navigation_drift(current_url, inputs.primary_url) {
        return Some(InterferenceObservation {
            kind: InterferenceKind::UnknownNavigationDrift,
            summary: "unexpected navigation drift detected".to_string(),
            current_url: current_url.map(ToOwned::to_owned),
            primary_url: inputs.primary_url.map(ToOwned::to_owned),
        });
    }

    None
}

fn has_human_verification_hint(
    current_url: Option<&str>,
    title: Option<&str>,
    readiness: &ReadinessInfo,
    handoff: &HumanVerificationHandoffInfo,
) -> bool {
    if matches!(handoff.status, HumanVerificationHandoffStatus::Active) {
        return true;
    }
    let haystack = format!(
        "{} {}",
        current_url.unwrap_or_default(),
        title.unwrap_or_default()
    )
    .to_ascii_lowercase();
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
    current_url: Option<&str>,
    title: Option<&str>,
    primary_url: Option<&str>,
) -> bool {
    let Some(current_url) = current_url else {
        return false;
    };
    if primary_url.is_some_and(|primary| primary == current_url) {
        return false;
    }
    let haystack = format!("{current_url} {}", title.unwrap_or_default()).to_ascii_lowercase();
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

fn has_unknown_navigation_drift(current_url: Option<&str>, primary_url: Option<&str>) -> bool {
    let Some(current_url) = current_url else {
        return false;
    };
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
mod tests;
