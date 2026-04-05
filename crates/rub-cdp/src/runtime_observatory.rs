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
    NetworkRequestRecord, PageErrorEvent, RequestSummaryEvent,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, next_listener_event};
use crate::request_correlation::{
    RequestCorrelation, RequestCorrelationRegistry, normalize_header_name,
};

type ConsoleCallback = Arc<dyn Fn(ConsoleErrorEvent) + Send + Sync>;
type PageErrorCallback = Arc<dyn Fn(PageErrorEvent) + Send + Sync>;
type NetworkFailureCallback = Arc<dyn Fn(NetworkFailureEvent) + Send + Sync>;
type RequestSummaryCallback = Arc<dyn Fn(RequestSummaryEvent) + Send + Sync>;
type RequestRecordCallback = Arc<dyn Fn(NetworkRequestRecord) + Send + Sync>;
type ObservatoryDegradedCallback = Arc<dyn Fn(String) + Send + Sync>;

const PENDING_REQUEST_RETENTION_LIMIT: usize = 1_024;

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

#[derive(Clone)]
struct PendingRequest {
    request_id: String,
    lifecycle: NetworkRequestLifecycle,
    url: String,
    method: String,
    tab_target_id: Option<String>,
    frame_id: Option<String>,
    resource_type: Option<String>,
    request_headers: BTreeMap<String, String>,
    request_body: Option<NetworkBodyPreview>,
    response_headers: BTreeMap<String, String>,
    response_body: Option<NetworkBodyPreview>,
    status: Option<u16>,
    original_url: Option<String>,
    rewritten_url: Option<String>,
    applied_rule_effects: Vec<rub_core::model::NetworkRuleEffect>,
    error_text: Option<String>,
    mime_type: Option<String>,
}

#[derive(Clone, Default)]
struct TerminalRequestIdentity {
    url: String,
    method: String,
    tab_target_id: Option<String>,
    frame_id: Option<String>,
    resource_type: Option<String>,
    request_headers: BTreeMap<String, String>,
}

pub(crate) type SharedPendingRequestRegistry = Arc<Mutex<PendingRequestRegistry>>;

struct PendingRegistryUpsert {
    current: PendingRequest,
    evicted: bool,
}

struct PendingTerminalState {
    pending: Option<PendingRequest>,
    terminal_identity: Option<TerminalRequestIdentity>,
}

#[derive(Default)]
pub(crate) struct PendingRequestRegistry {
    pending_requests: HashMap<String, PendingRequest>,
    terminal_identities: HashMap<String, TerminalRequestIdentity>,
    request_order: VecDeque<String>,
}

impl PendingRequestRegistry {
    fn record_request(
        &mut self,
        request_id: &str,
        pending: PendingRequest,
    ) -> PendingRegistryUpsert {
        let (current, evicted_pending) = self.upsert_pending(request_id, pending);
        let evicted_terminal =
            self.upsert_terminal_identity(request_id, terminal_identity_from_pending(&current));
        PendingRegistryUpsert {
            current,
            evicted: evicted_pending || evicted_terminal,
        }
    }

    fn take_terminal_state(&mut self, request_id: &str) -> PendingTerminalState {
        let (pending, terminal_identity) = self.remove(request_id);
        PendingTerminalState {
            pending,
            terminal_identity,
        }
    }

    fn upsert_pending(
        &mut self,
        request_id: &str,
        pending: PendingRequest,
    ) -> (PendingRequest, bool) {
        let current = if let Some(existing) = self.pending_requests.get_mut(request_id) {
            merge_pending_request(existing, pending);
            existing.clone()
        } else {
            self.pending_requests
                .insert(request_id.to_string(), pending.clone());
            pending
        };
        self.touch(request_id);
        let evicted = self.enforce_bound();
        (current, evicted)
    }

    fn upsert_terminal_identity(
        &mut self,
        request_id: &str,
        identity: TerminalRequestIdentity,
    ) -> bool {
        self.terminal_identities
            .insert(request_id.to_string(), identity);
        self.touch(request_id);
        self.enforce_bound()
    }

