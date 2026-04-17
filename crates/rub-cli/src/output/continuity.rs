use rub_core::model::CommandResult;
use serde_json::{Map, Value};
use std::path::Path;

mod authority;
mod blocker;
mod content;
mod network;
mod shared;

use self::shared::{
    command_hint, follow_up_network_request_command_hint, is_downstream_effect_like_request,
    is_failed_request, is_in_flight_write_like_request, is_local_runtime_read_like_request,
    is_same_runtime_follow_up_request, network_evidence_observation, network_registry_observation,
    same_runtime_roles, shell_double_quoted, workflow_same_runtime_descriptor,
    workflow_same_runtime_projection, workflow_same_runtime_projection_with_observation,
};

pub(super) fn attach_workflow_continuity(result: &mut CommandResult, rub_home: &Path) {
    let Some(data) = result.data.as_mut() else {
        return;
    };
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(projection) =
        workflow_continuity_projection(&result.command, &result.session, rub_home, object)
    else {
        return;
    };
    object.insert("workflow_continuity".to_string(), projection);
}

fn workflow_continuity_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let subject = data.get("subject").and_then(Value::as_object);
    let subject_kind = subject
        .and_then(|subject| subject.get("kind"))
        .and_then(Value::as_str);
    let outcome_summary = data
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.get("outcome_summary"))
        .and_then(Value::as_object);

    if subject_kind == Some("tab_navigation")
        && let Some(guidance) =
            workflow_navigation_authority_projection(command, session, rub_home, data)
    {
        return Some(guidance);
    }
    if subject_kind == Some("runtime_surface")
        && subject
            .and_then(|subject| subject.get("surface"))
            .and_then(Value::as_str)
            == Some("frame")
        && let Some(guidance) = workflow_runtime_frame_projection(command, session, rub_home, data)
    {
        return Some(guidance);
    }
    if subject_kind == Some("runtime_surface")
        && subject
            .and_then(|subject| subject.get("surface"))
            .and_then(Value::as_str)
            == Some("interference")
        && let Some(guidance) =
            workflow_runtime_interference_projection(command, session, rub_home, data)
    {
        return Some(guidance);
    }
    if subject_kind == Some("find_query")
        && let Some(guidance) =
            content::workflow_find_query_projection(command, session, rub_home, data)
    {
        return Some(guidance);
    }

    if subject_kind == Some("blocker_explain") {
        let diagnosis = data
            .get("result")
            .and_then(Value::as_object)
            .and_then(|result| result.get("diagnosis"))
            .and_then(Value::as_object)?;
        if let Some(guidance) = diagnosis
            .get("workflow_guidance")
            .and_then(Value::as_object)
            .and_then(|guidance| {
                blocker::blocker_workflow_continuity_projection(
                    command, session, rub_home, guidance,
                )
            })
        {
            return Some(guidance);
        }
        let class = diagnosis
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Some(match class {
            "provider_gate" => shared::workflow_fresh_home_projection(
                command,
                session,
                rub_home,
                "provider_gate",
                "This runtime is currently gated by provider or verification friction. Keep it for handoff or inspection, and continue alternate-provider work in a fresh RUB_HOME.",
                vec![
                    command_hint(
                        "rub handoff start",
                        "pause automation here and move the gated page into manual recovery",
                    ),
                    command_hint(
                        "rub --rub-home <fresh-home> open <alternate provider url>",
                        "continue the broader workflow in a separate isolated runtime",
                    ),
                ],
            ),
            "overlay_blocker" => workflow_same_runtime_projection(
                command,
                session,
                rub_home,
                "overlay_blocker",
                "The blocker is projected from the current runtime, so the next recovery step should stay in this same RUB_HOME/session.",
                vec![
                    command_hint(
                        "rub explain interactability ...",
                        "confirm which target is blocked by the overlay",
                    ),
                    command_hint(
                        "rub click ...",
                        "dismiss the blocking overlay in the current runtime if a safe target is known",
                    ),
                ],
                same_runtime_roles(
                    "manual_recovery_runtime",
                    "Keep using the current runtime as the recovery surface while you clear the blocker.",
                ),
            ),
            "route_transition" => workflow_same_runtime_projection(
                command,
                session,
                rub_home,
                "route_transition",
                "The page is still transitioning in the current runtime. Keep follow-up waits and checks in this same RUB_HOME/session.",
                vec![
                    command_hint(
                        "rub wait --title-contains ...",
                        "wait for the destination page title to stabilize in the current runtime",
                    ),
                    command_hint(
                        "rub state compact",
                        "summarize the current page after the transition settles",
                    ),
                ],
                same_runtime_roles(
                    "observation_runtime",
                    "Keep using the current runtime as the observation surface while the page settles.",
                ),
            ),
            "degraded_runtime" => workflow_same_runtime_projection(
                command,
                session,
                rub_home,
                "degraded_runtime",
                "Runtime surfaces are degraded, but the current RUB_HOME/session is still the authoritative place to inspect and recover this workflow.",
                vec![
                    command_hint(
                        "rub doctor",
                        "inspect the degraded runtime surfaces in the same session",
                    ),
                    command_hint(
                        "rub handoff start",
                        "switch to human recovery in the current runtime if automation cannot continue safely",
                    ),
                ],
                same_runtime_roles(
                    "observation_runtime",
                    "Keep using the current runtime as the inspection surface while you diagnose degraded signals.",
                ),
            ),
            _ => workflow_same_runtime_projection(
                command,
                session,
                rub_home,
                class,
                "The current RUB_HOME/session still owns the blocker state. Continue diagnosis and recovery here before branching elsewhere.",
                vec![
                    command_hint(
                        "rub explain blockers",
                        "re-check the dominant blocker after you make a recovery attempt",
                    ),
                    command_hint(
                        "rub state compact",
                        "summarize the page in the current runtime before the next action",
                    ),
                ],
                same_runtime_roles(
                    "active_execution_runtime",
                    "Keep using the current runtime as the primary workflow continuation surface.",
                ),
            ),
        });
    }
    if subject_kind == Some("network_request") {
        return workflow_network_request_projection(command, session, rub_home, data, false);
    }
    if subject_kind == Some("network_request_registry")
        && let Some(guidance) =
            workflow_network_request_registry_projection(command, session, rub_home, data)
    {
        return Some(guidance);
    }

    let class = outcome_summary
        .and_then(|summary| summary.get("class"))
        .and_then(Value::as_str)?;
    match class {
        "confirmed_context_transition" => Some(workflow_same_runtime_projection(
            command,
            session,
            rub_home,
            "confirmed_context_transition",
            "The observed context transition was confirmed in this runtime. Continue the next step in the same RUB_HOME/session.",
            vec![
                command_hint(
                    "rub state compact",
                    "inspect the new page state in the same runtime",
                ),
                command_hint(
                    "rub explain blockers",
                    "check whether the destination page is still blocked before acting",
                ),
            ],
            same_runtime_roles(
                "active_execution_runtime",
                "Keep using the current runtime as the primary execution surface after the confirmed context transition.",
            ),
        )),
        "confirmed_new_item_observed" => {
            content::workflow_new_item_projection(command, session, rub_home, data)
        }
        "confirmed_interactable_target" => Some(workflow_same_runtime_projection(
            command,
            session,
            rub_home,
            "confirmed_interactable_target",
            "The target is now interactable in this runtime. Keep the next action in the same RUB_HOME/session.",
            vec![
                command_hint(
                    "rub click ...",
                    "act on the confirmed interactable target in the current runtime",
                ),
                command_hint(
                    "rub type ...",
                    "write into the now-interactable target without switching runtimes",
                ),
            ],
            same_runtime_roles(
                "active_execution_runtime",
                "Keep using the current runtime as the primary execution surface now that the target is interactable.",
            ),
        )),
        "confirmed_target_description" => Some(workflow_same_runtime_projection(
            command,
            session,
            rub_home,
            "confirmed_target_description",
            "The target's descriptive status text matched in this runtime. Keep the next step in the same RUB_HOME/session.",
            vec![
                command_hint(
                    "rub state a11y",
                    "inspect the labeled control and its current descriptive status in the same runtime",
                ),
                command_hint(
                    "rub click ...",
                    "continue with the next action on the same confirmed target in this runtime",
                ),
            ],
            same_runtime_roles(
                "active_execution_runtime",
                "Keep using the current runtime as the primary execution surface now that the target's descriptive state is confirmed.",
            ),
        )),
        "confirmed_follow_up_activity" => {
            workflow_follow_up_activity_projection(command, session, rub_home, data)
        }
        "confirmed_terminal_request" => {
            workflow_network_request_projection(command, session, rub_home, data, true)
        }
        _ => None,
    }
}

fn workflow_navigation_authority_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    authority::workflow_navigation_authority_projection(command, session, rub_home, data)
}

fn workflow_runtime_interference_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    authority::workflow_runtime_interference_projection(command, session, rub_home, data)
}

fn workflow_runtime_frame_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    authority::workflow_runtime_frame_projection(command, session, rub_home, data)
}

fn workflow_follow_up_activity_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    network::workflow_follow_up_activity_projection(command, session, rub_home, data)
}

fn workflow_network_request_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
    include_request_lookup_hint: bool,
) -> Option<Value> {
    network::workflow_network_request_projection(
        command,
        session,
        rub_home,
        data,
        include_request_lookup_hint,
    )
}

fn workflow_network_request_registry_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    network::workflow_network_request_registry_projection(command, session, rub_home, data)
}

#[cfg(test)]
mod tests;
