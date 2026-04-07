use std::sync::Arc;
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{NetworkRequestLifecycle, NetworkRequestRecord};

use crate::router::request_args::parse_json_args;
use crate::session::SessionState;

#[derive(Clone, Copy)]
struct NetworkWaitErrorContext<'a> {
    request_id: Option<&'a str>,
    url_match: Option<&'a str>,
    method: Option<&'a str>,
    status: Option<u16>,
    desired_state: NetworkRequestWaitState,
    started: Instant,
}

#[derive(Debug)]
enum InspectNetworkCommand {
    Timeline(NetworkTimelineArgs),
    Curl(NetworkCurlArgs),
}

impl InspectNetworkCommand {
    fn parse(args: &serde_json::Value, sub: &str) -> Result<Self, RubError> {
        let mut normalized = args.clone();
        if let Some(object) = normalized.as_object_mut() {
            // Use the sub provided explicitly by cmd_inspect dispatch (already matched
            // from the routing key before it was stripped from forwarded args).
            object.insert("sub".to_string(), serde_json::json!(sub));
        }
        #[derive(Debug, serde::Deserialize)]
        #[serde(tag = "sub", rename_all = "lowercase")]
        enum TaggedInspectNetworkCommand {
            Network(NetworkTimelineArgs),
            Curl(NetworkCurlArgs),
        }

        match parse_json_args::<TaggedInspectNetworkCommand>(&normalized, "inspect network")? {
            TaggedInspectNetworkCommand::Network(args) => Ok(Self::Timeline(args)),
            TaggedInspectNetworkCommand::Curl(args) => Ok(Self::Curl(args)),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkTimelineArgs {
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    last: Option<u64>,
    #[serde(default)]
    url_match: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    status: Option<u64>,
    #[serde(default)]
    lifecycle: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkCurlArgs {
    id: String,
}

pub(super) async fn cmd_inspect_network(
    args: &serde_json::Value,
    inspect_sub: &str,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match InspectNetworkCommand::parse(args, inspect_sub)? {
        InspectNetworkCommand::Timeline(parsed) => cmd_network_timeline(parsed, state).await,
        InspectNetworkCommand::Curl(parsed) => cmd_network_curl(parsed, state).await,
    }
}

async fn cmd_network_timeline(
    args: NetworkTimelineArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    if args.wait {
        return cmd_network_wait(args, state).await;
    }

    if let Some(request_id) = args.id.as_deref() {
        let request = state
            .network_request_record(request_id)
            .await
            .ok_or_else(|| {
                RubError::domain(
                    ErrorCode::ElementNotFound,
                    format!("No recorded network request with id '{request_id}'"),
                )
            })?;
        return Ok(network_payload(
            network_request_subject(request_id),
            serde_json::json!({ "request": request }),
        ));
    }

    let last = args.last.map(|value| value.min(100) as usize);
    let url_match = args.url_match.as_deref();
    let method = args.method.as_deref();
    let status = args.status.map(|value| value.min(u16::MAX as u64) as u16);
    let lifecycle = parse_lifecycle_filter(args.lifecycle.as_deref())?;

    let requests = filter_requests_by_wait_state(
        state
            .network_request_records(
                last,
                url_match,
                method,
                status,
                lifecycle.and_then(NetworkRequestWaitState::actual_filter),
            )
            .await,
        lifecycle,
    )
    .into_iter()
    .map(summarize_request_record)
    .collect::<Vec<_>>();
    Ok(network_payload(
        network_registry_subject(last, url_match, method, status, lifecycle),
        serde_json::json!({
            "items": requests,
        }),
    ))
}

async fn cmd_network_wait(
    args: NetworkTimelineArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let request_id = args.id.as_deref();
    let url_match = args.url_match.as_deref();
    let method = args.method.as_deref();
    let status = args.status.map(|value| value.min(u16::MAX as u64) as u16);
    let desired_state = parse_lifecycle_filter(args.lifecycle.as_deref())?
        .unwrap_or(NetworkRequestWaitState::Terminal);
    let timeout =
        Duration::from_millis(args.timeout_ms.unwrap_or(rub_core::DEFAULT_WAIT_TIMEOUT_MS));
    let started = Instant::now();
    let deadline = started + timeout;
    let notify = state.network_request_notifier();
    let mut cursor = state.network_request_cursor().await;
    let mut observed_drop_count = state.network_request_drop_count().await;
    let error_context = NetworkWaitErrorContext {
        request_id,
        url_match,
        method,
        status,
        desired_state,
        started,
    };

    loop {
        let notified = notify.notified();
        let window = state
            .network_request_window_after(cursor, observed_drop_count)
            .await;
        observed_drop_count = state.network_request_drop_count().await;
        cursor = window.next_cursor;
        if !window.authoritative {
            return Err(network_wait_not_authoritative_error(
                error_context,
                cursor,
                observed_drop_count,
                window.degraded_reason,
            ));
        }

        let mut matches = if let Some(request_id) = request_id {
            state
                .network_request_record(request_id)
                .await
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            window.records
        };

        matches = filter_requests_by_wait_state(matches, Some(desired_state));
        if request_id.is_none() {
            matches.retain(|record| {
                url_match
                    .map(|needle| record.url.contains(needle))
                    .unwrap_or(true)
                    && method
                        .map(|needle| record.method.eq_ignore_ascii_case(needle))
                        .unwrap_or(true)
                    && status
                        .map(|value| record.status == Some(value))
                        .unwrap_or(true)
            });
        }
        if let Some(request_id) = request_id {
            matches.retain(|record| record.request_id == request_id);
        }
        if let Some(request) = matches.into_iter().next() {
            return Ok(network_payload(
                network_wait_subject(request_id, url_match, method, status, desired_state),
                serde_json::json!({
                    "matched": true,
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                    "request": request,
                }),
            ));
        }

        if Instant::now() >= deadline {
            return Err(network_wait_timeout_error(error_context));
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if tokio::time::timeout(remaining, notified).await.is_err() {
            return Err(network_wait_timeout_error(error_context));
        }
    }
}

fn network_wait_timeout_error(context: NetworkWaitErrorContext<'_>) -> RubError {
    RubError::domain_with_context(
        ErrorCode::WaitTimeout,
        "Network wait timed out before a matching request reached the requested lifecycle",
        serde_json::json!({
            "kind": "network_request",
            "id": context.request_id,
            "url_match": context.url_match,
            "method": context.method,
            "status": context.status,
            "lifecycle": context.desired_state.as_str(),
            "elapsed_ms": context.started.elapsed().as_millis() as u64,
        }),
    )
}

fn network_wait_not_authoritative_error(
    context: NetworkWaitErrorContext<'_>,
    next_cursor: u64,
    dropped_event_count: u64,
    degraded_reason: Option<String>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::BrowserCrashed,
        "Network wait is no longer authoritative because observatory evidence was dropped",
        serde_json::json!({
            "kind": "network_request",
            "id": context.request_id,
            "url_match": context.url_match,
            "method": context.method,
            "status": context.status,
            "lifecycle": context.desired_state.as_str(),
            "elapsed_ms": context.started.elapsed().as_millis() as u64,
            "reason": "runtime_observatory_not_authoritative",
            "degraded_reason": degraded_reason,
            "next_network_cursor": next_cursor,
            "dropped_event_count": dropped_event_count,
        }),
    )
}

async fn cmd_network_curl(
    args: NetworkCurlArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let request_id = args.id.as_str();
    let request = state
        .network_request_record(request_id)
        .await
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::ElementNotFound,
                format!("No recorded network request with id '{request_id}'"),
            )
        })?;
    let export = build_curl_export(&request)?;
    Ok(network_payload(
        network_request_subject(request_id),
        serde_json::json!({
            "request": request,
            "export": {
                "kind": "curl_command",
                "command": export.command,
                "body_complete": export.body_complete,
                "body_omitted_reason": export.body_omitted_reason,
            }
        }),
    ))
}

#[derive(Debug)]
struct CurlExport {
    command: String,
    body_complete: bool,
    body_omitted_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkRequestWaitState {
    Pending,
    Responded,
    Completed,
    Failed,
    Terminal,
}

impl NetworkRequestWaitState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Responded => "responded",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Terminal => "terminal",
        }
    }

    fn actual_filter(self) -> Option<NetworkRequestLifecycle> {
        match self {
            Self::Pending => Some(NetworkRequestLifecycle::Pending),
            Self::Responded => Some(NetworkRequestLifecycle::Responded),
            Self::Completed => Some(NetworkRequestLifecycle::Completed),
            Self::Failed => Some(NetworkRequestLifecycle::Failed),
            Self::Terminal => None,
        }
    }

    fn matches(self, lifecycle: NetworkRequestLifecycle) -> bool {
        match self {
            Self::Pending => lifecycle == NetworkRequestLifecycle::Pending,
            Self::Responded => lifecycle == NetworkRequestLifecycle::Responded,
            Self::Completed => lifecycle == NetworkRequestLifecycle::Completed,
            Self::Failed => lifecycle == NetworkRequestLifecycle::Failed,
            Self::Terminal => matches!(
                lifecycle,
                NetworkRequestLifecycle::Completed | NetworkRequestLifecycle::Failed
            ),
        }
    }
}