    fn remove(
        &mut self,
        request_id: &str,
    ) -> (Option<PendingRequest>, Option<TerminalRequestIdentity>) {
        self.remove_from_order(request_id);
        (
            self.pending_requests.remove(request_id),
            self.terminal_identities.remove(request_id),
        )
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending_requests.len()
    }

    #[cfg(test)]
    fn terminal_identity_len(&self) -> usize {
        self.terminal_identities.len()
    }

    fn touch(&mut self, request_id: &str) {
        self.remove_from_order(request_id);
        self.request_order.push_back(request_id.to_string());
    }

    fn remove_from_order(&mut self, request_id: &str) {
        if let Some(position) = self.request_order.iter().position(|id| id == request_id) {
            self.request_order.remove(position);
        }
    }

    fn enforce_bound(&mut self) -> bool {
        let mut evicted = false;
        while self.request_order.len() > PENDING_REQUEST_RETENTION_LIMIT {
            if let Some(oldest_request_id) = self.request_order.pop_front() {
                self.pending_requests.remove(&oldest_request_id);
                self.terminal_identities.remove(&oldest_request_id);
                evicted = true;
            }
        }
        evicted
    }
}

fn terminal_identity_from_pending(current: &PendingRequest) -> TerminalRequestIdentity {
    TerminalRequestIdentity {
        url: current.url.clone(),
        method: current.method.clone(),
        tab_target_id: current.tab_target_id.clone(),
        frame_id: current.frame_id.clone(),
        resource_type: current.resource_type.clone(),
        request_headers: current.request_headers.clone(),
    }
}

fn notify_pending_registry_evicted(
    evicted: bool,
    on_runtime_degraded: &Option<ObservatoryDegradedCallback>,
) {
    if evicted && let Some(callback) = on_runtime_degraded {
        callback("pending_request_registry_evicted".to_string());
    }
}

pub(crate) fn new_shared_pending_request_registry() -> SharedPendingRequestRegistry {
    Arc::new(Mutex::new(PendingRequestRegistry::default()))
}

pub(crate) async fn prune_stale_pending_request_registries(
    registries: &Arc<Mutex<HashMap<String, SharedPendingRequestRegistry>>>,
    live_target_ids: &HashSet<String>,
) {
    registries
        .lock()
        .await
        .retain(|target_id, _| live_target_ids.contains(target_id));
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
                let correlation = request_correlation.lock().await.peek_for_request(
                    &request_id,
                    &event.request.url,
                    event.request.method.as_str(),
                    Some(&request_headers),
                );
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
                    request_correlation.lock().await.peek_for_request(
                        &request_id,
                        &event.response.url,
                        &unknown_request_method(),
                        None,
                    )
                } else {
                    request_correlation.lock().await.take_for_request(
                        &request_id,
                        &event.response.url,
                        &unknown_request_method(),
                        None,
                    )
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
                let correlation = request_correlation.lock().await.take_for_request(
                    &request_id,
                    terminal_correlation_lookup_url(Some(&pending), terminal_identity.as_ref()),
                    terminal_correlation_lookup_method(Some(&pending), terminal_identity.as_ref()),
                    terminal_correlation_lookup_headers(Some(&pending), terminal_identity.as_ref()),
                );
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
                let correlation = request_correlation.lock().await.take_for_request(
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
                );
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
                        method: pending
                            .as_ref()
                            .map(|request| request.method.clone())
                            .unwrap_or_else(unknown_request_method),
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
                        method: pending
                            .as_ref()
                            .map(|request| request.method.clone())
                            .unwrap_or_else(|| fallback.method.clone()),
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

fn pending_request_from_response(
    request_id: &str,
    event: &EventResponseReceived,
    correlation: Option<&RequestCorrelation>,
    tab_target_id: &str,
) -> PendingRequest {
    let original_url = correlation.as_ref().and_then(|correlation| {
        (correlation.original_url != event.response.url).then(|| correlation.original_url.clone())
    });
    let rewritten_url = correlation.as_ref().and_then(|correlation| {
        correlation
            .rewritten_url
            .clone()
            .filter(|rewritten_url| rewritten_url != &event.response.url)
    });

    PendingRequest {
        request_id: request_id.to_string(),
        lifecycle: NetworkRequestLifecycle::Responded,
        url: event.response.url.clone(),
        method: unknown_request_method(),
        tab_target_id: Some(tab_target_id.to_string()),
        frame_id: event
            .frame_id
            .as_ref()
            .map(|frame_id| frame_id.as_ref().to_string()),
        resource_type: Some(event.r#type.as_ref().to_string()),
        request_headers: correlation
            .as_ref()
            .and_then(|correlation| correlation.effective_request_headers.clone())
            .unwrap_or_default(),
        request_body: None,
        response_headers: headers_to_map(&event.response.headers),
        response_body: Some(unavailable_body_preview("not_loaded")),
        status: normalize_status(event.response.status),
        original_url,
        rewritten_url,
        applied_rule_effects: correlation
            .as_ref()
            .map(|correlation| correlation.applied_rule_effects.clone())
            .unwrap_or_default(),
        error_text: None,
        mime_type: Some(event.response.mime_type.clone()),
    }
}

fn terminal_correlation_lookup_url<'a>(
    pending: Option<&'a PendingRequest>,
    terminal_identity: Option<&'a TerminalRequestIdentity>,
) -> &'a str {
    pending
        .map(|request| request.url.as_str())
        .filter(|url| !url.is_empty())
        .or_else(|| {
            terminal_identity
                .map(|identity| identity.url.as_str())
                .filter(|url| !url.is_empty())
        })
        .unwrap_or("")
}

