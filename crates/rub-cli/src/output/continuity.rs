use rub_core::model::CommandResult;
use serde_json::{Map, Value, json};
use std::path::Path;

mod authority;
mod content;
mod network;

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
                blocker_workflow_continuity_projection(command, session, rub_home, guidance)
            })
        {
            return Some(guidance);
        }
        let class = diagnosis
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Some(match class {
            "provider_gate" => workflow_fresh_home_projection(
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
                        "rub runtime doctor",
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

fn network_evidence_observation(kind: &str, request: &Map<String, Value>) -> Value {
    json!({
        "kind": "network_evidence_observation",
        "evidence_kind": kind,
        "request_id": request.get("request_id").cloned().unwrap_or(Value::Null),
        "method": request.get("method").cloned().unwrap_or(Value::Null),
        "url": request.get("url").cloned().unwrap_or(Value::Null),
        "status": request.get("status").cloned().unwrap_or(Value::Null),
        "lifecycle": request.get("lifecycle").cloned().unwrap_or(Value::Null),
        "resource_type": request.get("resource_type").cloned().unwrap_or(Value::Null),
    })
}

fn network_registry_observation(
    kind: &str,
    total_requests: usize,
    read_like_requests: usize,
    write_like_requests: usize,
    failed_requests: usize,
    in_flight_write_like_requests: usize,
    other_requests: usize,
) -> Value {
    json!({
        "kind": "network_registry_observation",
        "evidence_kind": kind,
        "total_requests": total_requests,
        "read_like_requests": read_like_requests,
        "write_like_requests": write_like_requests,
        "failed_requests": failed_requests,
        "in_flight_write_like_requests": in_flight_write_like_requests,
        "other_requests": other_requests,
    })
}

fn follow_up_network_request_command_hint(last_request: &Map<String, Value>) -> Option<Value> {
    let request_id = last_request.get("request_id").and_then(Value::as_str)?;
    if request_id.trim().is_empty() {
        return None;
    }
    let method = last_request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("request");
    let url = last_request
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("follow-up request");
    Some(command_hint(
        &format!(
            "rub inspect network --id {}",
            shell_double_quoted(request_id)
        ),
        &format!("inspect the exact authoritative follow-up {method} request observed for {url}"),
    ))
}

fn is_same_runtime_follow_up_request(
    data: &Map<String, Value>,
    last_request: &Map<String, Value>,
) -> bool {
    let request_url = last_request
        .get("url")
        .and_then(Value::as_str)
        .and_then(parse_origin_url);
    let frame_url = data
        .get("interaction")
        .and_then(Value::as_object)
        .and_then(|interaction| interaction.get("frame_context"))
        .and_then(Value::as_object)
        .and_then(|frame| frame.get("url"))
        .and_then(Value::as_str)
        .and_then(parse_origin_url);
    let same_origin =
        request_url
            .as_ref()
            .zip(frame_url.as_ref())
            .is_some_and(|(request_url, frame_url)| {
                request_url.scheme() == frame_url.scheme()
                    && request_url.domain() == frame_url.domain()
                    && request_url.port_or_known_default() == frame_url.port_or_known_default()
            });
    let method = last_request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let resource_type = last_request
        .get("resource_type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    same_origin
        && method.eq_ignore_ascii_case("GET")
        && (resource_type.eq_ignore_ascii_case("xhr")
            || resource_type.eq_ignore_ascii_case("fetch"))
}

fn is_local_runtime_read_like_request(request: &Map<String, Value>) -> bool {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let resource_type = request
        .get("resource_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    method.eq_ignore_ascii_case("GET")
        && (resource_type.eq_ignore_ascii_case("xhr")
            || resource_type.eq_ignore_ascii_case("fetch"))
}

fn is_downstream_effect_like_request(request: &Map<String, Value>) -> bool {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.eq_ignore_ascii_case("GET")
        || method.eq_ignore_ascii_case("HEAD")
        || method.eq_ignore_ascii_case("OPTIONS")
    {
        return false;
    }

    let lifecycle = request
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !lifecycle.eq_ignore_ascii_case("completed") {
        return false;
    }

    let status = request
        .get("status")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    (200..400).contains(&status)
}

fn is_failed_request(request: &Map<String, Value>) -> bool {
    let lifecycle = request
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if lifecycle.eq_ignore_ascii_case("failed") {
        return true;
    }
    request
        .get("status")
        .and_then(Value::as_u64)
        .is_some_and(|status| status >= 400)
}

fn is_in_flight_write_like_request(request: &Map<String, Value>) -> bool {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.eq_ignore_ascii_case("GET")
        || method.eq_ignore_ascii_case("HEAD")
        || method.eq_ignore_ascii_case("OPTIONS")
    {
        return false;
    }
    !is_failed_request(request) && !is_downstream_effect_like_request(request)
}

fn parse_origin_url(url: &str) -> Option<reqwest::Url> {
    reqwest::Url::parse(url).ok()
}

fn shell_double_quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

fn blocker_workflow_continuity_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    guidance: &Map<String, Value>,
) -> Option<Value> {
    let continuation_kind = guidance.get("continuation_kind")?.as_str()?;
    let summary = guidance.get("summary")?.as_str()?;
    let signal = guidance
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or(continuation_kind);
    let next_command_hints = guidance
        .get("next_command_hints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let runtime_roles = guidance
        .get("runtime_roles")
        .cloned()
        .unwrap_or_else(|| blocker_workflow_runtime_roles(continuation_kind, signal));
    let recommended_runtime = match guidance.get("recommended_runtime")?.as_object() {
        Some(runtime) => {
            let kind = runtime.get("kind").and_then(Value::as_str)?;
            match kind {
                "fresh_rub_home" => json!({
                    "kind": "fresh_rub_home",
                    "rub_home_hint": runtime
                        .get("rub_home_hint")
                        .and_then(Value::as_str)
                        .unwrap_or("<fresh RUB_HOME>"),
                    "session": runtime.get("session").and_then(Value::as_str).unwrap_or("default"),
                    "reason": runtime
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("isolated_runtime_recommended"),
                }),
                "current_runtime" => json!({
                    "kind": "current_runtime",
                    "rub_home": rub_home.display().to_string(),
                    "session": session,
                    "reason": runtime
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("same_runtime_authoritative_followup"),
                }),
                _ => return None,
            }
        }
        None => return None,
    };

    Some(workflow_continuity_base(
        command,
        session,
        rub_home,
        WorkflowContinuityDescriptor {
            continuation_kind,
            signal,
            summary,
            next_command_hints,
            recommended_runtime,
            runtime_roles: Some(runtime_roles),
            authority_observation: None,
        },
    ))
}

fn workflow_same_runtime_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    signal: &str,
    summary: &str,
    next_command_hints: Vec<Value>,
    runtime_roles: Value,
) -> Value {
    workflow_same_runtime_projection_with_observation(
        command,
        session,
        rub_home,
        workflow_same_runtime_descriptor(signal, summary, next_command_hints, runtime_roles, None),
    )
}

fn workflow_same_runtime_projection_with_observation(
    command: &str,
    session: &str,
    rub_home: &Path,
    mut descriptor: WorkflowContinuityDescriptor<'_>,
) -> Value {
    descriptor.recommended_runtime = json!({
        "kind": "current_runtime",
        "rub_home": rub_home.display().to_string(),
        "session": session,
        "reason": "same_runtime_authoritative_followup",
    });
    workflow_continuity_base(command, session, rub_home, descriptor)
}

fn workflow_same_runtime_descriptor<'a>(
    signal: &'a str,
    summary: &'a str,
    next_command_hints: Vec<Value>,
    runtime_roles: Value,
    authority_observation: Option<Value>,
) -> WorkflowContinuityDescriptor<'a> {
    WorkflowContinuityDescriptor {
        continuation_kind: "same_runtime",
        signal,
        summary,
        next_command_hints,
        recommended_runtime: Value::Null,
        runtime_roles: Some(runtime_roles),
        authority_observation,
    }
}

fn workflow_fresh_home_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    signal: &str,
    summary: &str,
    next_command_hints: Vec<Value>,
) -> Value {
    workflow_continuity_base(
        command,
        session,
        rub_home,
        WorkflowContinuityDescriptor {
            continuation_kind: "fresh_rub_home",
            signal,
            summary,
            next_command_hints,
            recommended_runtime: json!({
                "kind": "fresh_rub_home",
                "rub_home_hint": "<fresh RUB_HOME>",
                "session": "default",
                "reason": "isolated_runtime_recommended",
            }),
            runtime_roles: Some(json!({
                "current_runtime": {
                    "role": "gated_or_inspection_runtime",
                    "summary": "Keep the current runtime for inspection or recovery while the fresh RUB_HOME continues the broader workflow."
                },
                "recommended_runtime": {
                    "role": "continuation_runtime",
                    "summary": "Use the fresh RUB_HOME as the primary continuation path for the next workflow step."
                }
            })),
            authority_observation: None,
        },
    )
}

fn same_runtime_roles(role: &str, summary: &str) -> Value {
    json!({
        "current_runtime": {
            "role": role,
            "summary": summary,
        },
        "recommended_runtime": {
            "role": role,
            "summary": summary,
        }
    })
}

fn blocker_workflow_runtime_roles(continuation_kind: &str, signal: &str) -> Value {
    match continuation_kind {
        "fresh_rub_home" => json!({
            "current_runtime": {
                "role": "gated_or_inspection_runtime",
                "summary": "Keep the current runtime for inspection or recovery while the fresh RUB_HOME continues the broader workflow."
            },
            "recommended_runtime": {
                "role": "continuation_runtime",
                "summary": "Use the fresh RUB_HOME as the primary continuation path for the next workflow step."
            }
        }),
        "same_runtime" => match signal {
            "handoff_active" | "overlay_interference" => same_runtime_roles(
                "manual_recovery_runtime",
                "Keep using the current runtime as the recovery surface while you clear the blocker.",
            ),
            "route_stability_transitioning"
            | "loading_present"
            | "skeleton_present"
            | "degraded_runtime" => same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the observation surface while you verify blocker recovery.",
            ),
            _ => same_runtime_roles(
                "active_execution_runtime",
                "Keep using the current runtime as the primary workflow continuation surface.",
            ),
        },
        _ => same_runtime_roles(
            "active_execution_runtime",
            "Keep using the current runtime as the primary workflow continuation surface.",
        ),
    }
}

