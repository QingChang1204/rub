use serde_json::{Map, Value};
use std::path::Path;

pub(super) fn workflow_follow_up_activity_projection(
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
    let exact_request_hint = last_request.and_then(super::follow_up_network_request_command_hint);
    let local_runtime_follow_up =
        last_request.is_some_and(|request| super::is_same_runtime_follow_up_request(data, request));
    let failed_request = last_request.is_some_and(super::is_failed_request);
    let downstream_effect_like = last_request.is_some_and(super::is_downstream_effect_like_request);
    let in_flight_write_like = last_request.is_some_and(super::is_in_flight_write_like_request);
    let read_like_observation = last_request.map(|request| {
        super::network_evidence_observation("same_runtime_read_like_follow_up", request)
    });

    let mut hints = vec![super::command_hint(
        "rub state compact",
        "inspect the local page after the confirmed follow-up activity in the current runtime",
    )];
    if let Some(exact_request_hint) = exact_request_hint {
        hints.push(exact_request_hint);
    } else {
        hints.push(super::command_hint(
            "rub inspect network --last 5",
            "review the authoritative follow-up network activity that was observed after the action",
        ));
    }
    if local_runtime_follow_up {
        hints.push(super::command_hint(
            "rub find --content ...",
            "pivot into content-anchor discovery now that the current runtime has authoritative read-like evidence",
        ));
        hints.push(super::command_hint(
            "rub extract ...",
            "promote the now-confirmed content surface into structured fields or repeated records without switching runtimes",
        ));
    } else if failed_request {
        hints.push(super::command_hint(
            "rub explain blockers",
            "confirm whether the failed follow-up request corresponds to a local validation error, blocker, or route issue in the current runtime",
        ));
        hints.push(super::command_hint(
            "rub state a11y",
            "re-check the current page for local validation text, disabled controls, or route changes before assuming any downstream effect",
        ));
    } else if downstream_effect_like {
        hints.push(super::command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "verify any downstream side effect in the runtime that owns the relevant inbox, list, or result surface",
        ));
        hints.push(super::command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still needs local recovery while the downstream surface is being verified",
        ));
    } else if in_flight_write_like {
        hints.push(super::command_hint(
            "rub inspect network --id ...",
            "re-check the same authoritative request after it reaches a terminal lifecycle before branching to downstream surfaces",
        ));
        hints.push(super::command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still has a local blocker or validation state while the follow-up request is in flight",
        ));
    } else {
        hints.push(super::command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "continue downstream observation in the runtime that owns the relevant list or inbox surface",
        ));
    }

    let summary = if local_runtime_follow_up {
        "The action produced authoritative same-runtime read-like follow-up activity. Re-check content/read surfaces in the current runtime before branching to any external downstream surface."
    } else if failed_request {
        "The action produced authoritative failed follow-up activity in this runtime. Re-check the current page and the failed request before assuming any downstream effect."
    } else if downstream_effect_like {
        "The action produced authoritative write-like follow-up activity. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface."
    } else if in_flight_write_like {
        "The action produced authoritative in-flight write-like follow-up activity. Keep this runtime available while the request reaches a terminal lifecycle and local state settles."
    } else {
        "The action produced authoritative local follow-up activity in this runtime. Keep this runtime available while you verify any downstream effects."
    };

    if local_runtime_follow_up {
        Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "confirmed_follow_up_activity",
                summary,
                hints,
                super::same_runtime_roles(
                    "content_runtime",
                    "Keep using the current runtime as the local content/read surface while you confirm what the read-like evidence changed on the page.",
                ),
                read_like_observation,
            ),
        ))
    } else {
        Some(super::workflow_same_runtime_projection(
            command,
            session,
            rub_home,
            "confirmed_follow_up_activity",
            summary,
            hints,
            super::same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the local observation surface while you confirm any downstream effects.",
            ),
        ))
    }
}