fn terminal_correlation_lookup_headers<'a>(
    pending: Option<&'a PendingRequest>,
    terminal_identity: Option<&'a TerminalRequestIdentity>,
) -> Option<&'a BTreeMap<String, String>> {
    pending
        .map(|request| &request.request_headers)
        .filter(|headers| !headers.is_empty())
        .or_else(|| {
            terminal_identity
                .map(|identity| &identity.request_headers)
                .filter(|headers| !headers.is_empty())
        })
}

fn terminal_correlation_lookup_method<'a>(
    pending: Option<&'a PendingRequest>,
    terminal_identity: Option<&'a TerminalRequestIdentity>,
) -> &'a str {
    pending
        .map(|request| request.method.as_str())
        .filter(|method| !method.is_empty())
        .or_else(|| {
            terminal_identity
                .map(|identity| identity.method.as_str())
                .filter(|method| !method.is_empty())
        })
        .unwrap_or("")
}

fn pending_request_from_terminal(
    request_id: &str,
    lifecycle: NetworkRequestLifecycle,
    terminal_identity: Option<&TerminalRequestIdentity>,
    correlation: Option<&RequestCorrelation>,
    tab_target_id: &str,
) -> PendingRequest {
    let url = correlation
        .as_ref()
        .and_then(|correlation| {
            correlation
                .rewritten_url
                .clone()
                .or_else(|| Some(correlation.original_url.clone()))
        })
        .or_else(|| terminal_identity.map(|identity| identity.url.clone()))
        .unwrap_or_default();

    PendingRequest {
        request_id: request_id.to_string(),
        lifecycle,
        url,
        method: terminal_identity
            .map(|identity| identity.method.clone())
            .filter(|method| !method.is_empty())
            .unwrap_or_else(unknown_request_method),
        tab_target_id: terminal_identity
            .and_then(|identity| identity.tab_target_id.clone())
            .or_else(|| Some(tab_target_id.to_string())),
        frame_id: terminal_identity.and_then(|identity| identity.frame_id.clone()),
        resource_type: terminal_identity.and_then(|identity| identity.resource_type.clone()),
        request_headers: correlation
            .as_ref()
            .and_then(|correlation| correlation.effective_request_headers.clone())
            .or_else(|| terminal_identity.map(|identity| identity.request_headers.clone()))
            .unwrap_or_default(),
        request_body: None,
        response_headers: BTreeMap::new(),
        response_body: Some(unavailable_body_preview("not_loaded")),
        status: None,
        original_url: correlation
            .as_ref()
            .map(|correlation| correlation.original_url.clone()),
        rewritten_url: correlation
            .as_ref()
            .and_then(|correlation| correlation.rewritten_url.clone()),
        applied_rule_effects: correlation
            .as_ref()
            .map(|correlation| correlation.applied_rule_effects.clone())
            .unwrap_or_default(),
        error_text: None,
        mime_type: None,
    }
}

