use rub_core::error::{ErrorCode, RubError};
use rub_core::model::NetworkRequestRecord;

use super::args::NetworkRequestWaitState;

#[derive(Debug)]
pub(super) struct CurlExport {
    pub(super) command: String,
    pub(super) body_complete: bool,
    pub(super) body_omitted_reason: Option<String>,
}

pub(super) fn summarize_request_record(mut request: NetworkRequestRecord) -> NetworkRequestRecord {
    request.request_body = None;
    request.response_body = None;
    request
}

pub(super) fn build_curl_export(request: &NetworkRequestRecord) -> Result<CurlExport, RubError> {
    if request.method.trim().is_empty() {
        return Err(RubError::domain_with_context(
            ErrorCode::BrowserCrashed,
            "inspect curl requires an authoritative recorded request method",
            serde_json::json!({
                "kind": "network_request",
                "id": request.request_id,
                "reason": "request_method_missing",
            }),
        ));
    }

    let mut parts = vec!["curl".to_string()];
    parts.push("-X".to_string());
    parts.push(shell_quote(&request.method));

    for (name, value) in &request.request_headers {
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("host") {
            continue;
        }
        parts.push("-H".to_string());
        parts.push(shell_quote(&format!("{name}: {value}")));
    }

    let mut body_complete = true;
    let mut body_omitted_reason = None;
    if let Some(body) = &request.request_body {
        if body.available {
            let is_truncated = body.truncated.unwrap_or(false);
            match body.encoding.as_deref() {
                Some("text") if !is_truncated => {
                    parts.push("--data-raw".to_string());
                    parts.push(shell_quote(body.preview.as_deref().unwrap_or("")));
                }
                Some("base64") => {
                    body_complete = false;
                    body_omitted_reason = Some("request_body_base64".to_string());
                }
                _ if is_truncated => {
                    body_complete = false;
                    body_omitted_reason = Some("request_body_truncated".to_string());
                }
                _ => {}
            }
        } else {
            body_complete = false;
            body_omitted_reason = body.omitted_reason.clone();
        }
    }

    parts.push(shell_quote(&request.url));
    Ok(CurlExport {
        command: parts.join(" "),
        body_complete,
        body_omitted_reason,
    })
}

pub(super) fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    if input
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-._~/:?&=%@".contains(ch))
    {
        return input.to_string();
    }
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

pub(super) fn network_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

pub(super) fn network_wait_outcome_summary(
    lifecycle: NetworkRequestWaitState,
) -> serde_json::Value {
    let (class, summary) = match lifecycle {
        NetworkRequestWaitState::Completed
        | NetworkRequestWaitState::Failed
        | NetworkRequestWaitState::Terminal => (
            "confirmed_terminal_request",
            "A matching network request reached the requested terminal lifecycle.",
        ),
        NetworkRequestWaitState::Pending | NetworkRequestWaitState::Responded => (
            "confirmed_new_item_observed",
            "A matching network request was observed in the requested lifecycle window.",
        ),
    };
    serde_json::json!({
        "class": class,
        "authoritative": true,
        "summary": summary,
    })
}

pub(super) fn network_registry_subject(
    last: Option<usize>,
    url_match: Option<&str>,
    method: Option<&str>,
    status: Option<u16>,
    lifecycle: Option<NetworkRequestWaitState>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "network_request_registry",
        "last": last,
        "url_match": url_match,
        "method": method,
        "status": status,
        "lifecycle": lifecycle.map(NetworkRequestWaitState::as_str),
    })
}

pub(super) fn network_request_subject(request_id: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "network_request",
        "request_id": request_id,
    })
}

pub(super) fn network_wait_subject(
    request_id: Option<&str>,
    url_match: Option<&str>,
    method: Option<&str>,
    status: Option<u16>,
    lifecycle: NetworkRequestWaitState,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "network_request_wait",
        "request_id": request_id,
        "url_match": url_match,
        "method": method,
        "status": status,
        "lifecycle": lifecycle.as_str(),
    })
}
