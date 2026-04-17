mod pending_requests;
#[cfg(test)]
mod tests;

use base64::Engine;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams, EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent,
    EventResponseReceived, GetResponseBodyParams, Headers, Request,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    ConsoleApiCalledType, EventConsoleApiCalled, EventExceptionThrown, RemoteObject,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    ConsoleErrorEvent, NetworkBodyPreview, NetworkFailureEvent, NetworkRequestLifecycle,
    ObservedNetworkRequestRecord, PageErrorEvent, RequestSummaryEvent,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, next_listener_event};
use crate::request_correlation::{
    RequestCorrelation, RequestCorrelationRegistry, normalize_header_name,
};
use pending_requests::{
    PendingRequest, PendingTerminalState, apply_terminal_correlation, build_request_record,
    headers_to_map, notify_pending_registry_evicted, pending_request_from_response,
    pending_request_from_terminal, request_body_preview, response_body_preview,
    should_capture_response_preview, skipped_body_preview, terminal_correlation_lookup_headers,
    terminal_correlation_lookup_method, terminal_correlation_lookup_url, unavailable_body_preview,
    unknown_request_method,
};
pub(crate) use pending_requests::{
    SharedPendingRequestRegistry, new_shared_pending_request_registry,
    prune_stale_pending_request_registries,
};

type ConsoleCallback = Arc<dyn Fn(ConsoleErrorEvent) + Send + Sync>;
type PageErrorCallback = Arc<dyn Fn(PageErrorEvent) + Send + Sync>;
type NetworkFailureCallback = Arc<dyn Fn(NetworkFailureEvent) + Send + Sync>;
type RequestSummaryCallback = Arc<dyn Fn(RequestSummaryEvent) + Send + Sync>;
type RequestRecordCallback = Arc<dyn Fn(ObservedNetworkRequestRecord) + Send + Sync>;
type ObservatoryDegradedCallback = Arc<dyn Fn(String) + Send + Sync>;

const PENDING_REQUEST_RETENTION_LIMIT: usize = 1_024;

async fn peek_request_correlation_with_degraded(
    request_correlation: &Arc<Mutex<RequestCorrelationRegistry>>,
    request_id: &str,
    url: &str,
    method: &str,
    request_headers: Option<&BTreeMap<String, String>>,
    tab_target_id: Option<&str>,
    on_runtime_degraded: &Option<ObservatoryDegradedCallback>,
) -> Option<RequestCorrelation> {
    let (correlation, degraded_reasons) = {
        let mut request_correlation = request_correlation.lock().await;
        let correlation = request_correlation.peek_for_request(
            request_id,
            url,
            method,
            request_headers,
            tab_target_id,
        );
        let degraded_reasons = request_correlation.take_degraded_reasons();
        (correlation, degraded_reasons)
    };
    notify_request_correlation_degraded(degraded_reasons, on_runtime_degraded);
    correlation
}

async fn take_request_correlation_with_degraded(
    request_correlation: &Arc<Mutex<RequestCorrelationRegistry>>,
    request_id: &str,
    url: &str,
    method: &str,
    request_headers: Option<&BTreeMap<String, String>>,
    tab_target_id: Option<&str>,
    on_runtime_degraded: &Option<ObservatoryDegradedCallback>,
) -> Option<RequestCorrelation> {
    let (correlation, degraded_reasons) = {
        let mut request_correlation = request_correlation.lock().await;
        let correlation = request_correlation.take_for_request(
            request_id,
            url,
            method,
            request_headers,
            tab_target_id,
        );
        let degraded_reasons = request_correlation.take_degraded_reasons();
        (correlation, degraded_reasons)
    };
    notify_request_correlation_degraded(degraded_reasons, on_runtime_degraded);
    correlation
}

fn notify_request_correlation_degraded(
    degraded_reasons: Vec<&'static str>,
    on_runtime_degraded: &Option<ObservatoryDegradedCallback>,
) {
    if let Some(callback) = on_runtime_degraded {
        for reason in degraded_reasons {
            callback(reason.to_string());
        }
    }
}

