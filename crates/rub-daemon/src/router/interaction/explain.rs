mod hints;
mod target;

use self::hints::{
    interactability_has_page_level_blocker, interactability_has_target_waitable_blocker,
    interactability_next_command_hints, interactability_wait_command,
};
use self::target::interactability_target_summary;
use super::super::addressing::resolve_element;
use super::*;
use crate::runtime_refresh::{
    InterferenceRefreshIntent, refresh_live_interference_state, refresh_live_runtime_state,
};
use crate::session::SessionState;
use rub_core::error::ErrorCode;

pub(super) async fn cmd_interactability_probe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let resolved =
        resolve_element(router, args, state, deadline, "explain interactability").await?;
    refresh_live_runtime_state(&router.browser, state).await;
    let _ = refresh_live_interference_state(
        &router.browser,
        state,
        InterferenceRefreshIntent::ReadOnly,
    )
    .await;
    let readiness = state.readiness_state().await;
    let interference = state.interference_runtime().await;

    Ok(interactability_probe_payload(
        &resolved.element,
        &resolved.snapshot_id,
        args,
        &readiness,
        &interference,
    ))
}

fn interactability_probe_payload(
    element: &rub_core::model::Element,
    snapshot_id: &str,
    locator_request: &serde_json::Value,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
) -> serde_json::Value {
    let assessment = interactability_assessment(element, readiness, interference);
    serde_json::json!({
        "subject": {
            "kind": "interactability_explain",
            "surface": "interactive_snapshot",
            "snapshot_id": snapshot_id,
            "locator_request": locator_request,
        },
        "result": {
            "target": interactability_target_summary(element),
            "hit_test": {
                "status": "not_probed",
                "detail": "first_slice_uses_snapshot_target_and_runtime_blockers_only",
            },
            "top_blocking_element": serde_json::Value::Null,
            "readiness": {
                "status": readiness.status,
                "route_stability": readiness.route_stability,
                "loading_present": readiness.loading_present,
                "skeleton_present": readiness.skeleton_present,
                "overlay_state": readiness.overlay_state,
                "blocking_signals": readiness.blocking_signals,
                "degraded_reason": readiness.degraded_reason,
            },
            "interference": {
                "status": interference.status,
                "current_interference": interference.current_interference,
                "handoff_required": interference.handoff_required,
                "degraded_reason": interference.degraded_reason,
            },
            "assessment": interactability_assessment_payload(
                element,
                locator_request,
                &assessment,
            ),
        }
    })
}

fn interactability_assessment_payload(
    element: &rub_core::model::Element,
    locator_request: &serde_json::Value,
    assessment: &serde_json::Value,
) -> serde_json::Value {
    let mut payload = assessment.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "next_command_hints".to_string(),
            serde_json::Value::Array(interactability_next_command_hints(
                element,
                assessment,
                locator_request,
            )),
        );
    }
    payload
}

fn interactability_assessment(
    element: &rub_core::model::Element,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
) -> serde_json::Value {
    let blocker_details = interactability_blocker_details(element, readiness, interference);
    let blockers = blocker_details
        .iter()
        .filter_map(|detail| {
            detail
                .get("code")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();

    let likely_interactable = blockers.is_empty();
    serde_json::json!({
        "likely_interactable": likely_interactable,
        "blockers": blockers,
        "blocker_details": blocker_details,
        "summary": interactability_summary(likely_interactable, readiness, interference, element),
        "next_safe_actions": interactability_next_actions(likely_interactable, readiness, interference, element),
    })
}

fn interactability_blocker_details(
    element: &rub_core::model::Element,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
) -> Vec<serde_json::Value> {
    let mut details = Vec::new();

    if element.attributes.contains_key("disabled") {
        details.push(interactability_blocker_detail(
            "disabled_element",
            "interactive_snapshot",
            "Target is explicitly disabled in the authoritative snapshot.",
            None,
        ));
    }
    if element
        .attributes
        .get("aria-disabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        details.push(interactability_blocker_detail(
            "aria_disabled",
            "interactive_snapshot",
            "Target is marked aria-disabled in the authoritative snapshot.",
            None,
        ));
    }
    if !matches!(readiness.overlay_state, rub_core::model::OverlayState::None) {
        details.push(interactability_blocker_detail(
            "overlay_present",
            "runtime.readiness",
            "Readiness reports an overlay-related blocker on the page.",
            Some("rub runtime readiness"),
        ));
    }
    if matches!(
        readiness.route_stability,
        rub_core::model::RouteStability::Transitioning
    ) {
        details.push(interactability_blocker_detail(
            "route_transitioning",
            "runtime.readiness",
            "The page is still transitioning, so actuation timing is not yet stable.",
            Some("rub runtime readiness"),
        ));
    }
    if readiness.loading_present {
        details.push(interactability_blocker_detail(
            "loading_present",
            "runtime.readiness",
            "Readiness still reports active loading indicators on the page.",
            Some("rub runtime readiness"),
        ));
    }
    if readiness.skeleton_present {
        details.push(interactability_blocker_detail(
            "skeleton_present",
            "runtime.readiness",
            "Skeleton placeholders are still present, so the actionable surface may not be final.",
            Some("rub runtime readiness"),
        ));
    }
    if matches!(readiness.status, rub_core::model::ReadinessStatus::Degraded) {
        details.push(interactability_blocker_detail(
            "readiness_degraded",
            "runtime.readiness",
            "The readiness surface is degraded, so live blocker projection is not fully authoritative.",
            Some("rub runtime readiness"),
        ));
    }
    if let Some(current) = &interference.current_interference {
        details.push(interactability_blocker_detail(
            &format!(
                "interference:{}",
                interactability_interference_kind_name(current.kind)
            ),
            "runtime.interference",
            &format!(
                "Current runtime interference suggests this target is blocked by {}.",
                interactability_interference_kind_name(current.kind)
            ),
            match current.kind {
                rub_core::model::InterferenceKind::OverlayInterference => {
                    Some("rub interference recover")
                }
                rub_core::model::InterferenceKind::HumanVerificationRequired => {
                    Some("rub handoff start")
                }
                _ => Some("rub runtime interference"),
            },
        ));
    }
    if interference.handoff_required {
        details.push(interactability_blocker_detail(
            "handoff_required",
            "runtime.interference",
            "The runtime currently requires a human-verification handoff before automation can continue.",
            Some("rub handoff start"),
        ));
    }
    if matches!(
        interference.status,
        rub_core::model::InterferenceRuntimeStatus::Degraded
    ) {
        details.push(interactability_blocker_detail(
            "interference_degraded",
            "runtime.interference",
            "The interference surface is degraded, so blocker recovery advice may be incomplete.",
            Some("rub runtime interference"),
        ));
    }

    details
}

