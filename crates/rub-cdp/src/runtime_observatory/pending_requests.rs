use super::*;

#[derive(Clone)]
pub(super) struct PendingRequest {
    pub(super) request_id: String,
    pub(super) lifecycle: NetworkRequestLifecycle,
    pub(super) url: String,
    pub(super) method: String,
    pub(super) tab_target_id: Option<String>,
    pub(super) frame_id: Option<String>,
    pub(super) resource_type: Option<String>,
    pub(super) request_headers: BTreeMap<String, String>,
    pub(super) request_body: Option<NetworkBodyPreview>,
    pub(super) response_headers: BTreeMap<String, String>,
    pub(super) response_body: Option<NetworkBodyPreview>,
    pub(super) status: Option<u16>,
    pub(super) original_url: Option<String>,
    pub(super) rewritten_url: Option<String>,
    pub(super) applied_rule_effects: Vec<rub_core::model::NetworkRuleEffect>,
    pub(super) error_text: Option<String>,
    pub(super) mime_type: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct TerminalRequestIdentity {
    pub(super) url: String,
    pub(super) method: String,
    pub(super) tab_target_id: Option<String>,
    pub(super) frame_id: Option<String>,
    pub(super) resource_type: Option<String>,
    pub(super) request_headers: BTreeMap<String, String>,
}

pub(crate) type SharedPendingRequestRegistry = Arc<Mutex<PendingRequestRegistry>>;

pub(super) struct PendingRegistryUpsert {
    pub(super) current: PendingRequest,
    pub(super) evicted: bool,
}

pub(super) struct PendingTerminalState {
    pub(super) pending: Option<PendingRequest>,
    pub(super) terminal_identity: Option<TerminalRequestIdentity>,
}

#[derive(Default)]
pub(crate) struct PendingRequestRegistry {
    pending_requests: HashMap<String, PendingRequest>,
    terminal_identities: HashMap<String, TerminalRequestIdentity>,
    request_order: VecDeque<String>,
}

impl PendingRequestRegistry {
    pub(super) fn record_request(
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

    pub(super) fn take_terminal_state(&mut self, request_id: &str) -> PendingTerminalState {
        let (pending, terminal_identity) = self.remove(request_id);
        PendingTerminalState {
            pending,
            terminal_identity,
        }
    }

    pub(super) fn peek_terminal_identity(
        &self,
        request_id: &str,
    ) -> Option<TerminalRequestIdentity> {
        self.terminal_identities
            .get(request_id)
            .cloned()
            .or_else(|| {
                self.pending_requests
                    .get(request_id)
                    .map(terminal_identity_from_pending)
            })
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

    pub(super) fn remove(
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
    pub(super) fn pending_len(&self) -> usize {
        self.pending_requests.len()
    }

    #[cfg(test)]
    pub(super) fn terminal_identity_len(&self) -> usize {
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

pub(super) fn notify_pending_registry_evicted(
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

#[cfg(test)]
pub(crate) async fn prune_stale_pending_request_registries(
    registries: &Arc<Mutex<HashMap<String, SharedPendingRequestRegistry>>>,
    live_target_ids: &HashSet<String>,
) {
    registries
        .lock()
        .await
        .retain(|target_id, _| live_target_ids.contains(target_id));
}

pub(super) fn pending_request_from_response(
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

pub(super) fn terminal_correlation_lookup_url<'a>(
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

pub(super) fn terminal_correlation_lookup_headers<'a>(
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

pub(super) fn terminal_correlation_lookup_method<'a>(
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

pub(super) fn pending_request_from_terminal(
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

pub(super) fn apply_terminal_correlation(
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

pub(super) fn merge_pending_request(existing: &mut PendingRequest, incoming: PendingRequest) {
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

pub(super) fn unknown_request_method() -> String {
    String::new()
}

pub(super) fn build_request_record(
    pending: &PendingRequest,
) -> rub_core::model::ObservedNetworkRequestRecord {
    rub_core::model::ObservedNetworkRequestRecord {
        request_id: pending.request_id.clone(),
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

pub(super) fn headers_to_map(headers: &Headers) -> BTreeMap<String, String> {
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

pub(super) fn request_body_preview(request: &Request) -> Option<NetworkBodyPreview> {
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

pub(super) fn should_capture_response_preview(pending: &PendingRequest) -> bool {
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

pub(super) async fn response_body_preview(
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

pub(super) fn unavailable_body_preview(reason: impl Into<String>) -> NetworkBodyPreview {
    NetworkBodyPreview {
        available: false,
        preview: None,
        encoding: None,
        truncated: None,
        omitted_reason: Some(reason.into()),
    }
}

pub(super) fn skipped_body_preview(pending: &PendingRequest) -> NetworkBodyPreview {
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

pub(super) fn normalize_status(status: i64) -> Option<u16> {
    if (0..=u16::MAX as i64).contains(&status) {
        Some(status as u16)
    } else {
        None
    }
}