#[derive(Clone, Default)]
pub struct ObservatoryCallbacks {
    pub on_console_error: Option<ConsoleCallback>,
    pub on_page_error: Option<PageErrorCallback>,
    pub on_network_failure: Option<NetworkFailureCallback>,
    pub on_request_summary: Option<RequestSummaryCallback>,
    pub on_request_record: Option<RequestRecordCallback>,
    pub on_runtime_degraded: Option<ObservatoryDegradedCallback>,
}

impl ObservatoryCallbacks {
    pub fn is_empty(&self) -> bool {
        self.on_console_error.is_none()
            && self.on_page_error.is_none()
            && self.on_network_failure.is_none()
            && self.on_request_summary.is_none()
            && self.on_request_record.is_none()
            && self.on_runtime_degraded.is_none()
    }
}

pub(crate) async fn ensure_page_observatory(
    page: Arc<Page>,
    callbacks: ObservatoryCallbacks,
    request_correlation: Arc<Mutex<RequestCorrelationRegistry>>,
    pending_registry: SharedPendingRequestRegistry,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
) -> Result<(), RubError> {
    if callbacks.is_empty() {
        return Ok(());
    }

    page.execute(EnableParams::default())
        .await
        .map_err(|error| {
            RubError::domain(
                ErrorCode::BrowserCrashed,
                format!("Runtime observatory failed to enable Network domain: {error}"),
            )
        })?;

    page.enable_runtime().await.map_err(|error| {
        RubError::domain(
            ErrorCode::BrowserCrashed,
            format!("Runtime observatory failed to enable Runtime domain: {error}"),
        )
    })?;
    let tab_target_id = page.target_id().as_ref().to_string();

    if let Some(callback) = callbacks.on_console_error.clone()
        && let Ok(mut listener) = page.event_listener::<EventConsoleApiCalled>().await
    {
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                if let Some(level) = console_level(&event.r#type) {
                    callback(ConsoleErrorEvent {
                        level: level.to_string(),
                        message: console_message(&event.args),
                        source: event.context.clone(),
                    });
                }
            }
        });
    } else if callbacks.on_console_error.is_some() {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            "Runtime observatory failed to subscribe to console events",
        ));
    }

    if let Some(callback) = callbacks.on_page_error.clone()
        && let Ok(mut listener) = page.event_listener::<EventExceptionThrown>().await
    {
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                callback(PageErrorEvent {
                    message: exception_message(&event),
                    source: event.exception_details.url.clone(),
                });
            }
        });
    } else if callbacks.on_page_error.is_some() {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            "Runtime observatory failed to subscribe to page exception events",
        ));
    }

    if let Ok(mut listener) = page.event_listener::<EventRequestWillBeSent>().await {
        let pending_registry = pending_registry.clone();
        let request_correlation = request_correlation.clone();
        let on_request_record = callbacks.on_request_record.clone();
        let on_runtime_degraded = callbacks.on_runtime_degraded.clone();
        let tab_target_id = tab_target_id.clone();
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                let request_id = event.request_id.as_ref().to_string();
                let request_headers = headers_to_map(&event.request.headers);
                let correlation = peek_request_correlation_with_degraded(
                    &request_correlation,
                    &request_id,
                    &event.request.url,
                    event.request.method.as_str(),
                    Some(&request_headers),
                    Some(&tab_target_id),
                    &on_runtime_degraded,
                )
                .await;
                let pending = PendingRequest {
                    request_id: request_id.clone(),
                    lifecycle: NetworkRequestLifecycle::Pending,
                    url: event.request.url.clone(),
                    method: event.request.method.to_string(),
                    tab_target_id: Some(tab_target_id.clone()),
                    frame_id: event
                        .frame_id
                        .as_ref()
                        .map(|frame_id| frame_id.as_ref().to_string()),
                    resource_type: event
                        .r#type
                        .as_ref()
                        .map(|value| value.as_ref().to_string()),
                    request_headers: request_headers.clone(),
                    request_body: request_body_preview(&event.request),
                    response_headers: BTreeMap::new(),
                    response_body: None,
                    status: None,
                    original_url: correlation.as_ref().map(|value| value.original_url.clone()),
                    rewritten_url: correlation
                        .as_ref()
                        .and_then(|value| value.rewritten_url.clone()),
                    applied_rule_effects: correlation
                        .as_ref()
                        .map(|value| value.applied_rule_effects.clone())
                        .unwrap_or_default(),
                    error_text: None,
                    mime_type: None,
                };
                let current = {
                    let mut pending_registry = pending_registry.lock().await;
                    let upsert = pending_registry.record_request(&request_id, pending);
                    notify_pending_registry_evicted(upsert.evicted, &on_runtime_degraded);
                    upsert.current
                };
                if let Some(callback) = &on_request_record {
                    callback(build_request_record(&current));
                }
            }
        });
    } else {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            "Runtime observatory failed to subscribe to requestWillBeSent events",
        ));
    }

    if (callbacks.on_request_summary.is_some() || callbacks.on_request_record.is_some())
        && let Ok(mut listener) = page.event_listener::<EventResponseReceived>().await
    {
        let on_request_summary = callbacks.on_request_summary.clone();
        let pending_registry = pending_registry.clone();
        let request_correlation = request_correlation.clone();
        let on_request_record = callbacks.on_request_record.clone();
        let on_runtime_degraded = callbacks.on_runtime_degraded.clone();
        let tab_target_id = tab_target_id.clone();
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                let request_id = event.request_id.as_ref().to_string();
                let correlation = if on_request_record.is_some() {
                    peek_request_correlation_with_degraded(
                        &request_correlation,
                        &request_id,
                        &event.response.url,
                        &unknown_request_method(),
                        None,
                        Some(&tab_target_id),
                        &on_runtime_degraded,
                    )
                    .await
                } else {
                    take_request_correlation_with_degraded(
                        &request_correlation,
                        &request_id,
                        &event.response.url,
                        &unknown_request_method(),
                        None,
                        Some(&tab_target_id),
                        &on_runtime_degraded,
                    )
                    .await
                };
                let current = {
                    let mut pending_registry = pending_registry.lock().await;
                    let upsert = pending_registry.record_request(
                        &request_id,
                        pending_request_from_response(
                            &request_id,
                            &event,
                            correlation.as_ref(),
                            &tab_target_id,
                        ),
                    );
                    notify_pending_registry_evicted(upsert.evicted, &on_runtime_degraded);
                    upsert.current
                };

                if let Some(callback) = &on_request_summary {
                    callback(RequestSummaryEvent {
                        request_id: request_id.clone(),
                        url: current.url.clone(),
                        method: current.method.clone(),
                        status: current.status,
                        original_url: current.original_url.clone(),
                        rewritten_url: current.rewritten_url.clone(),
                        applied_rule_effects: current.applied_rule_effects.clone(),
                    });
                }
                if let Some(callback) = &on_request_record {
                    callback(build_request_record(&current));
                }
            }
        });
    } else if callbacks.on_request_summary.is_some() || callbacks.on_request_record.is_some() {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            "Runtime observatory failed to subscribe to responseReceived events",
        ));
    }

    if (callbacks.on_request_record.is_some() || callbacks.on_request_summary.is_some())
        && let Ok(mut listener) = page.event_listener::<EventLoadingFinished>().await
    {
        let on_request_record = callbacks.on_request_record.clone();
        let pending_registry = pending_registry.clone();
        let page = page.clone();
        let request_correlation = request_correlation.clone();
        let on_runtime_degraded = callbacks.on_runtime_degraded.clone();
        let tab_target_id = tab_target_id.clone();
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                let request_id = event.request_id.as_ref().to_string();
                let PendingTerminalState {
                    pending,
                    terminal_identity,
                } = pending_registry
                    .lock()
                    .await
                    .take_terminal_state(&request_id);
                let mut pending = pending.unwrap_or_else(|| {
                    pending_request_from_terminal(
                        &request_id,
                        NetworkRequestLifecycle::Completed,
                        terminal_identity.as_ref(),
                        None,
                        &tab_target_id,
                    )
                });
                let correlation = take_request_correlation_with_degraded(
                    &request_correlation,
                    &request_id,
                    terminal_correlation_lookup_url(Some(&pending), terminal_identity.as_ref()),
                    terminal_correlation_lookup_method(Some(&pending), terminal_identity.as_ref()),
                    terminal_correlation_lookup_headers(Some(&pending), terminal_identity.as_ref()),
                    Some(&tab_target_id),
                    &on_runtime_degraded,
                )
                .await;
                apply_terminal_correlation(
                    &mut pending,
                    &request_id,
                    NetworkRequestLifecycle::Completed,
                    correlation.as_ref(),
                    &tab_target_id,
                );
                pending.lifecycle = NetworkRequestLifecycle::Completed;
                let response_body = if should_capture_response_preview(&pending) {
                    response_body_preview(&page, &event).await
                } else {
                    skipped_body_preview(&pending)
                };
                pending.response_body = Some(response_body);
                if let Some(callback) = &on_request_record {
                    callback(build_request_record(&pending));
                }
            }
        });
    } else if callbacks.on_request_record.is_some() || callbacks.on_request_summary.is_some() {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            "Runtime observatory failed to subscribe to loadingFinished events",
        ));
    }

    if (callbacks.on_network_failure.is_some()
        || callbacks.on_request_record.is_some()
        || callbacks.on_request_summary.is_some())
        && let Ok(mut listener) = page.event_listener::<EventLoadingFailed>().await
    {
        let on_network_failure = callbacks.on_network_failure.clone();
        let pending_registry = pending_registry.clone();
        let request_correlation = request_correlation.clone();
        let on_request_record = callbacks.on_request_record.clone();
        let on_runtime_degraded = callbacks.on_runtime_degraded.clone();
        let tab_target_id = tab_target_id.clone();
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                let request_id = event.request_id.as_ref().to_string();
                let PendingTerminalState {
                    pending,
                    terminal_identity,
                } = pending_registry
                    .lock()
                    .await
                    .take_terminal_state(&request_id);
                let correlation = take_request_correlation_with_degraded(
                    &request_correlation,
                    &request_id,
                    terminal_correlation_lookup_url(pending.as_ref(), terminal_identity.as_ref()),
                    terminal_correlation_lookup_method(
                        pending.as_ref(),
                        terminal_identity.as_ref(),
                    ),
                    terminal_correlation_lookup_headers(
                        pending.as_ref(),
                        terminal_identity.as_ref(),
                    ),
                    Some(&tab_target_id),
                    &on_runtime_degraded,
                )
                .await;
                let fallback = pending_request_from_terminal(
                    &request_id,
                    NetworkRequestLifecycle::Failed,
                    terminal_identity.as_ref(),
                    correlation.as_ref(),
                    &tab_target_id,
                );
                let current_url = pending
                    .as_ref()
                    .map(|request| request.url.clone())
                    .unwrap_or_else(|| fallback.url.clone());
                let original_url = correlation.as_ref().and_then(|correlation| {
                    (!current_url.is_empty() && correlation.original_url != current_url)
                        .then(|| correlation.original_url.clone())
                });
                let rewritten_url = correlation.as_ref().and_then(|correlation| {
                    correlation
                        .rewritten_url
                        .clone()
                        .filter(|rewritten_url| rewritten_url != &current_url)
                });
                let applied_rule_effects = correlation
                    .as_ref()
                    .map(|correlation| correlation.applied_rule_effects.clone())
                    .unwrap_or_default();
                let effective_request_headers = correlation
                    .as_ref()
                    .and_then(|correlation| correlation.effective_request_headers.clone());
                if let Some(callback) = &on_network_failure {
                    callback(NetworkFailureEvent {
                        request_id: request_id.clone(),
                        url: current_url.clone(),
                        method: terminal_failure_method(pending.as_ref(), &fallback),
                        error_text: event.error_text.clone(),
                        original_url: original_url.clone(),
                        rewritten_url: rewritten_url.clone(),
                        applied_rule_effects: applied_rule_effects.clone(),
                    });
                }
                if let Some(callback) = &on_request_record {
                    let fallback = PendingRequest {
                        request_id: request_id.clone(),
                        lifecycle: NetworkRequestLifecycle::Failed,
                        url: current_url,
                        method: terminal_failure_method(pending.as_ref(), &fallback),
                        tab_target_id: pending
                            .as_ref()
                            .and_then(|request| request.tab_target_id.clone())
                            .or(fallback.tab_target_id.clone()),
                        frame_id: pending
                            .as_ref()
                            .and_then(|request| request.frame_id.clone())
                            .or(fallback.frame_id.clone()),
                        resource_type: pending
                            .as_ref()
                            .and_then(|request| request.resource_type.clone())
                            .or(fallback.resource_type.clone()),
                        request_headers: pending
                            .as_ref()
                            .map(|request| {
                                effective_request_headers
                                    .clone()
                                    .unwrap_or_else(|| request.request_headers.clone())
                            })
                            .unwrap_or_else(|| fallback.request_headers.clone()),
                        request_body: pending
                            .as_ref()
                            .and_then(|request| request.request_body.clone())
                            .or(fallback.request_body.clone()),
                        response_headers: pending
                            .as_ref()
                            .map(|request| request.response_headers.clone())
                            .unwrap_or_else(|| fallback.response_headers.clone()),
                        response_body: Some(unavailable_body_preview("request_failed")),
                        status: pending.as_ref().and_then(|request| request.status),
                        original_url,
                        rewritten_url,
                        applied_rule_effects,
                        error_text: Some(event.error_text.clone()),
                        mime_type: pending
                            .as_ref()
                            .and_then(|request| request.mime_type.clone())
                            .or(fallback.mime_type.clone()),
                    };
                    callback(build_request_record(&fallback));
                }
            }
        });
    } else if callbacks.on_network_failure.is_some()
        || callbacks.on_request_record.is_some()
        || callbacks.on_request_summary.is_some()
    {
        return Err(RubError::domain(
            ErrorCode::BrowserCrashed,
            "Runtime observatory failed to subscribe to loadingFailed events",
        ));
    }

    Ok(())
}

