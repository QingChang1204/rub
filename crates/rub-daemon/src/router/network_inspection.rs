mod args;
mod projection;

use std::sync::Arc;
use std::time::{Duration, Instant};

use self::args::{
    InspectNetworkCommand, NetworkCurlArgs, NetworkRequestWaitState, NetworkTimelineArgs,
    NetworkWaitErrorContext, filter_requests_by_wait_state, parse_lifecycle_filter,
};
use self::projection::{
    build_curl_export, network_payload, network_registry_subject, network_request_subject,
    network_wait_outcome_summary, network_wait_subject, summarize_request_record,
};
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};

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

    if let Some(request) = find_matching_network_wait_record(
        state,
        request_id,
        url_match,
        method,
        status,
        desired_state,
    )
    .await
    {
        return Ok(network_payload(
            network_wait_subject(request_id, url_match, method, status, desired_state),
            serde_json::json!({
                "matched": true,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "request": request,
                "outcome_summary": network_wait_outcome_summary(desired_state),
            }),
        ));
    }

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
                    "outcome_summary": network_wait_outcome_summary(desired_state),
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

async fn find_matching_network_wait_record(
    state: &Arc<SessionState>,
    request_id: Option<&str>,
    url_match: Option<&str>,
    method: Option<&str>,
    status: Option<u16>,
    desired_state: NetworkRequestWaitState,
) -> Option<rub_core::model::NetworkRequestRecord> {
    if let Some(request_id) = request_id {
        let request = state.network_request_record(request_id).await?;
        return desired_state.matches(request.lifecycle).then_some(request);
    }

    let requests = state
        .network_request_records(
            None,
            url_match,
            method,
            status,
            desired_state.actual_filter(),
        )
        .await;
    filter_requests_by_wait_state(requests, Some(desired_state))
        .into_iter()
        .next()
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

#[cfg(test)]
mod tests {
    use super::projection::shell_quote;
    use super::{
        InspectNetworkCommand, NetworkRequestWaitState, NetworkTimelineArgs, build_curl_export,
        cmd_network_wait, filter_requests_by_wait_state, network_payload, network_registry_subject,
        network_request_subject, network_wait_outcome_summary, network_wait_subject,
        parse_lifecycle_filter,
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

    #[tokio::test]
    async fn network_wait_matches_existing_terminal_record_before_waiting() {
        let state = Arc::new(crate::session::SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-test"),
            None,
        ));
        state
            .upsert_network_request_record(NetworkRequestRecord {
                request_id: "req-existing".to_string(),
                sequence: 1,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: "https://example.com/api/delayed?order=7".to_string(),
                method: "POST".to_string(),
                tab_target_id: Some("tab-1".to_string()),
                status: Some(200),
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_body: None,
                response_body: None,
                original_url: None,
                rewritten_url: None,
                applied_rule_effects: Vec::new(),
                error_text: None,
                frame_id: Some("frame-1".to_string()),
                resource_type: Some("Fetch".to_string()),
                mime_type: Some("application/json".to_string()),
            })
            .await;

        let result = cmd_network_wait(
            NetworkTimelineArgs {
                wait: true,
                id: None,
                last: None,
                url_match: Some("/api/delayed".to_string()),
                method: Some("POST".to_string()),
                status: None,
                lifecycle: Some("terminal".to_string()),
                timeout_ms: Some(5_000),
            },
            &state,
        )
        .await
        .expect("existing terminal request should satisfy wait immediately");

        assert_eq!(result["result"]["matched"], true);
        assert_eq!(result["result"]["request"]["request_id"], "req-existing");
        assert_eq!(result["subject"]["lifecycle"], "terminal");
        assert_eq!(
            result["result"]["outcome_summary"]["class"],
            "confirmed_terminal_request"
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

    #[test]
    fn network_wait_outcome_summary_distinguishes_terminal_and_observed_states() {
        let terminal = network_wait_outcome_summary(NetworkRequestWaitState::Terminal);
        assert_eq!(terminal["class"], "confirmed_terminal_request");
        assert_eq!(terminal["authoritative"], true);

        let pending = network_wait_outcome_summary(NetworkRequestWaitState::Pending);
        assert_eq!(pending["class"], "confirmed_new_item_observed");
    }
}