fn apply_terminal_correlation(
    pending: &mut PendingRequest,
    request_id: &str,
    lifecycle: NetworkRequestLifecycle,
    correlation: Option<&RequestCorrelation>,
    tab_target_id: &str,
) {
    if correlation.is_none() {
        return;
    }

    merge_pending_request(
        pending,
        pending_request_from_terminal(request_id, lifecycle, None, correlation, tab_target_id),
    );
}

fn merge_pending_request(existing: &mut PendingRequest, incoming: PendingRequest) {
    if lifecycle_rank(incoming.lifecycle) >= lifecycle_rank(existing.lifecycle) {
        existing.lifecycle = incoming.lifecycle;
    }
    if !incoming.url.is_empty() {
        existing.url = incoming.url;
    }
    if !incoming.method.is_empty() && existing.method.is_empty() {
        existing.method = incoming.method;
    }
    if incoming.tab_target_id.is_some() {
        existing.tab_target_id = incoming.tab_target_id;
    }
    if incoming.status.is_some() {
        existing.status = incoming.status;
    }
    if !incoming.request_headers.is_empty() {
        existing.request_headers = incoming.request_headers;
    }
    if !incoming.response_headers.is_empty() {
        existing.response_headers = incoming.response_headers;
    }
    if incoming.request_body.is_some() {
        existing.request_body = incoming.request_body;
    }
    if incoming.response_body.is_some() {
        existing.response_body = incoming.response_body;
    }
    if incoming.original_url.is_some() {
        existing.original_url = incoming.original_url;
    }
    if incoming.rewritten_url.is_some() {
        existing.rewritten_url = incoming.rewritten_url;
    }
    if !incoming.applied_rule_effects.is_empty() {
        existing.applied_rule_effects = incoming.applied_rule_effects;
    }
    if incoming.error_text.is_some() {
        existing.error_text = incoming.error_text;
    }
    if incoming.frame_id.is_some() {
        existing.frame_id = incoming.frame_id;
    }
    if incoming.resource_type.is_some() {
        existing.resource_type = incoming.resource_type;
    }
    if incoming.mime_type.is_some() {
        existing.mime_type = incoming.mime_type;
    }
}

fn lifecycle_rank(lifecycle: NetworkRequestLifecycle) -> u8 {
    match lifecycle {
        NetworkRequestLifecycle::Pending => 0,
        NetworkRequestLifecycle::Responded => 1,
        NetworkRequestLifecycle::Completed => 2,
        NetworkRequestLifecycle::Failed => 2,
    }
}

fn unknown_request_method() -> String {
    String::new()
}

fn build_request_record(pending: &PendingRequest) -> NetworkRequestRecord {
    NetworkRequestRecord {
        request_id: pending.request_id.clone(),
        sequence: 0,
        lifecycle: pending.lifecycle,
        url: pending.url.clone(),
        method: pending.method.clone(),
        tab_target_id: pending.tab_target_id.clone(),
        status: pending.status,
        request_headers: pending.request_headers.clone(),
        response_headers: pending.response_headers.clone(),
        request_body: pending.request_body.clone(),
        response_body: pending.response_body.clone(),
        original_url: pending.original_url.clone(),
        rewritten_url: pending.rewritten_url.clone(),
        applied_rule_effects: pending.applied_rule_effects.clone(),
        error_text: pending.error_text.clone(),
        frame_id: pending.frame_id.clone(),
        resource_type: pending.resource_type.clone(),
        mime_type: pending.mime_type.clone(),
    }
}

fn headers_to_map(headers: &Headers) -> BTreeMap<String, String> {
    let mut projected = BTreeMap::new();
    if let Some(obj) = headers.inner().as_object() {
        for (name, value) in obj {
            if let Some(value) = value.as_str() {
                projected.insert(normalize_header_name(name), value.to_string());
            } else if !value.is_null() {
                projected.insert(normalize_header_name(name), value.to_string());
            }
        }
    }
    projected
}