fn interactability_blocker_detail(
    code: &str,
    surface: &str,
    summary: &str,
    recommended_command: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "surface": surface,
        "summary": summary,
        "recommended_command": recommended_command,
    })
}

fn interactability_interference_kind_name(kind: rub_core::model::InterferenceKind) -> &'static str {
    match kind {
        rub_core::model::InterferenceKind::InterstitialNavigation => "interstitial_navigation",
        rub_core::model::InterferenceKind::PopupHijack => "popup_hijack",
        rub_core::model::InterferenceKind::OverlayInterference => "overlay_interference",
        rub_core::model::InterferenceKind::ThirdPartyNoise => "third_party_noise",
        rub_core::model::InterferenceKind::HumanVerificationRequired => {
            "human_verification_required"
        }
        rub_core::model::InterferenceKind::UnknownNavigationDrift => "unknown_navigation_drift",
    }
}

fn interactability_summary(
    likely_interactable: bool,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    element: &rub_core::model::Element,
) -> String {
    if likely_interactable {
        return "No authoritative disabled-state or ambient blocker is currently projected for this target.".to_string();
    }
    if element.attributes.contains_key("disabled") {
        return "Target is explicitly disabled in the authoritative snapshot.".to_string();
    }
    if element
        .attributes
        .get("aria-disabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return "Target is marked aria-disabled in the authoritative snapshot.".to_string();
    }
    if let Some(current) = &interference.current_interference {
        return format!(
            "Current runtime interference suggests this target is blocked by {}.",
            interactability_interference_kind_name(current.kind)
        );
    }
    if !matches!(readiness.overlay_state, rub_core::model::OverlayState::None) {
        return "Readiness reports an overlay-related blocker on the page.".to_string();
    }
    if matches!(
        readiness.route_stability,
        rub_core::model::RouteStability::Transitioning
    ) {
        return "Readiness reports that the page is still transitioning.".to_string();
    }
    "Interactability is not yet safe because the authoritative runtime still reports blocker signals.".to_string()
}

fn interactability_next_actions(
    likely_interactable: bool,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    element: &rub_core::model::Element,
) -> Vec<String> {
    if likely_interactable {
        return vec![
            "Try the intended interaction directly; there are no current runtime blockers."
                .to_string(),
            "If actuation still fails, compare with `rub explain locator ...` to confirm the exact target.".to_string(),
        ];
    }

    let mut actions = Vec::new();
    if element.attributes.contains_key("disabled")
        || element
            .attributes
            .get("aria-disabled")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        actions.push(
            "Wait for the control to become enabled, or choose a different actionable target."
                .to_string(),
        );
    }
    if !matches!(readiness.overlay_state, rub_core::model::OverlayState::None) {
        actions.push(
            "Dismiss or accept the blocking overlay, then retry the interaction.".to_string(),
        );
        actions.push(
            "Run `rub runtime readiness` to inspect `overlay_state` and `blocking_signals` directly."
                .to_string(),
        );
    }
    if let Some(current) = &interference.current_interference {
        if matches!(
            current.kind,
            rub_core::model::InterferenceKind::OverlayInterference
        ) {
            actions.push(
                "Run `rub interference recover` if you want the canonical overlay recovery path."
                    .to_string(),
            );
        } else if matches!(
            current.kind,
            rub_core::model::InterferenceKind::HumanVerificationRequired
        ) {
            actions.push(
                "Start a handoff if human verification is required before continuing.".to_string(),
            );
        }
    }
    if matches!(
        readiness.route_stability,
        rub_core::model::RouteStability::Transitioning
    ) || readiness.loading_present
        || readiness.skeleton_present
    {
        actions.push(
            "Wait for route/loading blockers to clear before retrying the interaction.".to_string(),
        );
    }
    actions.push(
        "Use `rub explain locator ...` if you still need to confirm that the intended candidate is the one being targeted.".to_string(),
    );
    actions
}