struct WorkflowContinuityDescriptor<'a> {
    continuation_kind: &'a str,
    signal: &'a str,
    summary: &'a str,
    next_command_hints: Vec<Value>,
    recommended_runtime: Value,
    runtime_roles: Option<Value>,
    authority_observation: Option<Value>,
}

fn workflow_continuity_base(
    command: &str,
    session: &str,
    rub_home: &Path,
    descriptor: WorkflowContinuityDescriptor<'_>,
) -> Value {
    let mut payload = json!({
        "surface": "cli_workflow_continuity",
        "truth_level": "operator_projection",
        "projection_kind": "workflow_continuity",
        "projection_authority": "cli.output.workflow_continuity",
        "upstream_commit_truth": "command_result_data",
        "control_role": "guidance_only",
        "durability": "ephemeral",
        "continuation_kind": descriptor.continuation_kind,
        "source_command": command,
        "source_signal": descriptor.signal,
        "summary": descriptor.summary,
        "current_runtime": {
            "rub_home": rub_home.display().to_string(),
            "session": session,
        },
        "recommended_runtime": descriptor.recommended_runtime,
        "next_command_hints": descriptor.next_command_hints,
    });
    if let Some(runtime_roles) = descriptor.runtime_roles
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("runtime_roles".to_string(), runtime_roles);
    }
    if let Some(authority_observation) = descriptor.authority_observation
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("authority_observation".to_string(), authority_observation);
    }
    payload
}

fn command_hint(command: &str, reason: &str) -> Value {
    json!({
        "command": command,
        "reason": reason,
    })
}

#[cfg(test)]
mod tests;
