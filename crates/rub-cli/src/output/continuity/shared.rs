use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn network_evidence_observation(kind: &str, request: &Map<String, Value>) -> Value {
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

pub(super) fn network_registry_observation(
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

pub(super) fn follow_up_network_request_command_hint(
    last_request: &Map<String, Value>,
) -> Option<Value> {
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

pub(super) fn is_same_runtime_follow_up_request(
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

pub(super) fn is_local_runtime_read_like_request(request: &Map<String, Value>) -> bool {
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

pub(super) fn is_downstream_effect_like_request(request: &Map<String, Value>) -> bool {
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

pub(super) fn is_failed_request(request: &Map<String, Value>) -> bool {
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

pub(super) fn is_in_flight_write_like_request(request: &Map<String, Value>) -> bool {
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

pub(super) fn shell_double_quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

pub(super) fn workflow_same_runtime_projection(
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

pub(super) fn workflow_same_runtime_projection_with_observation(
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

pub(super) fn workflow_same_runtime_descriptor<'a>(
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

pub(super) fn workflow_fresh_home_projection(
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

pub(super) fn same_runtime_roles(role: &str, summary: &str) -> Value {
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

pub(super) struct WorkflowContinuityDescriptor<'a> {
    pub(super) continuation_kind: &'a str,
    pub(super) signal: &'a str,
    pub(super) summary: &'a str,
    pub(super) next_command_hints: Vec<Value>,
    pub(super) recommended_runtime: Value,
    pub(super) runtime_roles: Option<Value>,
    pub(super) authority_observation: Option<Value>,
}

pub(super) fn workflow_continuity_base(
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

pub(super) fn command_hint(command: &str, reason: &str) -> Value {
    json!({
        "command": command,
        "reason": reason,
    })
}
