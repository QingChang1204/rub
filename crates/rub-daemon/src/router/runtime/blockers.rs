use super::*;
use crate::runtime_refresh::refresh_live_runtime_and_interference;
use crate::session::SessionState;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockerDiagnosisClass {
    Clear,
    OverlayBlocker,
    ProviderGate,
    RouteTransition,
    DegradedRuntime,
    UnknownBlocker,
}

pub(super) async fn cmd_blocker_diagnose(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _ = refresh_live_runtime_and_interference(&router.browser, state).await;
    let readiness = state.readiness_state().await;
    let interference = state.interference_runtime().await;
    let handoff = state.human_verification_handoff().await;
    Ok(blocker_diagnosis_payload(
        &readiness,
        &interference,
        &handoff,
    ))
}

pub(super) fn blocker_diagnosis_payload(
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "blocker_explain",
            "surface": "runtime_blockers",
        },
        "result": {
            "diagnosis": blocker_diagnosis_result(readiness, interference, handoff),
            "readiness": readiness,
            "interference": interference,
            "handoff": handoff,
        }
    })
}

pub(super) fn blocker_diagnosis_result(
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> serde_json::Value {
    let class = blocker_diagnosis_class(readiness, interference, handoff);
    let primary_reason = blocker_diagnosis_primary_reason(class, readiness, interference, handoff);
    serde_json::json!({
        "class": blocker_diagnosis_class_name(class),
        "primary_reason": primary_reason,
        "authoritative": !matches!(class, BlockerDiagnosisClass::DegradedRuntime),
        "summary": blocker_diagnosis_summary(class, readiness, interference, handoff),
        "next_safe_actions": blocker_diagnosis_actions(class, readiness, interference, handoff),
        "details": blocker_diagnosis_details(class, readiness, interference, handoff),
        "workflow_guidance": blocker_diagnosis_workflow_guidance(class, primary_reason),
    })
}

fn blocker_diagnosis_class(
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> BlockerDiagnosisClass {
    if matches!(readiness.status, rub_core::model::ReadinessStatus::Degraded)
        || matches!(
            interference.status,
            rub_core::model::InterferenceRuntimeStatus::Degraded
        )
    {
        return BlockerDiagnosisClass::DegradedRuntime;
    }
    if interference.handoff_required
        || matches!(
            handoff.status,
            rub_core::model::HumanVerificationHandoffStatus::Active
        )
        || interference
            .current_interference
            .as_ref()
            .is_some_and(|current| {
                matches!(
                    current.kind,
                    rub_core::model::InterferenceKind::HumanVerificationRequired
                )
            })
    {
        return BlockerDiagnosisClass::ProviderGate;
    }
    if !matches!(readiness.overlay_state, rub_core::model::OverlayState::None)
        || interference
            .current_interference
            .as_ref()
            .is_some_and(|current| {
                matches!(
                    current.kind,
                    rub_core::model::InterferenceKind::OverlayInterference
                )
            })
    {
        return BlockerDiagnosisClass::OverlayBlocker;
    }
    if matches!(
        readiness.route_stability,
        rub_core::model::RouteStability::Transitioning
    ) || readiness.loading_present
        || readiness.skeleton_present
    {
        return BlockerDiagnosisClass::RouteTransition;
    }
    if interference.current_interference.is_some() {
        return BlockerDiagnosisClass::UnknownBlocker;
    }
    BlockerDiagnosisClass::Clear
}

fn blocker_diagnosis_class_name(class: BlockerDiagnosisClass) -> &'static str {
    match class {
        BlockerDiagnosisClass::Clear => "clear",
        BlockerDiagnosisClass::OverlayBlocker => "overlay_blocker",
        BlockerDiagnosisClass::ProviderGate => "provider_gate",
        BlockerDiagnosisClass::RouteTransition => "route_transition",
        BlockerDiagnosisClass::DegradedRuntime => "degraded_runtime",
        BlockerDiagnosisClass::UnknownBlocker => "unknown_blocker",
    }
}

fn blocker_diagnosis_primary_reason(
    class: BlockerDiagnosisClass,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> &'static str {
    match class {
        BlockerDiagnosisClass::Clear => "clear",
        BlockerDiagnosisClass::OverlayBlocker => {
            if interference
                .current_interference
                .as_ref()
                .is_some_and(|current| {
                    matches!(
                        current.kind,
                        rub_core::model::InterferenceKind::OverlayInterference
                    )
                })
            {
                "overlay_interference"
            } else {
                "overlay_state"
            }
        }
        BlockerDiagnosisClass::ProviderGate => {
            if matches!(
                handoff.status,
                rub_core::model::HumanVerificationHandoffStatus::Active
            ) {
                "handoff_active"
            } else if interference.handoff_required {
                "handoff_required"
            } else if interference
                .current_interference
                .as_ref()
                .is_some_and(|current| {
                    matches!(
                        current.kind,
                        rub_core::model::InterferenceKind::HumanVerificationRequired
                    )
                })
            {
                "human_verification_required"
            } else {
                "provider_gate"
            }
        }
        BlockerDiagnosisClass::RouteTransition => {
            if matches!(
                readiness.route_stability,
                rub_core::model::RouteStability::Transitioning
            ) {
                "route_stability_transitioning"
            } else if readiness.loading_present {
                "loading_present"
            } else if readiness.skeleton_present {
                "skeleton_present"
            } else {
                "route_transition"
            }
        }
        BlockerDiagnosisClass::DegradedRuntime => {
            if matches!(readiness.status, rub_core::model::ReadinessStatus::Degraded)
                && matches!(
                    interference.status,
                    rub_core::model::InterferenceRuntimeStatus::Degraded
                )
            {
                "runtime_surfaces_degraded"
            } else if matches!(readiness.status, rub_core::model::ReadinessStatus::Degraded) {
                "readiness_degraded"
            } else {
                "interference_degraded"
            }
        }
        BlockerDiagnosisClass::UnknownBlocker => interference
            .current_interference
            .as_ref()
            .map(|current| match current.kind {
                rub_core::model::InterferenceKind::InterstitialNavigation => {
                    "interstitial_navigation"
                }
                rub_core::model::InterferenceKind::PopupHijack => "popup_hijack",
                rub_core::model::InterferenceKind::OverlayInterference => "overlay_interference",
                rub_core::model::InterferenceKind::ThirdPartyNoise => "third_party_noise",
                rub_core::model::InterferenceKind::HumanVerificationRequired => {
                    "human_verification_required"
                }
                rub_core::model::InterferenceKind::UnknownNavigationDrift => {
                    "unknown_navigation_drift"
                }
            })
            .unwrap_or("unknown_blocker"),
    }
}

fn blocker_diagnosis_summary(
    class: BlockerDiagnosisClass,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> String {
    match class {
        BlockerDiagnosisClass::Clear => {
            "No dominant page-level blocker is currently projected by the runtime surfaces."
                .to_string()
        }
        BlockerDiagnosisClass::OverlayBlocker => {
            "A blocking overlay is the dominant page-level blocker right now.".to_string()
        }
        BlockerDiagnosisClass::ProviderGate => {
            if matches!(
                handoff.status,
                rub_core::model::HumanVerificationHandoffStatus::Active
            ) {
                "A provider or verification gate is active and automation is currently paused for handoff."
                    .to_string()
            } else if interference.handoff_required {
                "A provider or verification gate has required handoff before automation can continue."
                    .to_string()
            } else {
                "A provider or verification gate is the dominant blocker right now.".to_string()
            }
        }
        BlockerDiagnosisClass::RouteTransition => {
            if matches!(
                readiness.route_stability,
                rub_core::model::RouteStability::Transitioning
            ) {
                "The page is still transitioning to a new route, so acting now is likely premature."
                    .to_string()
            } else if readiness.loading_present {
                "Loading blockers are still present, so the actionable surface may not be final yet."
                    .to_string()
            } else if readiness.skeleton_present {
                "Skeleton placeholders are still present, so the actionable surface may not be final yet."
                    .to_string()
            } else {
                "The page is still transitioning or loading, so acting now is likely premature."
                    .to_string()
            }
        }
        BlockerDiagnosisClass::DegradedRuntime => format!(
            "Blocker diagnosis is not fully authoritative because runtime surfaces are degraded (readiness: {}, interference: {}).",
            blocker_diagnosis_readiness_status_name(readiness.status),
            blocker_diagnosis_interference_status_name(interference.status),
        ),
        BlockerDiagnosisClass::UnknownBlocker => {
            "The runtime sees a blocker, but it does not currently map to a more specific product class."
                .to_string()
        }
    }
}

fn blocker_diagnosis_actions(
    class: BlockerDiagnosisClass,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> Vec<String> {
    match class {
        BlockerDiagnosisClass::Clear => vec![
            "Try the intended action directly; no dominant page-level blocker is currently projected.".to_string(),
            "Use `rub explain interactability ...` if the failure is target-specific instead of page-level.".to_string(),
        ],
        BlockerDiagnosisClass::OverlayBlocker => {
            let mut actions = vec![
                "Dismiss or accept the overlay before retrying the workflow.".to_string(),
                "Run `rub runtime readiness` to inspect `overlay_state` and `blocking_signals` directly.".to_string(),
            ];
            if interference.current_interference.as_ref().is_some_and(|current| {
                matches!(
                    current.kind,
                    rub_core::model::InterferenceKind::OverlayInterference
                )
            }) {
                actions.push(
                    "Run `rub interference recover` if you want the canonical overlay recovery path.".to_string(),
                );
            }
            actions
        }
        BlockerDiagnosisClass::ProviderGate => {
            let mut actions = vec!["Run `rub handoff status` to inspect whether human verification is already active.".to_string()];
            if matches!(
                handoff.status,
                rub_core::model::HumanVerificationHandoffStatus::Active
            ) {
                actions.push(
                    "Complete the manual verification, then run `rub handoff complete` before resuming automation.".to_string(),
                );
            } else {
                actions.push(
                    "Run `rub handoff start` if manual verification is required before continuing.".to_string(),
                );
            }
            actions.push(
                "Run `rub runtime interference` to inspect the classified gate and current recovery state.".to_string(),
            );
            actions
        }
        BlockerDiagnosisClass::RouteTransition => {
            let mut actions = vec![
                if matches!(
                    readiness.route_stability,
                    rub_core::model::RouteStability::Transitioning
                ) {
                    "Wait for the route transition to settle before retrying the interaction.".to_string()
                } else if readiness.loading_present {
                    "Wait for loading blockers to clear before retrying the interaction.".to_string()
                } else if readiness.skeleton_present {
                    "Wait for skeleton placeholders to clear before retrying the interaction.".to_string()
                } else {
                    "Wait for route and loading blockers to clear before retrying the interaction.".to_string()
                },
                "Run `rub runtime readiness` to watch route stability, loading, and skeleton signals.".to_string(),
            ];
            if readiness.loading_present || readiness.skeleton_present {
                actions.push(
                    "Use `rub wait ...` once the expected post-navigation target is known, instead of shell polling.".to_string(),
                );
            }
            actions
        }
        BlockerDiagnosisClass::DegradedRuntime => vec![
            "Run `rub runtime summary` to inspect which runtime surfaces are currently degraded."
                .to_string(),
            "Check `rub runtime readiness` and `rub runtime interference` before trusting blocker diagnosis."
                .to_string(),
        ],
        BlockerDiagnosisClass::UnknownBlocker => vec![
            "Run `rub runtime interference` to inspect the current classified interference in more detail."
                .to_string(),
            "Run `rub runtime readiness` to confirm whether route, loading, or overlay signals also explain the blockage."
                .to_string(),
            "Use `rub explain interactability ...` if the workflow is failing on one specific target."
                .to_string(),
        ],
    }
}

fn blocker_diagnosis_details(
    class: BlockerDiagnosisClass,
    readiness: &rub_core::model::ReadinessInfo,
    interference: &rub_core::model::InterferenceRuntimeInfo,
    handoff: &rub_core::model::HumanVerificationHandoffInfo,
) -> Vec<serde_json::Value> {
    let mut details = Vec::new();
    match class {
        BlockerDiagnosisClass::Clear => {
            details.push(serde_json::json!({
                "surface": "runtime.summary",
                "summary": "No dominant page-level blocker is projected right now."
            }));
        }
        BlockerDiagnosisClass::OverlayBlocker => {
            details.push(serde_json::json!({
                "surface": "runtime.readiness",
                "overlay_state": readiness.overlay_state,
                "blocking_signals": readiness.blocking_signals,
            }));
            if let Some(current) = &interference.current_interference {
                details.push(serde_json::json!({
                    "surface": "runtime.interference",
                    "kind": current.kind,
                    "summary": current.summary,
                    "current_url": current.current_url,
                    "primary_url": current.primary_url,
                }));
            }
        }
        BlockerDiagnosisClass::ProviderGate => {
            if let Some(current) = &interference.current_interference {
                details.push(serde_json::json!({
                    "surface": "runtime.interference",
                    "kind": current.kind,
                    "summary": current.summary,
                    "current_url": current.current_url,
                    "primary_url": current.primary_url,
                }));
            }
            details.push(serde_json::json!({
                "surface": "runtime.handoff",
                "status": handoff.status,
                "automation_paused": handoff.automation_paused,
                "resume_supported": handoff.resume_supported,
                "unavailable_reason": handoff.unavailable_reason,
            }));
        }
        BlockerDiagnosisClass::RouteTransition => {
            details.push(serde_json::json!({
                "surface": "runtime.readiness",
                "route_stability": readiness.route_stability,
                "loading_present": readiness.loading_present,
                "skeleton_present": readiness.skeleton_present,
                "blocking_signals": readiness.blocking_signals,
            }));
        }
        BlockerDiagnosisClass::DegradedRuntime => {
            details.push(serde_json::json!({
                "surface": "runtime.readiness",
                "status": readiness.status,
                "degraded_reason": readiness.degraded_reason,
            }));
            details.push(serde_json::json!({
                "surface": "runtime.interference",
                "status": interference.status,
                "degraded_reason": interference.degraded_reason,
            }));
        }
        BlockerDiagnosisClass::UnknownBlocker => {
            if let Some(current) = &interference.current_interference {
                details.push(serde_json::json!({
                    "surface": "runtime.interference",
                    "kind": current.kind,
                    "summary": current.summary,
                    "current_url": current.current_url,
                    "primary_url": current.primary_url,
                }));
            } else {
                details.push(serde_json::json!({
                    "surface": "runtime.summary",
                    "summary": "No overlay, route, or provider-gate class matched, but a blocker was still inferred.",
                }));
            }
        }
    }
    details
}

fn blocker_diagnosis_workflow_guidance(
    class: BlockerDiagnosisClass,
    primary_reason: &str,
) -> serde_json::Value {
    match class {
        BlockerDiagnosisClass::ProviderGate => match primary_reason {
            "handoff_active" => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "Manual verification is already active in the current runtime. Finish recovery here before resuming automation.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "manual_recovery_runtime",
                        "summary": "Keep this runtime as the manual recovery surface until verification is complete."
                    },
                    "recommended_runtime": {
                        "role": "manual_recovery_runtime",
                        "summary": "Stay in this runtime to complete the current handoff before resuming automation."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub handoff status",
                        "reason": "inspect the current handoff state before continuing manual recovery"
                    },
                    {
                        "command": "rub handoff complete",
                        "reason": "resume automation in the same runtime after manual verification succeeds"
                    }
                ]
            }),
            _ => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "fresh_rub_home",
                "summary": "Keep the gated runtime for handoff or inspection, and continue alternate-provider work in a fresh RUB_HOME.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "gated_recovery_runtime",
                        "summary": "Keep this runtime for handoff, inspection, or manual recovery of the gated provider flow."
                    },
                    "recommended_runtime": {
                        "role": "alternate_provider_runtime",
                        "summary": "Use the fresh RUB_HOME to continue the broader workflow against an alternate provider."
                    }
                },
                "recommended_runtime": {
                    "kind": "fresh_rub_home",
                    "rub_home_hint": "<fresh RUB_HOME>",
                    "session": "default",
                    "reason": "isolated_runtime_recommended",
                },
                "next_command_hints": [
                    {
                        "command": "rub handoff start",
                        "reason": "pause automation here and move the gated page into manual recovery"
                    },
                    {
                        "command": "rub --rub-home <fresh-home> open <alternate provider url>",
                        "reason": "continue the broader workflow in a separate isolated runtime"
                    }
                ]
            }),
        },
        BlockerDiagnosisClass::OverlayBlocker => match primary_reason {
            "overlay_interference" => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "Overlay interference is active in the current runtime. Recover it here before continuing the workflow.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "manual_recovery_runtime",
                        "summary": "Keep this runtime as the blocker-recovery surface while you clear the overlay."
                    },
                    "recommended_runtime": {
                        "role": "manual_recovery_runtime",
                        "summary": "Stay in this runtime while you run the canonical overlay recovery path."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub interference recover",
                        "reason": "run the canonical overlay recovery path in the current runtime"
                    },
                    {
                        "command": "rub explain interactability ...",
                        "reason": "confirm which target is blocked by the overlay before retrying"
                    }
                ]
            }),
            _ => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "An overlay-related readiness blocker is projected from the current runtime, so the next recovery step should stay in this same RUB_HOME/session.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "observation_runtime",
                        "summary": "Keep this runtime as the inspection surface while you confirm overlay-related readiness signals."
                    },
                    "recommended_runtime": {
                        "role": "observation_runtime",
                        "summary": "Stay in this runtime while you inspect readiness and choose the next recovery step."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub runtime readiness",
                        "reason": "inspect overlay_state and blocking_signals before retrying"
                    },
                    {
                        "command": "rub explain interactability ...",
                        "reason": "confirm which target is blocked by the overlay"
                    }
                ]
            }),
        },
        BlockerDiagnosisClass::RouteTransition => match primary_reason {
            "route_stability_transitioning" => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "The page is still changing routes in the current runtime. Keep follow-up waits and checks in this same RUB_HOME/session.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "observation_runtime",
                        "summary": "Keep this runtime as the observation surface while navigation settles."
                    },
                    "recommended_runtime": {
                        "role": "observation_runtime",
                        "summary": "Stay in this runtime while you wait for the route transition to become authoritative."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub wait --title-contains ...",
                        "reason": "wait for the destination page title to stabilize in the current runtime"
                    },
                    {
                        "command": "rub wait --url-contains ...",
                        "reason": "wait for the destination route to become authoritative once you know part of the target URL"
                    }
                ]
            }),
            "loading_present" => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "Loading blockers are still active in the current runtime. Wait for the post-load surface before continuing.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "observation_runtime",
                        "summary": "Keep this runtime as the observation surface while loading blockers clear."
                    },
                    "recommended_runtime": {
                        "role": "observation_runtime",
                        "summary": "Stay in this runtime while you watch for the stable post-load target."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub runtime readiness",
                        "reason": "watch loading_present and blocking_signals until the page settles"
                    },
                    {
                        "command": "rub wait --selector ... --state visible",
                        "reason": "wait for a known post-load target once you know which element should appear"
                    }
                ]
            }),
            "skeleton_present" => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "Skeleton placeholders are still active in the current runtime. Wait for the real interactive surface before continuing.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "observation_runtime",
                        "summary": "Keep this runtime as the observation surface while skeleton placeholders clear."
                    },
                    "recommended_runtime": {
                        "role": "observation_runtime",
                        "summary": "Stay in this runtime while you wait for the final interactive surface."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub runtime readiness",
                        "reason": "watch skeleton_present until the page exposes the final surface"
                    },
                    {
                        "command": "rub wait --selector ... --state interactable",
                        "reason": "wait for a known target to become interactable once the skeleton clears"
                    }
                ]
            }),
            _ => serde_json::json!({
                "signal": primary_reason,
                "continuation_kind": "same_runtime",
                "summary": "The page is still transitioning in the current runtime. Keep follow-up waits and checks in this same RUB_HOME/session.",
                "runtime_roles": {
                    "current_runtime": {
                        "role": "observation_runtime",
                        "summary": "Keep this runtime as the observation surface while the page settles."
                    },
                    "recommended_runtime": {
                        "role": "observation_runtime",
                        "summary": "Stay in this runtime while you confirm the next stable page state."
                    }
                },
                "recommended_runtime": {
                    "kind": "current_runtime",
                    "reason": "same_runtime_authoritative_followup",
                },
                "next_command_hints": [
                    {
                        "command": "rub state compact",
                        "reason": "summarize the current page after the transition settles"
                    },
                    {
                        "command": "rub runtime readiness",
                        "reason": "inspect route, loading, and skeleton signals before retrying"
                    }
                ]
            }),
        },
        BlockerDiagnosisClass::DegradedRuntime => serde_json::json!({
            "signal": primary_reason,
            "continuation_kind": "same_runtime",
            "summary": "Keep diagnosis in the current runtime, but treat blocker guidance as non-authoritative until degraded surfaces recover.",
            "runtime_roles": {
                "current_runtime": {
                    "role": "observation_runtime",
                    "summary": "Keep this runtime as the inspection surface while degraded signals recover."
                },
                "recommended_runtime": {
                    "role": "observation_runtime",
                    "summary": "Stay in this runtime while you inspect degraded surfaces."
                }
            },
            "recommended_runtime": {
                "kind": "current_runtime",
                "reason": "same_runtime_authoritative_followup",
            },
            "next_command_hints": [
                {
                    "command": "rub runtime doctor",
                    "reason": "inspect degraded runtime surfaces in the current session"
                },
                {
                    "command": "rub runtime readiness",
                    "reason": "re-check readiness once degraded surfaces recover"
                }
            ]
        }),
        BlockerDiagnosisClass::UnknownBlocker => serde_json::json!({
            "signal": primary_reason,
            "continuation_kind": "same_runtime",
            "summary": "The current runtime still owns the blocker state. Continue diagnosis here before branching elsewhere.",
            "runtime_roles": {
                "current_runtime": {
                    "role": "observation_runtime",
                    "summary": "Keep this runtime as the primary blocker-diagnosis surface."
                },
                "recommended_runtime": {
                    "role": "observation_runtime",
                    "summary": "Stay in this runtime while you refine the blocker diagnosis."
                }
            },
            "recommended_runtime": {
                "kind": "current_runtime",
                "reason": "same_runtime_authoritative_followup",
            },
            "next_command_hints": [
                {
                    "command": "rub runtime interference",
                    "reason": "inspect the classified interference in more detail"
                },
                {
                    "command": "rub explain interactability ...",
                    "reason": "switch to target-level diagnosis if one target is the real blocker"
                }
            ]
        }),
        BlockerDiagnosisClass::Clear => serde_json::json!({
            "signal": primary_reason,
            "continuation_kind": "same_runtime",
            "summary": "No dominant page-level blocker is projected right now. Continue in the current runtime.",
            "runtime_roles": {
                "current_runtime": {
                    "role": "active_execution_runtime",
                    "summary": "Keep this runtime as the primary execution surface."
                },
                "recommended_runtime": {
                    "role": "active_execution_runtime",
                    "summary": "Continue the workflow in this same runtime."
                }
            },
            "recommended_runtime": {
                "kind": "current_runtime",
                "reason": "same_runtime_authoritative_followup",
            },
            "next_command_hints": [
                {
                    "command": "rub state compact",
                    "reason": "inspect the current page state before the next action"
                },
                {
                    "command": "rub explain interactability ...",
                    "reason": "switch to target-level diagnosis if the failure is specific to one element"
                }
            ]
        }),
    }
}

fn blocker_diagnosis_readiness_status_name(
    status: rub_core::model::ReadinessStatus,
) -> &'static str {
    match status {
        rub_core::model::ReadinessStatus::Active => "active",
        rub_core::model::ReadinessStatus::Inactive => "inactive",
        rub_core::model::ReadinessStatus::Degraded => "degraded",
    }
}

fn blocker_diagnosis_interference_status_name(
    status: rub_core::model::InterferenceRuntimeStatus,
) -> &'static str {
    match status {
        rub_core::model::InterferenceRuntimeStatus::Active => "active",
        rub_core::model::InterferenceRuntimeStatus::Inactive => "inactive",
        rub_core::model::InterferenceRuntimeStatus::Degraded => "degraded",
    }
}

#[cfg(test)]
mod tests;