fn console_level(level: &ConsoleApiCalledType) -> Option<&'static str> {
    match level {
        ConsoleApiCalledType::Error => Some("error"),
        ConsoleApiCalledType::Warning => Some("warning"),
        ConsoleApiCalledType::Assert => Some("assert"),
        _ => None,
    }
}

fn console_message(args: &[RemoteObject]) -> String {
    let parts: Vec<String> = args.iter().map(remote_object_summary).collect();
    if parts.is_empty() {
        "<console event with no arguments>".to_string()
    } else {
        parts.join(" ")
    }
}

fn remote_object_summary(object: &RemoteObject) -> String {
    if let Some(value) = &object.value {
        return match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
    }
    if let Some(value) = &object.unserializable_value {
        return format!("{value:?}");
    }
    if let Some(description) = &object.description {
        return description.clone();
    }
    object.r#type.as_ref().to_string()
}

fn exception_message(event: &EventExceptionThrown) -> String {
    event
        .exception_details
        .exception
        .as_ref()
        .and_then(|exception| exception.description.clone())
        .unwrap_or_else(|| event.exception_details.text.clone())
}

fn terminal_failure_method(pending: Option<&PendingRequest>, fallback: &PendingRequest) -> String {
    pending
        .as_ref()
        .map(|request| request.method.clone())
        .unwrap_or_else(|| fallback.method.clone())
}