fn request_body_preview(request: &Request) -> Option<NetworkBodyPreview> {
    if let Some(entries) = &request.post_data_entries {
        let body = entries
            .iter()
            .filter_map(|entry| entry.bytes.as_ref().cloned().map(String::from))
            .collect::<String>();
        return Some(project_body_preview(body, true));
    }

    request
        .has_post_data
        .filter(|has_post_data| *has_post_data)
        .map(|_| unavailable_body_preview("not_captured"))
}

fn should_capture_response_preview(pending: &PendingRequest) -> bool {
    let resource_type = pending
        .resource_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        resource_type.as_str(),
        "image" | "media" | "font" | "stylesheet"
    ) {
        return false;
    }

    let mime_type = pending
        .mime_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if mime_type.starts_with("image/")
        || mime_type.starts_with("audio/")
        || mime_type.starts_with("video/")
        || mime_type.starts_with("font/")
        || matches!(
            mime_type.as_str(),
            "application/octet-stream" | "application/pdf" | "application/zip"
        )
    {
        return false;
    }

    pending
        .response_headers
        .get("content-length")
        .or_else(|| pending.response_headers.get("Content-Length"))
        .and_then(|value| value.parse::<u64>().ok())
        .is_none_or(|length| length <= 64 * 1024)
}

async fn response_body_preview(
    page: &Arc<Page>,
    event: &EventLoadingFinished,
) -> NetworkBodyPreview {
    match page
        .execute(GetResponseBodyParams::new(event.request_id.clone()))
        .await
    {
        Ok(response) => project_body_preview(response.body.clone(), response.base64_encoded),
        Err(error) => unavailable_body_preview(format!("unavailable:{error}")),
    }
}

fn project_body_preview(body: String, base64_encoded: bool) -> NetworkBodyPreview {
    const BODY_PREVIEW_LIMIT: usize = 4096;

    let (preview, encoding) = if base64_encoded {
        match base64::engine::general_purpose::STANDARD.decode(body.as_bytes()) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => (text, "text"),
                Err(_) => (body, "base64"),
            },
            Err(_) => (body, "base64"),
        }
    } else {
        (body, "text")
    };

    let truncated = preview.chars().count() > BODY_PREVIEW_LIMIT;
    let preview = preview.chars().take(BODY_PREVIEW_LIMIT).collect::<String>();
    NetworkBodyPreview {
        available: true,
        preview: Some(preview),
        encoding: Some(encoding.to_string()),
        truncated: truncated.then_some(true),
        omitted_reason: None,
    }
}

fn unavailable_body_preview(reason: impl Into<String>) -> NetworkBodyPreview {
    NetworkBodyPreview {
        available: false,
        preview: None,
        encoding: None,
        truncated: None,
        omitted_reason: Some(reason.into()),
    }
}