fn parse_lifecycle_filter(
    value: Option<&str>,
) -> Result<Option<NetworkRequestWaitState>, RubError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        "pending" => Ok(Some(NetworkRequestWaitState::Pending)),
        "responded" => Ok(Some(NetworkRequestWaitState::Responded)),
        "completed" => Ok(Some(NetworkRequestWaitState::Completed)),
        "failed" => Ok(Some(NetworkRequestWaitState::Failed)),
        "terminal" => Ok(Some(NetworkRequestWaitState::Terminal)),
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Unknown network lifecycle '{other}'. Valid: pending, responded, completed, failed, terminal"
            ),
        )),
    }
}

fn filter_requests_by_wait_state(
    requests: Vec<NetworkRequestRecord>,
    state: Option<NetworkRequestWaitState>,
) -> Vec<NetworkRequestRecord> {
    let Some(state) = state else {
        return requests;
    };
    requests
        .into_iter()
        .filter(|record| state.matches(record.lifecycle))
        .collect()
}

fn summarize_request_record(mut request: NetworkRequestRecord) -> NetworkRequestRecord {
    request.request_body = None;
    request.response_body = None;
    request
}

fn build_curl_export(
    request: &rub_core::model::NetworkRequestRecord,
) -> Result<CurlExport, RubError> {
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

fn shell_quote(input: &str) -> String {
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

fn network_payload(subject: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

fn network_registry_subject(
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

fn network_request_subject(request_id: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "network_request",
        "request_id": request_id,
    })
}

fn network_wait_subject(
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

#[cfg(test)]
mod tests {
    use super::{
        InspectNetworkCommand, NetworkRequestWaitState, NetworkTimelineArgs, build_curl_export,
        cmd_network_wait, filter_requests_by_wait_state, network_payload, network_registry_subject,
        network_request_subject, network_wait_subject, parse_lifecycle_filter, shell_quote,
    };
    use rub_core::error::ErrorCode;
    use rub_core::model::{NetworkBodyPreview, NetworkRequestLifecycle, NetworkRequestRecord};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn network_wait_fails_closed_when_observatory_degrades_while_waiting() {
        let state = Arc::new(crate::session::SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-test"),
            None,
        ));
        let args = NetworkTimelineArgs {
            wait: true,
            id: None,
            last: None,
            url_match: Some("/api/orders".to_string()),
            method: None,
            status: None,
            lifecycle: None,
            timeout_ms: Some(5_000),
        };
        let state_for_task = state.clone();
        let wait = tokio::spawn(async move { cmd_network_wait(args, &state_for_task).await });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        state
            .mark_observatory_degraded("observatory_ingress_overflow")
            .await;

        let error = wait
            .await
            .expect("wait task should join")
            .expect_err("degraded evidence should fail closed")
            .into_envelope();
        assert_eq!(error.code, ErrorCode::BrowserCrashed);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("degraded_reason"))
                .and_then(|value| value.as_str()),
            Some("observatory_ingress_overflow")
        );
    }

    #[test]
    fn typed_inspect_network_payload_defaults_to_timeline() {
        // "sub" is now provided explicitly by cmd_inspect dispatch; pass "network" directly.
        let parsed = InspectNetworkCommand::parse(
            &serde_json::json!({
                "wait": true,
                "timeout_ms": 10,
            }),
            "network",
        )
        .expect("network inspect should parse as timeline");
        assert!(matches!(
            parsed,
            InspectNetworkCommand::Timeline(NetworkTimelineArgs {
                wait: true,
                timeout_ms: Some(10),
                ..
            })
        ));
    }

    #[test]
    fn shell_quote_handles_embedded_single_quotes() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn curl_export_omits_unreproducible_request_bodies() {
        let mut request_headers = BTreeMap::new();
        request_headers.insert("content-type".to_string(), "application/json".to_string());
        let request = NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 1,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api".to_string(),
            method: "POST".to_string(),
            tab_target_id: None,
            status: Some(200),
            request_headers,
            response_headers: BTreeMap::new(),
            request_body: Some(NetworkBodyPreview {
                available: true,
                preview: Some("eyJvayI6dHJ1ZX0=".to_string()),
                encoding: Some("base64".to_string()),
                truncated: None,
                omitted_reason: None,
            }),
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: None,
        };

        let export = build_curl_export(&request).expect("curl export should succeed");
        assert!(!export.body_complete);
        assert_eq!(
            export.body_omitted_reason.as_deref(),
            Some("request_body_base64")
        );
        assert!(export.command.contains("curl -X POST"));
        assert!(!export.command.contains("--data-raw"));
    }

    #[test]
    fn curl_export_fails_closed_when_request_method_is_missing() {
        let request = NetworkRequestRecord {
            request_id: "req-missing-method".to_string(),
            sequence: 1,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api".to_string(),
            method: String::new(),
            tab_target_id: None,
            status: Some(200),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: None,
        };

        let error = build_curl_export(&request)
            .expect_err("missing authoritative request method should fail closed")
            .into_envelope();
        assert_eq!(error.code, ErrorCode::BrowserCrashed);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("request_method_missing")
        );
    }

    #[test]
    fn lifecycle_filter_accepts_terminal_and_rejects_unknown_values() {
        assert_eq!(
            parse_lifecycle_filter(Some("terminal")).expect("terminal should parse"),
            Some(NetworkRequestWaitState::Terminal)
        );
        let err =
            parse_lifecycle_filter(Some("mystery")).expect_err("unknown lifecycle should error");
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn terminal_filter_matches_completed_and_failed_requests() {
        let requests = vec![
            NetworkRequestRecord {
                request_id: "req-1".to_string(),
                sequence: 1,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: "https://example.com/api/orders".to_string(),
                method: "GET".to_string(),
                tab_target_id: None,
                status: Some(200),
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_body: None,
                response_body: None,
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
                error_text: None,
                frame_id: None,
                resource_type: None,
                mime_type: None,
            },
            NetworkRequestRecord {
                request_id: "req-2".to_string(),
                sequence: 2,
                lifecycle: NetworkRequestLifecycle::Failed,
                url: "https://example.com/api/missing".to_string(),
                method: "GET".to_string(),
                tab_target_id: None,
                status: None,
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_body: None,
                response_body: None,
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
                error_text: Some("net::ERR_ABORTED".to_string()),
                frame_id: None,
                resource_type: None,
                mime_type: None,
            },
            NetworkRequestRecord {
                request_id: "req-3".to_string(),
                sequence: 3,
                lifecycle: NetworkRequestLifecycle::Responded,
                url: "https://example.com/api/pending".to_string(),
                method: "GET".to_string(),
                tab_target_id: None,
                status: Some(202),
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_body: None,
                response_body: None,
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
                error_text: None,
                frame_id: None,
                resource_type: None,
                mime_type: None,
            },
        ];

        let filtered =
            filter_requests_by_wait_state(requests, Some(NetworkRequestWaitState::Terminal));
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].request_id, "req-1");
        assert_eq!(filtered[1].request_id, "req-2");
    }

    #[test]
    fn network_subject_helpers_are_machine_facing() {
        let registry = network_registry_subject(
            Some(10),
            Some("/api/orders"),
            Some("POST"),
            Some(200),
            Some(NetworkRequestWaitState::Terminal),
        );
        assert_eq!(registry["kind"], "network_request_registry");
        assert_eq!(registry["last"], 10);
        assert_eq!(registry["url_match"], "/api/orders");
        assert_eq!(registry["method"], "POST");
        assert_eq!(registry["status"], 200);
        assert_eq!(registry["lifecycle"], "terminal");

        let request = network_request_subject("req-1");
        assert_eq!(request["kind"], "network_request");
        assert_eq!(request["request_id"], "req-1");

        let wait = network_wait_subject(
            None,
            Some("/api"),
            Some("GET"),
            None,
            NetworkRequestWaitState::Completed,
        );
        assert_eq!(wait["kind"], "network_request_wait");
        assert_eq!(wait["url_match"], "/api");
        assert_eq!(wait["method"], "GET");
        assert_eq!(wait["lifecycle"], "completed");
    }

    #[test]
    fn network_payload_wraps_subject_and_result() {
        let payload = network_payload(
            network_request_subject("req-1"),
            serde_json::json!({ "request": { "request_id": "req-1" } }),
        );
        assert_eq!(payload["subject"]["kind"], "network_request");
        assert_eq!(payload["result"]["request"]["request_id"], "req-1");
    }
}
