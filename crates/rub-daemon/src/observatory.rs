use std::collections::{HashMap, VecDeque};

use rub_core::model::{
    ConsoleErrorEvent, NetworkFailureEvent, NetworkRequestLifecycle, NetworkRequestRecord,
    PageErrorEvent, RequestSummaryEvent, RuntimeObservatoryEvent, RuntimeObservatoryEventPayload,
    RuntimeObservatoryInfo, RuntimeObservatoryStatus,
};

const OBSERVATORY_RING_LIMIT: usize = 32;
const OBSERVATORY_TIMELINE_LIMIT: usize = 64;
const OBSERVATORY_REQUEST_RECORD_LIMIT: usize = 1_024;

#[derive(Debug, Clone)]
pub(crate) struct NetworkRequestWindow {
    pub(crate) records: Vec<NetworkRequestRecord>,
    pub(crate) next_cursor: u64,
    pub(crate) authoritative: bool,
    pub(crate) degraded_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ObservatoryEventWindow {
    pub(crate) events: Vec<RuntimeObservatoryEvent>,
    pub(crate) authoritative: bool,
    pub(crate) degraded_reason: Option<String>,
}

/// Session-scoped ring buffer for runtime observability events.
#[derive(Debug, Default)]
pub struct RuntimeObservatoryState {
    status: RuntimeObservatoryStatus,
    degraded_reason: Option<String>,
    next_sequence: u64,
    next_request_sequence: u64,
    timeline: VecDeque<RuntimeObservatoryEvent>,
    console_errors: VecDeque<ConsoleErrorEvent>,
    page_errors: VecDeque<PageErrorEvent>,
    network_failures: VecDeque<NetworkFailureEvent>,
    requests: VecDeque<RequestSummaryEvent>,
    request_records: HashMap<String, NetworkRequestRecord>,
    request_order: VecDeque<String>,
    dropped_timeline_event_count: u64,
    dropped_request_record_count: u64,
    last_evicted_timeline_sequence: u64,
    last_evicted_request_sequence: u64,
}

impl RuntimeObservatoryState {
    pub fn projection(&self) -> RuntimeObservatoryInfo {
        self.projection_with_drop_count(
            self.dropped_timeline_event_count
                .saturating_add(self.dropped_request_record_count),
        )
    }

    pub fn projection_with_drop_count(&self, dropped_event_count: u64) -> RuntimeObservatoryInfo {
        RuntimeObservatoryInfo {
            status: self.status,
            recent_console_errors: self.console_errors.iter().cloned().collect(),
            recent_page_errors: self.page_errors.iter().cloned().collect(),
            recent_network_failures: self.network_failures.iter().cloned().collect(),
            recent_requests: self.requests.iter().cloned().collect(),
            dropped_event_count,
            degraded_reason: self.degraded_reason.clone(),
        }
    }

    pub fn is_ready(&self) -> bool {
        !matches!(self.status, RuntimeObservatoryStatus::Inactive)
    }

    pub fn mark_active(&mut self) {
        if !matches!(self.status, RuntimeObservatoryStatus::Degraded) {
            self.status = RuntimeObservatoryStatus::Active;
            self.degraded_reason = None;
        }
    }

    pub fn mark_ready(&mut self) {
        if matches!(self.status, RuntimeObservatoryStatus::Inactive) {
            self.status = RuntimeObservatoryStatus::Active;
            self.degraded_reason = None;
        }
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) {
        self.status = RuntimeObservatoryStatus::Degraded;
        self.degraded_reason = Some(reason.into());
    }