pub(super) async fn enrich_interactability_error_if_needed(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    command: &str,
    element: &rub_core::model::Element,
    snapshot_id: &str,
    locator_request: &serde_json::Value,
    error: RubError,
) -> RubError {
    let envelope = error.into_envelope();
    if envelope.code != ErrorCode::ElementNotInteractable {
        return RubError::Domain(envelope);
    }

    refresh_live_runtime_state(&router.browser, state).await;
    let _ = refresh_live_interference_state(
        &router.browser,
        state,
        InterferenceRefreshIntent::ReadOnly,
    )
    .await;
    let readiness = state.readiness_state().await;
    let interference = state.interference_runtime().await;

    enrich_interactability_error_envelope(
        envelope,
        command,
        element,
        snapshot_id,
        locator_request,
        &readiness,
        &interference,
    )
}

fn enrich_interactability_error_envelope(
    envelope: rub_core::error::ErrorEnvelope,
    command: &str,
    element: &rub_core::model::Element,
    snapshot_id: &str,
    locator_request: &serde_json::Value,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
) -> RubError {
    if envelope.code != ErrorCode::ElementNotInteractable {
        return RubError::Domain(envelope);
    }

    let assessment = interactability_assessment(element, readiness, interference);
    let suggestion = interactability_error_suggestion(&envelope, &assessment, locator_request);
    let context = interactability_error_context(
        envelope.context,
        command,
        element,
        snapshot_id,
        locator_request,
        &assessment,
    );

    RubError::Domain(rub_core::error::ErrorEnvelope {
        code: envelope.code,
        message: envelope.message,
        suggestion,
        context: Some(context),
    })
}

fn interactability_error_context(
    upstream_context: Option<serde_json::Value>,
    command: &str,
    element: &rub_core::model::Element,
    snapshot_id: &str,
    locator_request: &serde_json::Value,
    assessment: &serde_json::Value,
) -> serde_json::Value {
    let mut context = serde_json::Map::new();
    context.insert(
        "surface".to_string(),
        serde_json::json!("router.interaction.error_projection"),
    );
    context.insert("command".to_string(), serde_json::json!(command));
    context.insert("snapshot_id".to_string(), serde_json::json!(snapshot_id));
    context.insert("locator_request".to_string(), locator_request.clone());
    context.insert(
        "target".to_string(),
        interactability_target_summary(element),
    );
    context.insert("interactability".to_string(), assessment.clone());
    if let Some(upstream_context) = upstream_context {
        context.insert("upstream_context".to_string(), upstream_context);
    }
    serde_json::Value::Object(context)
}

fn interactability_error_suggestion(
    envelope: &rub_core::error::ErrorEnvelope,
    assessment: &serde_json::Value,
    locator_request: &serde_json::Value,
) -> String {
    let wait_command = if interactability_has_target_waitable_blocker(assessment) {
        interactability_wait_command(locator_request)
    } else {
        None
    };
    let page_level_blockers = interactability_has_page_level_blocker(assessment);
    let first_action = assessment
        .get("next_safe_actions")
        .and_then(serde_json::Value::as_array)
        .and_then(|actions| actions.first())
        .and_then(serde_json::Value::as_str);
    let first_command = assessment
        .get("blocker_details")
        .and_then(serde_json::Value::as_array)
        .and_then(|details| {
            details.iter().find_map(|detail| {
                detail
                    .get("recommended_command")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
            })
        });

    let mut guidance = Vec::new();
    if let Some(wait_command) = wait_command {
        guidance.push(format!(
            "Run `{wait_command}` to wait for this target to become interactable before retrying."
        ));
    }
    if page_level_blockers {
        guidance.push(
            "Run `rub explain blockers` to inspect the dominant page-level blocker in the current runtime."
                .to_string(),
        );
    } else if let Some(command) = first_command {
        guidance.push(format!(
            "If you need the canonical blocker surface, run `{command}`."
        ));
    }
    if let Some(action) = first_action {
        guidance.push(action.to_string());
    }

    if guidance.is_empty() {
        format!(
            "{} Use `rub explain interactability ...` for target-specific blocker details and next safe actions.",
            envelope.suggestion
        )
    } else {
        guidance.push(
            "Use `rub explain interactability ...` for target-specific blocker details and next safe actions."
                .to_string(),
        );
        guidance.join(" ")
    }
}

#[cfg(test)]
mod tests;