pub(super) fn workflow_network_request_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
    include_request_lookup_hint: bool,
) -> Option<Value> {
    let result = data.get("result")?.as_object()?;
    let request = result.get("request").and_then(Value::as_object)?;
    let local_runtime_read_like = super::is_local_runtime_read_like_request(request);
    let failed_request = super::is_failed_request(request);
    let downstream_effect_like = super::is_downstream_effect_like_request(request);
    let in_flight_write_like = super::is_in_flight_write_like_request(request);
    let read_like_observation =
        super::network_evidence_observation("read_like_network_request", request);
    let mut hints = Vec::new();
    if include_request_lookup_hint
        && let Some(request_hint) = super::follow_up_network_request_command_hint(request)
    {
        hints.push(request_hint);
    }
    hints.push(super::command_hint(
        "rub state compact",
        "inspect the current page alongside this authoritative network evidence in the same runtime",
    ));
    if local_runtime_read_like {
        hints.push(super::command_hint(
            "rub find --content ...",
            "pivot into content-anchor discovery now that the current runtime has authoritative read-like evidence",
        ));
        hints.push(super::command_hint(
            "rub extract ...",
            "promote the content surface into structured fields or repeated records without leaving the current runtime",
        ));
    } else if failed_request {
        hints.push(super::command_hint(
            "rub explain blockers",
            "confirm whether the failed request corresponds to a local validation error, blocker, or route issue in the current runtime",
        ));
        hints.push(super::command_hint(
            "rub state a11y",
            "re-check the current page for local validation text, disabled controls, or route changes before assuming any downstream effect",
        ));
    } else if downstream_effect_like {
        hints.push(super::command_hint(
            "rub inspect list ... --wait-field ... --wait-contains ...",
            "verify any downstream side effect in the runtime that owns the relevant inbox, list, or result surface",
        ));
        hints.push(super::command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still needs local recovery while the downstream surface is being verified",
        ));
    } else if in_flight_write_like {
        hints.push(super::command_hint(
            "rub explain blockers",
            "confirm whether the current runtime still has a local blocker or validation state while the follow-up request is in flight",
        ));
        hints.push(super::command_hint(
            "rub inspect network --id ...",
            "re-check the same authoritative request after it reaches a terminal lifecycle before branching to downstream surfaces",
        ));
    } else {
        hints.push(super::command_hint(
            "rub explain blockers",
            "check whether the current runtime still has a local blocker or transition to resolve",
        ));
        hints.push(super::command_hint(
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
            "The current runtime now has authoritative read-like network evidence for the observed {request_method} request to {request_url}. Re-check content/read surfaces in the current runtime before branching to any external downstream surface."
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

    if local_runtime_read_like {
        Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                signal,
                &summary,
                hints,
                super::same_runtime_roles(
                    "content_runtime",
                    "Keep using the current runtime as the content/read surface while you map read-like network evidence back onto page content.",
                ),
                Some(read_like_observation),
            ),
        ))
    } else {
        Some(super::workflow_same_runtime_projection(
            command,
            session,
            rub_home,
            signal,
            &summary,
            hints,
            super::same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the observation surface while you interpret authoritative network evidence and decide the next workflow step.",
            ),
        ))
    }
}