fn skipped_body_preview(pending: &PendingRequest) -> NetworkBodyPreview {
    let resource_type = pending
        .resource_type
        .as_deref()
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    let mime_type = pending.mime_type.as_deref().unwrap_or("unknown");
    let content_length = pending
        .response_headers
        .get("content-length")
        .or_else(|| pending.response_headers.get("Content-Length"))
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    unavailable_body_preview(format!(
        "omitted_by_budget:resource_type={resource_type}:mime_type={mime_type}:content_length={content_length}"
    ))
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

fn normalize_status(status: i64) -> Option<u16> {
    if (0..=u16::MAX as i64).contains(&status) {
        Some(status as u16)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ObservatoryCallbacks, PENDING_REQUEST_RETENTION_LIMIT, PendingRequest,
        PendingRequestRegistry, RequestCorrelation, TerminalRequestIdentity, console_message,
        exception_message, merge_pending_request, new_shared_pending_request_registry,
        normalize_status, pending_request_from_terminal, prune_stale_pending_request_registries,
        remote_object_summary, terminal_correlation_lookup_headers,
        terminal_correlation_lookup_url,
    };
    use chromiumoxide::cdp::js_protocol::runtime::{
        EventExceptionThrown, ExceptionDetails, RemoteObject, RemoteObjectType, Timestamp,
    };
    use rub_core::model::{NetworkRequestLifecycle, NetworkRuleEffect, NetworkRuleEffectKind};
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn sample_pending_request(request_id: &str) -> PendingRequest {
        PendingRequest {
            request_id: request_id.to_string(),
            lifecycle: NetworkRequestLifecycle::Pending,
            url: format!("https://example.com/{request_id}"),
            method: "GET".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            resource_type: Some("xhr".to_string()),
            request_headers: BTreeMap::new(),
            request_body: None,
            response_headers: BTreeMap::new(),
            response_body: None,
            status: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            mime_type: None,
        }
    }

    #[test]
    fn remote_object_summary_prefers_value_then_description() {
        let string_object = RemoteObject {
            r#type: RemoteObjectType::String,
            subtype: None,
            class_name: None,
            value: Some(serde_json::Value::String("boom".to_string())),
            unserializable_value: None,
            description: Some("ignored".to_string()),
            deep_serialized_value: None,
            object_id: None,
            preview: None,
            custom_preview: None,
        };
        assert_eq!(remote_object_summary(&string_object), "boom");
        assert_eq!(console_message(&[string_object]), "boom");
    }

    #[test]
    fn exception_message_prefers_exception_description() {
        let event = EventExceptionThrown {
            timestamp: Timestamp::new(0.0),
            exception_details: ExceptionDetails {
                exception_id: 1,
                text: "fallback".to_string(),
                line_number: 0,
                column_number: 0,
                script_id: None,
                url: Some("https://example.com/app.js".to_string()),
                stack_trace: None,
                exception: Some(RemoteObject {
                    r#type: RemoteObjectType::Object,
                    subtype: None,
                    class_name: Some("Error".to_string()),
                    value: None,
                    unserializable_value: None,
                    description: Some("Error: boom".to_string()),
                    deep_serialized_value: None,
                    object_id: None,
                    preview: None,
                    custom_preview: None,
                }),
                execution_context_id: None,
                exception_meta_data: None,
            },
        };

        assert_eq!(exception_message(&event), "Error: boom");
    }

    #[test]
    fn normalize_status_rejects_invalid_values() {
        assert_eq!(normalize_status(200), Some(200));
        assert_eq!(normalize_status(-1), None);
        assert_eq!(normalize_status(i64::MAX), None);
    }

    #[test]
    fn late_request_merge_does_not_downgrade_responded_pending() {
        let mut responded = super::PendingRequest {
            request_id: "req-1".to_string(),
            lifecycle: NetworkRequestLifecycle::Responded,
            url: "https://example.com/final".to_string(),
            method: "GET".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: None,
            resource_type: Some("xhr".to_string()),
            request_headers: BTreeMap::from([("x-test".to_string(), "1".to_string())]),
            request_body: None,
            response_headers: BTreeMap::new(),
            response_body: None,
            status: Some(200),
            original_url: Some("https://example.com/original".to_string()),
            rewritten_url: Some("https://example.com/final".to_string()),
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 1,
                kind: NetworkRuleEffectKind::Rewrite,
            }],
            error_text: None,
            mime_type: Some("application/json".to_string()),
        };

        let late_pending = super::PendingRequest {
            request_id: "req-1".to_string(),
            lifecycle: NetworkRequestLifecycle::Pending,
            url: "https://example.com/original".to_string(),
            method: "POST".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("main".to_string()),
            resource_type: Some("xhr".to_string()),
            request_headers: BTreeMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]),
            request_body: None,
            response_headers: BTreeMap::new(),
            response_body: None,
            status: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            mime_type: None,
        };

        merge_pending_request(&mut responded, late_pending);

        assert_eq!(responded.lifecycle, NetworkRequestLifecycle::Responded);
        assert_eq!(responded.method, "GET");
        assert_eq!(
            responded.request_headers["content-type"],
            "application/json"
        );
        assert_eq!(
            responded.original_url.as_deref(),
            Some("https://example.com/original")
        );
        assert_eq!(
            responded.rewritten_url.as_deref(),
            Some("https://example.com/final")
        );
    }

    #[test]
    fn terminal_fallback_uses_correlation_when_pending_request_is_missing() {
        let correlation = RequestCorrelation {
            original_url: "https://example.com/image".to_string(),
            rewritten_url: Some("https://cdn.example.com/image.webp".to_string()),
            effective_request_headers: None,
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 9,
                kind: NetworkRuleEffectKind::Rewrite,
            }],
        };

        let pending = pending_request_from_terminal(
            "req-9",
            NetworkRequestLifecycle::Completed,
            None,
            Some(&correlation),
            "tab-9",
        );

        assert_eq!(pending.lifecycle, NetworkRequestLifecycle::Completed);
        assert_eq!(pending.url, "https://cdn.example.com/image.webp");
        assert!(pending.method.is_empty());
        assert_eq!(
            pending.original_url.as_deref(),
            Some("https://example.com/image")
        );
    }

    #[test]
    fn terminal_correlation_merges_into_existing_pending_request() {
        let mut pending = super::PendingRequest {
            request_id: "req-10".to_string(),
            lifecycle: NetworkRequestLifecycle::Responded,
            url: "https://example.com/image.webp".to_string(),
            method: "GET".to_string(),
            tab_target_id: Some("tab-10".to_string()),
            frame_id: Some("frame-10".to_string()),
            resource_type: Some("image".to_string()),
            request_headers: BTreeMap::new(),
            request_body: None,
            response_headers: BTreeMap::new(),
            response_body: None,
            status: Some(200),
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            mime_type: Some("image/webp".to_string()),
        };
        let correlation = RequestCorrelation {
            original_url: "https://example.com/image".to_string(),
            rewritten_url: Some("https://cdn.example.com/image.webp".to_string()),
            effective_request_headers: None,
            applied_rule_effects: vec![NetworkRuleEffect {
                rule_id: 11,
                kind: NetworkRuleEffectKind::Rewrite,
            }],
        };

        super::apply_terminal_correlation(
            &mut pending,
            "req-10",
            NetworkRequestLifecycle::Completed,
            Some(&correlation),
            "tab-10",
        );

        assert_eq!(
            pending.original_url.as_deref(),
            Some("https://example.com/image")
        );
        assert_eq!(
            pending.rewritten_url.as_deref(),
            Some("https://cdn.example.com/image.webp")
        );
        assert_eq!(pending.applied_rule_effects.len(), 1);
    }

    #[test]
    fn terminal_fallback_uses_stored_request_identity_when_pending_is_missing() {
        let identity = TerminalRequestIdentity {
            url: "https://example.com/image.webp".to_string(),
            method: "GET".to_string(),
            tab_target_id: Some("tab-11".to_string()),
            frame_id: Some("frame-11".to_string()),
            resource_type: Some("image".to_string()),
            request_headers: BTreeMap::from([("accept".to_string(), "image/webp".to_string())]),
        };

        let pending = pending_request_from_terminal(
            "req-11",
            NetworkRequestLifecycle::Failed,
            Some(&identity),
            None,
            "tab-11",
        );

        assert_eq!(pending.url, "https://example.com/image.webp");
        assert_eq!(pending.method, "GET");
        assert_eq!(pending.frame_id.as_deref(), Some("frame-11"));
        assert_eq!(
            pending.request_headers.get("accept").map(String::as_str),
            Some("image/webp")
        );
    }

    #[test]
    fn terminal_correlation_lookup_uses_stored_request_identity_when_pending_is_missing() {
        let identity = TerminalRequestIdentity {
            url: "https://cdn.example.com/image.webp".to_string(),
            method: "GET".to_string(),
            tab_target_id: Some("tab-12".to_string()),
            frame_id: Some("frame-12".to_string()),
            resource_type: Some("image".to_string()),
            request_headers: BTreeMap::from([("accept".to_string(), "image/webp".to_string())]),
        };

        assert_eq!(
            terminal_correlation_lookup_url(None, Some(&identity)),
            "https://cdn.example.com/image.webp"
        );
        assert_eq!(
            terminal_correlation_lookup_headers(None, Some(&identity))
                .and_then(|headers| headers.get("accept"))
                .map(String::as_str),
            Some("image/webp")
        );
    }

    #[test]
    fn late_request_merge_populates_unknown_method_without_fabricating_get() {
        let mut responded = super::PendingRequest {
            request_id: "req-2".to_string(),
            lifecycle: NetworkRequestLifecycle::Responded,
            url: "https://example.com/upload".to_string(),
            method: String::new(),
            tab_target_id: Some("tab-2".to_string()),
            frame_id: None,
            resource_type: Some("fetch".to_string()),
            request_headers: BTreeMap::new(),
            request_body: None,
            response_headers: BTreeMap::new(),
            response_body: None,
            status: Some(201),
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            mime_type: Some("application/json".to_string()),
        };

        let late_pending = super::PendingRequest {
            request_id: "req-2".to_string(),
            lifecycle: NetworkRequestLifecycle::Pending,
            url: "https://example.com/upload".to_string(),
            method: "POST".to_string(),
            tab_target_id: Some("tab-2".to_string()),
            frame_id: Some("frame-1".to_string()),
            resource_type: Some("fetch".to_string()),
            request_headers: BTreeMap::new(),
            request_body: None,
            response_headers: BTreeMap::new(),
            response_body: None,
            status: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            mime_type: None,
        };

        merge_pending_request(&mut responded, late_pending);

        assert_eq!(responded.lifecycle, NetworkRequestLifecycle::Responded);
        assert_eq!(responded.method, "POST");
    }

    #[test]
    fn pending_request_registry_bounds_pending_and_terminal_tracking() {
        let mut registry = PendingRequestRegistry::default();
        let mut saw_eviction = false;
        for index in 0..(PENDING_REQUEST_RETENTION_LIMIT + 16) {
            let request_id = format!("req-{index}");
            let pending = sample_pending_request(&request_id);
            let upsert = registry.record_request(&request_id, pending);
            saw_eviction |= upsert.evicted;
        }

        assert!(
            saw_eviction,
            "bounded registry should report when it evicts old authority"
        );
        assert_eq!(registry.pending_len(), PENDING_REQUEST_RETENTION_LIMIT);
        assert_eq!(
            registry.terminal_identity_len(),
            PENDING_REQUEST_RETENTION_LIMIT
        );
        assert!(registry.remove("req-0").0.is_none());
        assert!(registry.remove("req-0").1.is_none());
        assert!(registry.remove("req-16").0.is_some());
    }

    #[test]
    fn record_request_updates_terminal_identity_from_merged_pending_state() {
        let mut registry = PendingRequestRegistry::default();
        let request_id = "req-merged";
        registry.record_request(request_id, sample_pending_request(request_id));

        let mut responded = sample_pending_request(request_id);
        responded.lifecycle = NetworkRequestLifecycle::Responded;
        responded.url = "https://example.com/final".to_string();
        responded.method = String::new();
        responded.request_headers = BTreeMap::new();
        registry.record_request(request_id, responded);

        let terminal = registry
            .take_terminal_state(request_id)
            .terminal_identity
            .expect("terminal identity should be retained");
        assert_eq!(terminal.url, "https://example.com/final");
        assert_eq!(terminal.method, "GET");
    }

    #[test]
    fn degraded_only_callbacks_are_not_empty() {
        let callbacks = ObservatoryCallbacks {
            on_runtime_degraded: Some(Arc::new(|_| {})),
            ..Default::default()
        };

        assert!(
            !callbacks.is_empty(),
            "degraded-only observatory installs must still keep listeners alive"
        );
    }

    #[tokio::test]
    async fn prune_stale_pending_request_registries_drops_missing_targets() {
        let registries = Arc::new(Mutex::new(HashMap::from([
            (
                "tab-live".to_string(),
                new_shared_pending_request_registry(),
            ),
            (
                "tab-stale".to_string(),
                new_shared_pending_request_registry(),
            ),
        ])));

        prune_stale_pending_request_registries(
            &registries,
            &HashSet::from(["tab-live".to_string()]),
        )
        .await;

        let guard = registries.lock().await;
        assert!(guard.contains_key("tab-live"));
        assert!(!guard.contains_key("tab-stale"));
    }
}