    pub fn cursor(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    pub fn request_cursor(&self) -> u64 {
        self.next_request_sequence.saturating_sub(1)
    }

    pub fn events_after(&self, cursor: u64) -> Vec<RuntimeObservatoryEvent> {
        self.timeline
            .iter()
            .filter(|event| event_sequence(event) > cursor)
            .cloned()
            .collect()
    }

    pub(crate) fn event_window_after(
        &self,
        cursor: u64,
        total_drop_count: u64,
        last_observed_drop_count: u64,
    ) -> ObservatoryEventWindow {
        let events = self.events_after(cursor);
        let dropped_since_last_poll = total_drop_count > last_observed_drop_count;
        let cursor_lost_to_eviction =
            dropped_since_last_poll && cursor < self.last_evicted_timeline_sequence;
        let degraded_reason = if cursor_lost_to_eviction {
            Some("observatory_timeline_overflow".to_string())
        } else {
            self.degraded_reason.clone()
        };
        ObservatoryEventWindow {
            events,
            authoritative: degraded_reason.is_none(),
            degraded_reason,
        }
    }

    pub fn push_console_error(&mut self, event: ConsoleErrorEvent) {
        self.mark_active();
        push_ring(&mut self.console_errors, event.clone());
        let sequence = self.next_sequence();
        self.push_timeline(RuntimeObservatoryEvent {
            sequence,
            payload: RuntimeObservatoryEventPayload::ConsoleError(event),
        });
    }

    pub fn push_page_error(&mut self, event: PageErrorEvent) {
        self.mark_active();
        push_ring(&mut self.page_errors, event.clone());
        let sequence = self.next_sequence();
        self.push_timeline(RuntimeObservatoryEvent {
            sequence,
            payload: RuntimeObservatoryEventPayload::PageError(event),
        });
    }

    pub fn push_network_failure(&mut self, event: NetworkFailureEvent) {
        self.mark_active();
        push_ring(&mut self.network_failures, event.clone());
        let sequence = self.next_sequence();
        self.push_timeline(RuntimeObservatoryEvent {
            sequence,
            payload: RuntimeObservatoryEventPayload::NetworkFailure(event),
        });
    }

    pub fn push_request(&mut self, event: RequestSummaryEvent) {
        self.mark_active();
        push_ring(&mut self.requests, event.clone());
        let sequence = self.next_sequence();
        self.push_timeline(RuntimeObservatoryEvent {
            sequence,
            payload: RuntimeObservatoryEventPayload::RequestSummary(event),
        });
    }

    pub fn upsert_request_record(&mut self, mut record: NetworkRequestRecord) {
        self.mark_active();
        let next_sequence = self.next_request_sequence();
        if let Some(existing) = self.request_records.get_mut(&record.request_id) {
            let request_id = record.request_id.clone();
            merge_request_record(existing, record);
            existing.sequence = next_sequence;
            if let Some(position) = self.request_order.iter().position(|id| id == &request_id) {
                self.request_order.remove(position);
            }
            self.request_order.push_back(request_id);
            return;
        }

        let request_id = record.request_id.clone();
        record.sequence = next_sequence;
        self.request_records.insert(request_id.clone(), record);
        self.request_order.push_back(request_id);
        while self.request_order.len() > OBSERVATORY_REQUEST_RECORD_LIMIT {
            if let Some(oldest) = self.request_order.pop_front() {
                if let Some(evicted) = self.request_records.remove(&oldest) {
                    self.last_evicted_request_sequence =
                        self.last_evicted_request_sequence.max(evicted.sequence);
                }
                self.dropped_request_record_count =
                    self.dropped_request_record_count.saturating_add(1);
                self.mark_degraded("network_request_ring_overflow");
            }
        }
    }

    pub fn request_record(&self, request_id: &str) -> Option<NetworkRequestRecord> {
        self.request_records.get(request_id).cloned()
    }

    pub fn request_records(
        &self,
        last: Option<usize>,
        url_match: Option<&str>,
        method: Option<&str>,
        status: Option<u16>,
        lifecycle: Option<NetworkRequestLifecycle>,
    ) -> Vec<NetworkRequestRecord> {
        let method = method.map(|value| value.to_ascii_uppercase());
        let mut records = self
            .request_order
            .iter()
            .rev()
            .filter_map(|request_id| self.request_records.get(request_id))
            .filter(|record| {
                url_match
                    .map(|needle| record.url.contains(needle))
                    .unwrap_or(true)
                    && method
                        .as_deref()
                        .map(|needle| record.method.eq_ignore_ascii_case(needle))
                        .unwrap_or(true)
                    && status
                        .map(|value| record.status == Some(value))
                        .unwrap_or(true)
                    && lifecycle
                        .map(|value| record.lifecycle == value)
                        .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(last) = last {
            records.truncate(last);
        }
        records
    }

    pub fn request_records_after(&self, cursor: u64) -> Vec<NetworkRequestRecord> {
        self.request_order
            .iter()
            .filter_map(|request_id| self.request_records.get(request_id))
            .filter(|record| record.sequence > cursor)
            .cloned()
            .collect()
    }

    pub(crate) fn request_window_after(
        &self,
        cursor: u64,
        total_drop_count: u64,
        last_observed_drop_count: u64,
    ) -> NetworkRequestWindow {
        let records = self.request_records_after(cursor);
        let next_cursor = records
            .iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or_else(|| self.request_cursor());
        let dropped_since_last_poll = total_drop_count > last_observed_drop_count;
        let cursor_lost_to_eviction =
            dropped_since_last_poll && cursor < self.last_evicted_request_sequence;
        let degraded_reason = if cursor_lost_to_eviction {
            Some("network_request_ring_overflow".to_string())
        } else if self
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason != "network_request_ring_overflow")
        {
            self.degraded_reason.clone()
        } else {
            None
        };
        NetworkRequestWindow {
            records,
            next_cursor,
            authoritative: degraded_reason.is_none(),
            degraded_reason,
        }
    }

    pub fn dropped_request_record_count(&self) -> u64 {
        self.dropped_request_record_count
    }

    pub fn dropped_timeline_event_count(&self) -> u64 {
        self.dropped_timeline_event_count
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;
        sequence
    }

    fn next_request_sequence(&mut self) -> u64 {
        let sequence = self.next_request_sequence.max(1);
        self.next_request_sequence = sequence + 1;
        sequence
    }

    fn push_timeline(&mut self, event: RuntimeObservatoryEvent) {
        if self.timeline.len() == OBSERVATORY_TIMELINE_LIMIT
            && let Some(evicted) = self.timeline.pop_front()
        {
            self.last_evicted_timeline_sequence =
                self.last_evicted_timeline_sequence.max(evicted.sequence);
            self.dropped_timeline_event_count = self.dropped_timeline_event_count.saturating_add(1);
        }
        self.timeline.push_back(event);
    }
}

fn push_ring<T>(queue: &mut VecDeque<T>, event: T) {
    if queue.len() == OBSERVATORY_RING_LIMIT {
        queue.pop_front();
    }
    queue.push_back(event);
}

fn event_sequence(event: &RuntimeObservatoryEvent) -> u64 {
    event.sequence
}

fn merge_request_record(existing: &mut NetworkRequestRecord, incoming: NetworkRequestRecord) {
    if request_lifecycle_rank(incoming.lifecycle) >= request_lifecycle_rank(existing.lifecycle) {
        existing.lifecycle = incoming.lifecycle;
    }
    if !incoming.url.is_empty() {
        existing.url = incoming.url;
    }
    if !incoming.method.is_empty() {
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

fn request_lifecycle_rank(lifecycle: NetworkRequestLifecycle) -> u8 {
    match lifecycle {
        NetworkRequestLifecycle::Pending => 0,
        NetworkRequestLifecycle::Responded => 1,
        NetworkRequestLifecycle::Completed => 2,
        NetworkRequestLifecycle::Failed => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{ObservatoryEventWindow, RuntimeObservatoryState};
    use rub_core::model::{
        ConsoleErrorEvent, NetworkBodyPreview, NetworkRequestLifecycle, NetworkRequestRecord,
        RequestSummaryEvent, RuntimeObservatoryEventPayload, RuntimeObservatoryStatus,
    };
    use std::collections::BTreeMap;

    #[test]
    fn observatory_ring_buffers_are_bounded() {
        let mut state = RuntimeObservatoryState::default();
        for index in 0..40 {
            state.push_console_error(ConsoleErrorEvent {
                level: "error".to_string(),
                message: format!("console-{index}"),
                source: None,
            });
        }

        let projection = state.projection();
        assert_eq!(projection.status, RuntimeObservatoryStatus::Active);
        assert_eq!(projection.recent_console_errors.len(), 32);
        assert_eq!(projection.recent_console_errors[0].message, "console-8");
        assert_eq!(projection.recent_console_errors[31].message, "console-39");
    }

    #[test]
    fn raw_projection_reports_local_drop_counts_truthfully() {
        let mut state = RuntimeObservatoryState::default();
        for index in 0..1_030 {
            state.upsert_request_record(NetworkRequestRecord {
                request_id: format!("req-{index}"),
                sequence: 0,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: format!("https://example.com/api/{index}"),
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
            });
        }

        let projection = state.projection();
        assert_eq!(
            projection.dropped_event_count,
            state.dropped_request_record_count()
        );
        assert!(projection.dropped_event_count > 0);
    }

    #[test]
    fn observatory_timeline_reports_events_after_cursor() {
        let mut state = RuntimeObservatoryState::default();
        state.push_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "boom".to_string(),
            source: Some("main".to_string()),
        });
        let cursor = state.cursor();
        state.push_request(RequestSummaryEvent {
            request_id: "req-1".to_string(),
            url: "https://example.com/api".to_string(),
            method: "GET".to_string(),
            status: Some(200),
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
        });

        let events = state.events_after(cursor);
        assert_eq!(events.len(), 1);
        match &events[0].payload {
            RuntimeObservatoryEventPayload::RequestSummary(event) => {
                assert_eq!(events[0].sequence, cursor + 1);
                assert_eq!(event.url, "https://example.com/api");
            }
            other => panic!("unexpected observatory event: {other:?}"),
        }
    }

    #[test]
    fn request_records_are_upserted_and_merged() {
        let mut state = RuntimeObservatoryState::default();
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Responded,
            url: "https://example.com/api".to_string(),
            method: "POST".to_string(),
            tab_target_id: None,
            status: Some(202),
            request_headers: BTreeMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]),
            response_headers: BTreeMap::new(),
            request_body: Some(NetworkBodyPreview {
                available: true,
                preview: Some("{\"ok\":true}".to_string()),
                encoding: Some("text".to_string()),
                truncated: None,
                omitted_reason: None,
            }),
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: Some("main".to_string()),
            resource_type: Some("xhr".to_string()),
            mime_type: None,
        });
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api".to_string(),
            method: "POST".to_string(),
            tab_target_id: None,
            status: Some(202),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]),
            request_body: None,
            response_body: Some(NetworkBodyPreview {
                available: true,
                preview: Some("{\"done\":true}".to_string()),
                encoding: Some("text".to_string()),
                truncated: None,
                omitted_reason: None,
            }),
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: Some("application/json".to_string()),
        });

        let record = state.request_record("req-1").expect("record should exist");
        assert_eq!(record.sequence, 2);
        assert_eq!(record.lifecycle, NetworkRequestLifecycle::Completed);
        assert_eq!(record.request_headers["content-type"], "application/json");
        assert_eq!(record.response_headers["content-type"], "application/json");
        assert_eq!(
            record
                .response_body
                .as_ref()
                .and_then(|body| body.preview.as_deref()),
            Some("{\"done\":true}")
        );
    }

    #[test]
    fn request_record_merge_keeps_terminal_lifecycle_when_late_pending_arrives() {
        let mut state = RuntimeObservatoryState::default();
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api".to_string(),
            method: "POST".to_string(),
            tab_target_id: None,
            status: Some(200),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: Some(NetworkBodyPreview {
                available: true,
                preview: Some("done".to_string()),
                encoding: Some("text".to_string()),
                truncated: None,
                omitted_reason: None,
            }),
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: Some("text/plain".to_string()),
        });
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Pending,
            url: "https://example.com/api".to_string(),
            method: "POST".to_string(),
            tab_target_id: None,
            status: None,
            request_headers: BTreeMap::from([("x-test".to_string(), "1".to_string())]),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: Some("xhr".to_string()),
            mime_type: None,
        });

        let record = state.request_record("req-1").expect("record should exist");
        assert_eq!(record.lifecycle, NetworkRequestLifecycle::Completed);
        assert_eq!(record.request_headers["x-test"], "1");
        assert_eq!(
            record
                .response_body
                .as_ref()
                .and_then(|body| body.preview.as_deref()),
            Some("done")
        );
    }

    #[test]
    fn request_record_filters_honor_match_method_and_status() {
        let mut state = RuntimeObservatoryState::default();
        for (request_id, method, status, url) in [
            ("req-1", "GET", Some(200), "https://example.com/api/orders"),
            ("req-2", "POST", Some(201), "https://example.com/api/orders"),
            ("req-3", "GET", Some(404), "https://example.com/api/missing"),
        ] {
            state.upsert_request_record(NetworkRequestRecord {
                request_id: request_id.to_string(),
                sequence: 0,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: url.to_string(),
                method: method.to_string(),
                tab_target_id: None,
                status,
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
            });
        }

        let matched = state.request_records(
            Some(1),
            Some("/api/orders"),
            Some("post"),
            Some(201),
            Some(NetworkRequestLifecycle::Completed),
        );
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].request_id, "req-2");
    }

    #[test]
    fn request_records_after_returns_only_newer_sequences_in_order() {
        let mut state = RuntimeObservatoryState::default();
        for (request_id, url) in [
            ("req-1", "https://example.com/api/one"),
            ("req-2", "https://example.com/api/two"),
            ("req-3", "https://example.com/api/three"),
        ] {
            state.upsert_request_record(NetworkRequestRecord {
                request_id: request_id.to_string(),
                sequence: 0,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: url.to_string(),
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
            });
        }

        let after_first = state.request_records_after(1);
        assert_eq!(after_first.len(), 2);
        assert_eq!(after_first[0].request_id, "req-2");
        assert_eq!(after_first[1].request_id, "req-3");
    }

    #[test]
    fn request_window_after_ignores_sequence_gaps_from_same_request_updates() {
        let mut state = RuntimeObservatoryState::default();
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Pending,
            url: "https://example.com/api".to_string(),
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
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: None,
        });
        let cursor = state.request_cursor();
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Responded,
            url: "https://example.com/api".to_string(),
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
        });
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api".to_string(),
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
        });

        let window = state.request_window_after(cursor, 0, 0);
        assert!(window.authoritative);
        assert_eq!(window.degraded_reason, None);
        assert_eq!(window.records.len(), 1);
        assert_eq!(window.records[0].request_id, "req-1");
        assert_eq!(
            window.records[0].lifecycle,
            NetworkRequestLifecycle::Completed
        );
    }

    #[test]
    fn request_window_after_treats_request_record_eviction_as_non_authoritative() {
        let mut state = RuntimeObservatoryState::default();
        for index in 0..1_030 {
            state.upsert_request_record(NetworkRequestRecord {
                request_id: format!("req-{index}"),
                sequence: 0,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: format!("https://example.com/api/{index}"),
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
            });
        }

        let total_drop_count = state.dropped_request_record_count();
        let window = state.request_window_after(1, total_drop_count, 0);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("network_request_ring_overflow")
        );
        assert!(!window.records.is_empty());
    }

    #[test]
    fn request_window_recovers_authority_after_cursor_advances_past_eviction_boundary() {
        let mut state = RuntimeObservatoryState::default();
        for index in 0..1_025 {
            state.upsert_request_record(NetworkRequestRecord {
                request_id: format!("req-{index}"),
                sequence: 0,
                lifecycle: NetworkRequestLifecycle::Completed,
                url: format!("https://example.com/api/{index}"),
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
            });
        }

        let total_drop_count = state.dropped_request_record_count();
        let lost_window = state.request_window_after(0, total_drop_count, 0);
        assert!(!lost_window.authoritative);
        assert_eq!(
            lost_window.degraded_reason.as_deref(),
            Some("network_request_ring_overflow")
        );

        let recovered_cursor = state.last_evicted_request_sequence;
        let window = state.request_window_after(recovered_cursor, total_drop_count, 0);
        assert!(window.authoritative);
        assert_eq!(window.degraded_reason, None);
    }

    #[test]
    fn request_window_treats_non_overflow_degradation_as_non_authoritative() {
        let mut state = RuntimeObservatoryState::default();
        state.mark_degraded("listener_install_failed");
        let total_drop_count = state.dropped_request_record_count();
        let window = state.request_window_after(0, total_drop_count, total_drop_count);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("listener_install_failed")
        );
    }

    #[test]
    fn observatory_event_window_reports_timeline_eviction_truthfully() {
        let mut state = RuntimeObservatoryState::default();
        for index in 0..70 {
            state.push_console_error(ConsoleErrorEvent {
                level: "error".to_string(),
                message: format!("console-{index}"),
                source: None,
            });
        }

        let total_drop_count = state.dropped_timeline_event_count();
        let window: ObservatoryEventWindow = state.event_window_after(0, total_drop_count, 0);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("observatory_timeline_overflow")
        );
        assert!(!window.events.is_empty());
    }
}
