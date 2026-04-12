use rub_core::model::CommandResult;
use serde_json::{Map, Value, json};
use std::path::Path;

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
    let subject_kind = data
        .get("subject")
        .and_then(Value::as_object)
        .and_then(|subject| subject.get("kind"))
        .and_then(Value::as_str);
    let outcome_summary = data
        .get("result")
        .and_then(Value::as_object)
        .and_then(|result| result.get("outcome_summary"))
        .and_then(Value::as_object);

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
            workflow_new_item_projection(command, session, rub_home, data)
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

fn workflow_new_item_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let result = data.get("result")?.as_object()?;
    let matched_item = result.get("matched_item").and_then(Value::as_object);
    let open_hint = matched_item.and_then(matched_item_open_command_hint);
    let text_hint = matched_item.and_then(matched_item_text_command_hint);
    let mut hints = vec![command_hint(
        "rub inspect list ...",
        "re-read the current list surface in the same runtime",
    )];
    if let Some(open_hint) = open_hint {
        hints.push(open_hint);
    } else if let Some(text_hint) = text_hint {
        hints.push(text_hint);
    } else {
        hints.push(command_hint(
            "rub click ...",
            "open or act on the newly observed item without switching runtimes",
        ));
    }

    Some(workflow_same_runtime_projection(
        command,
        session,
        rub_home,
        "confirmed_new_item_observed",
        "A new matching item was observed in this runtime. Keep follow-up inspection or actuation in the same RUB_HOME/session.",
        hints,
        same_runtime_roles(
            "observation_runtime",
            "Keep using the current runtime as the observation surface while you follow up on the newly observed item.",
        ),
    ))
}

fn workflow_follow_up_activity_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let result = data.get("result")?.as_object()?;
    let activity = result
        .get("outcome_summary")
        .and_then(Value::as_object)
        .and_then(|summary| summary.get("activity"))
        .and_then(Value::as_object);
    let last_request = activity
        .and_then(|activity| activity.get("last_request"))
        .and_then(Value::as_object);
    let exact_request_hint = last_request.and_then(follow_up_network_request_command_hint);
    let local_runtime_follow_up =
        last_request.is_some_and(|request| is_same_runtime_follow_up_request(data, request));
    let failed_request = last_request.is_some_and(is_failed_request);
    let downstream_effect_like = last_request.is_some_and(is_downstream_effect_like_request);
    let in_flight_write_like = last_request.is_some_and(is_in_flight_write_like_request);

    let mut hints = vec![command_hint(
        "rub state compact",
        "inspect the local page after the confirmed follow-up activity in the current runtime",
    )];
    if let Some(exact_request_hint) = exact_request_hint {
        hints.push(exact_request_hint);
    } else {
        hints.push(command_hint(
            "rub inspect network --last 5",
            "review the authoritative follow-up network activity that was observed after the action",
        ));
    }
    if local_runtime_follow_up {
        hints.push(command_hint(
            "rub state a11y",
            "re-check the current page's accessible text and field descriptions before branching to an external surface",
        ));
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still has a local blocker or validation message to resolve",
        ));
    } else if failed_request {
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the failed follow-up request corresponds to a local validation error, blocker, or route issue in the current runtime",
        ));
        hints.push(command_hint(
            "rub state a11y",
            "re-check the current page for local validation text, disabled controls, or route changes before assuming any downstream effect",
        ));
    } else if downstream_effect_like {
        hints.push(command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "verify any downstream side effect in the runtime that owns the relevant inbox, list, or result surface",
        ));
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still needs local recovery while the downstream surface is being verified",
        ));
    } else if in_flight_write_like {
        hints.push(command_hint(
            "rub inspect network --id ...",
            "re-check the same authoritative request after it reaches a terminal lifecycle before branching to downstream surfaces",
        ));
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still has a local blocker or validation state while the follow-up request is in flight",
        ));
    } else {
        hints.push(command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "continue downstream observation in the runtime that owns the relevant list or inbox surface",
        ));
    }

    Some(workflow_same_runtime_projection(
        command,
        session,
        rub_home,
        "confirmed_follow_up_activity",
        if local_runtime_follow_up {
            "The action produced authoritative same-runtime follow-up activity. Re-check the current page before branching to any external downstream surface."
        } else if failed_request {
            "The action produced authoritative failed follow-up activity in this runtime. Re-check the current page and the failed request before assuming any downstream effect."
        } else if downstream_effect_like {
            "The action produced authoritative write-like follow-up activity. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface."
        } else if in_flight_write_like {
            "The action produced authoritative in-flight write-like follow-up activity. Keep this runtime available while the request reaches a terminal lifecycle and local state settles."
        } else {
            "The action produced authoritative local follow-up activity in this runtime. Keep this runtime available while you verify any downstream effects."
        },
        hints,
        same_runtime_roles(
            "observation_runtime",
            "Keep using the current runtime as the local observation surface while you confirm any downstream effects.",
        ),
    ))
}