pub(super) fn workflow_network_request_registry_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let result = data.get("result")?.as_object()?;
    let items = result.get("items")?.as_array()?;
    if items.is_empty() {
        return None;
    }

    let mut total = 0usize;
    let mut read_like = 0usize;
    let mut write_like = 0usize;
    let mut failed = 0usize;
    let mut in_flight_write_like = 0usize;
    let mut other = 0usize;

    for item in items {
        let Some(request) = item.as_object() else {
            continue;
        };
        total += 1;
        if super::is_local_runtime_read_like_request(request) {
            read_like += 1;
        } else if super::is_failed_request(request) {
            failed += 1;
        } else if super::is_downstream_effect_like_request(request) {
            write_like += 1;
        } else if super::is_in_flight_write_like_request(request) {
            in_flight_write_like += 1;
        } else {
            other += 1;
        }
    }

    if total == 0 {
        return None;
    }

    let observation = if read_like > 0
        && write_like == 0
        && failed == 0
        && in_flight_write_like == 0
        && other == 0
    {
        super::network_registry_observation(
            "read_like_network_registry",
            total,
            read_like,
            write_like,
            failed,
            in_flight_write_like,
            other,
        )
    } else if write_like > 0
        && read_like == 0
        && failed == 0
        && in_flight_write_like == 0
        && other == 0
    {
        super::network_registry_observation(
            "write_like_network_registry",
            total,
            read_like,
            write_like,
            failed,
            in_flight_write_like,
            other,
        )
    } else if failed > 0
        && read_like == 0
        && write_like == 0
        && in_flight_write_like == 0
        && other == 0
    {
        super::network_registry_observation(
            "failed_network_registry",
            total,
            read_like,
            write_like,
            failed,
            in_flight_write_like,
            other,
        )
    } else if in_flight_write_like > 0
        && read_like == 0
        && write_like == 0
        && failed == 0
        && other == 0
    {
        super::network_registry_observation(
            "in_flight_write_network_registry",
            total,
            read_like,
            write_like,
            failed,
            in_flight_write_like,
            other,
        )
    } else {
        super::network_registry_observation(
            "mixed_network_registry",
            total,
            read_like,
            write_like,
            failed,
            in_flight_write_like,
            other,
        )
    };

    let (signal, summary, hints, runtime_roles) = if observation["evidence_kind"].as_str()
        == Some("read_like_network_registry")
    {
        (
            "network_request_registry",
            "The current runtime now has authoritative read-like network registry evidence. Re-check content/read surfaces in the current runtime before branching to any external downstream surface.",
            vec![
                super::command_hint(
                    "rub state compact",
                    "inspect the current page alongside the authoritative read-like registry evidence in the same runtime",
                ),
                super::command_hint(
                    "rub find --content ...",
                    "pivot into content-anchor discovery now that the current runtime has authoritative read-like evidence across the matched request set",
                ),
                super::command_hint(
                    "rub extract ...",
                    "promote the content surface into structured fields or repeated records without leaving the current runtime",
                ),
            ],
            super::same_runtime_roles(
                "content_runtime",
                "Keep using the current runtime as the content/read surface while you map read-like registry evidence back onto page content.",
            ),
        )
    } else if observation["evidence_kind"].as_str() == Some("write_like_network_registry") {
        (
            "network_request_registry",
            "The current runtime now has authoritative write-like network registry evidence. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface.",
            vec![
                super::command_hint(
                    "rub state compact",
                    "inspect the current page while you keep the authoritative write-like registry evidence anchored to this runtime",
                ),
                super::command_hint(
                    "rub inspect list ... --wait-field ... --wait-contains ...",
                    "verify any downstream side effect in the runtime that owns the relevant inbox, list, or result surface",
                ),
                super::command_hint(
                    "rub explain blockers",
                    "confirm whether the current runtime still needs local recovery while the downstream surface is being verified",
                ),
            ],
            super::same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the observation surface while you interpret authoritative write-like registry evidence and confirm downstream effects.",
            ),
        )
    } else if observation["evidence_kind"].as_str() == Some("failed_network_registry") {
        (
            "network_request_registry",
            "The current runtime now has authoritative failed network registry evidence. Re-check the current page and the failed request set before assuming any downstream effect.",
            vec![
                super::command_hint(
                    "rub state compact",
                    "inspect the current page before treating the failed registry evidence as a downstream workflow result",
                ),
                super::command_hint(
                    "rub explain blockers",
                    "confirm whether the failed requests correspond to local validation errors, blockers, or route issues in the current runtime",
                ),
                super::command_hint(
                    "rub state a11y",
                    "re-check the current page for validation text, disabled controls, or route changes before attempting recovery",
                ),
            ],
            super::same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the local recovery surface while you interpret authoritative failed registry evidence.",
            ),
        )
    } else if observation["evidence_kind"].as_str() == Some("in_flight_write_network_registry") {
        (
            "network_request_registry",
            "The current runtime now has authoritative in-flight write-like network registry evidence. Keep this runtime available while the request set reaches a terminal lifecycle and local state settles.",
            vec![
                super::command_hint(
                    "rub state compact",
                    "inspect the current page while the authoritative in-flight registry evidence settles in the same runtime",
                ),
                super::command_hint(
                    "rub inspect network --id ...",
                    "re-check a specific in-flight authoritative request after it reaches a terminal lifecycle before branching to downstream surfaces",
                ),
                super::command_hint(
                    "rub explain blockers",
                    "confirm whether the current runtime still has a local blocker or validation state while the request set is in flight",
                ),
            ],
            super::same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the observation surface while authoritative in-flight registry evidence settles.",
            ),
        )
    } else {
        (
            "network_request_registry",
            "The current runtime now has authoritative mixed network evidence. Re-check local content/read surfaces first, then verify any downstream side effect or local blocker before branching away from this runtime.",
            vec![
                super::command_hint(
                    "rub state compact",
                    "inspect the current page while keeping the mixed authoritative network evidence anchored to this runtime",
                ),
                super::command_hint(
                    "rub find --content ...",
                    "check whether the read-like portion of the registry evidence has already changed page content in the current runtime",
                ),
                super::command_hint(
                    "rub extract ...",
                    "promote any newly confirmed content surface into structured fields before switching to downstream verification",
                ),
                super::command_hint(
                    "rub inspect list ... --wait-field ... --wait-contains ...",
                    "verify any downstream side effect implied by the write-like portion of the authoritative request set",
                ),
                super::command_hint(
                    "rub explain blockers",
                    "confirm whether any failed or in-flight requests still correspond to a local blocker or validation issue in the current runtime",
                ),
            ],
            super::same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the observation surface while you separate local content effects from downstream side effects across the mixed request set.",
            ),
        )
    };

    Some(super::workflow_same_runtime_projection_with_observation(
        command,
        session,
        rub_home,
        super::workflow_same_runtime_descriptor(
            signal,
            summary,
            hints,
            runtime_roles,
            Some(observation),
        ),
    ))
}
