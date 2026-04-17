use super::BlockerDiagnosisClass;

pub(super) fn blocker_diagnosis_workflow_guidance(
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
                    "command": "rub doctor",
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