fn workflow_network_request_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
    include_request_lookup_hint: bool,
) -> Option<Value> {
    let result = data.get("result")?.as_object()?;
    let request = result.get("request").and_then(Value::as_object)?;
    let local_runtime_read_like = is_local_runtime_read_like_request(request);
    let failed_request = is_failed_request(request);
    let downstream_effect_like = is_downstream_effect_like_request(request);
    let in_flight_write_like = is_in_flight_write_like_request(request);
    let mut hints = Vec::new();
    if include_request_lookup_hint
        && let Some(request_hint) = follow_up_network_request_command_hint(request)
    {
        hints.push(request_hint);
    }
    hints.push(command_hint(
        "rub state compact",
        "inspect the current page alongside this authoritative network evidence in the same runtime",
    ));
    if local_runtime_read_like {
        hints.push(command_hint(
            "rub state a11y",
            "re-check the current page's accessible text and control descriptions before branching to any external downstream surface",
        ));
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still has a local blocker, validation message, or route transition to resolve",
        ));
    } else if failed_request {
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the failed request corresponds to a local validation error, blocker, or route issue in the current runtime",
        ));
        hints.push(command_hint(
            "rub state a11y",
            "re-check the current page for local validation text, disabled controls, or route changes before assuming any downstream effect",
        ));
    } else if downstream_effect_like {
        hints.push(command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "verify any downstream side effect in the runtime that owns the relevant inbox, list, or result surface",
        ));
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still needs local recovery while the downstream surface is being verified",
        ));
    } else if in_flight_write_like {
        hints.push(command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still has a local blocker or validation state while the follow-up request is in flight",
        ));
        hints.push(command_hint(
            "rub inspect network --id ...",
            "re-check the same authoritative request after it reaches a terminal lifecycle before branching to downstream surfaces",
        ));
    } else {
        hints.push(command_hint(
            "rub explain blockers",
            "check whether the current runtime still has a local blocker or transition to resolve",
        ));
        hints.push(command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "continue downstream observation in the runtime that owns the relevant list or inbox surface",
        ));
    }

    let request_method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("request");
    let request_url = request
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("the observed URL");
    let signal = if include_request_lookup_hint {
        "confirmed_terminal_request"
    } else {
        "network_request_record"
    };
    let summary = if local_runtime_read_like {
        format!(
            "The current runtime now has authoritative read-like network evidence for the observed {request_method} request to {request_url}. Re-check the current page before branching to any external downstream surface."
        )
    } else if failed_request {
        format!(
            "The current runtime now has authoritative failed network evidence for the observed {request_method} request to {request_url}. Re-check the current page and the failed request before assuming any downstream effect."
        )
    } else if downstream_effect_like {
        format!(
            "The current runtime now has authoritative write-like network evidence for the observed {request_method} request to {request_url}. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface."
        )
    } else if in_flight_write_like {
        format!(
            "The current runtime now has authoritative in-flight write-like network evidence for the observed {request_method} request to {request_url}. Keep this runtime available while the request reaches a terminal lifecycle and local state settles."
        )
    } else {
        format!(
            "The current runtime now has authoritative network evidence for the observed {request_method} request to {request_url}. Keep follow-up diagnosis and downstream checks anchored to this runtime."
        )
    };

    Some(workflow_same_runtime_projection(
        command,
        session,
        rub_home,
        signal,
        &summary,
        hints,
        same_runtime_roles(
            "observation_runtime",
            "Keep using the current runtime as the observation surface while you interpret authoritative network evidence and decide the next workflow step.",
        ),
    ))
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

fn matched_item_open_command_hint(matched_item: &Map<String, Value>) -> Option<Value> {
    for key in ["activation_url", "target_url", "href", "url", "link"] {
        let Some(value) = matched_item.get(key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        return Some(command_hint(
            &format!("rub open {}", shell_double_quoted(value)),
            &format!("continue directly from the extracted `{key}` field in the current runtime"),
        ));
    }
    None
}

fn matched_item_text_command_hint(matched_item: &Map<String, Value>) -> Option<Value> {
    for key in ["subject", "title", "name", "label", "text"] {
        let Some(value) = matched_item.get(key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        return Some(command_hint(
            &format!("rub click --target-text {}", shell_double_quoted(value)),
            &format!(
                "act on the newly observed item using the extracted `{key}` text anchor in the current runtime"
            ),
        ));
    }
    None
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
    workflow_continuity_base(
        command,
        session,
        rub_home,
        WorkflowContinuityDescriptor {
            continuation_kind: "same_runtime",
            signal,
            summary,
            next_command_hints,
            recommended_runtime: json!({
                "kind": "current_runtime",
                "rub_home": rub_home.display().to_string(),
                "session": session,
                "reason": "same_runtime_authoritative_followup",
            }),
            runtime_roles: Some(runtime_roles),
        },
    )
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
