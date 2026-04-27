use std::collections::{HashMap, VecDeque};

use rub_core::model::{
    ConsoleErrorEvent, NetworkFailureEvent, NetworkRequestRecord, PageErrorEvent,
    RequestSummaryEvent, RuntimeObservatoryEvent, RuntimeObservatoryEventPayload,
    RuntimeObservatoryInfo, RuntimeObservatoryStatus,
};

mod events;
mod projection;
mod requests;

pub(crate) use projection::{NetworkRequestWindow, ObservatoryEventWindow};

const OBSERVATORY_RING_LIMIT: usize = 32;
const OBSERVATORY_TIMELINE_LIMIT: usize = 64;
const OBSERVATORY_REQUEST_RECORD_LIMIT: usize = 1_024;

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

    pub fn dropped_request_record_count(&self) -> u64 {
        self.dropped_request_record_count
    }

    pub fn dropped_timeline_event_count(&self) -> u64 {
        self.dropped_timeline_event_count
    }
}

#[cfg(test)]
mod tests {
    use super::{ObservatoryEventWindow, RuntimeObservatoryState};
    use rub_core::model::{
        ConsoleErrorEvent, NetworkBodyPreview, NetworkRequestLifecycle, NetworkRequestRecord,
        ObservedNetworkRequestRecord, RequestSummaryEvent, RuntimeObservatoryEventPayload,
        RuntimeObservatoryStatus,
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
        assert_eq!(projection.status, RuntimeObservatoryStatus::Active);
        assert_eq!(projection.degraded_reason, None);
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
    fn observed_request_records_receive_sequence_only_at_daemon_authority() {
        let mut state = RuntimeObservatoryState::default();
        state.upsert_observed_request_record(ObservedNetworkRequestRecord {
            request_id: "req-obs".to_string(),
            lifecycle: NetworkRequestLifecycle::Pending,
            url: "https://example.com/obs".to_string(),
            method: "GET".to_string(),
            tab_target_id: Some("tab-1".to_string()),
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
            resource_type: Some("xhr".to_string()),
            mime_type: None,
        });

        let record = state
            .request_record("req-obs")
            .expect("daemon should publish authoritative request record");
        assert_eq!(record.sequence, 1);
        assert_eq!(record.lifecycle, NetworkRequestLifecycle::Pending);
        assert_eq!(record.url, "https://example.com/obs");
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

        let window = state.request_window_after(1, 0, 0);
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

        let lost_window = state.request_window_after(0, 0, 0);
        assert!(!lost_window.authoritative);
        assert_eq!(
            lost_window.degraded_reason.as_deref(),
            Some("network_request_ring_overflow")
        );

        let recovered_cursor = state.last_evicted_request_sequence;
        let window = state.request_window_after(recovered_cursor, 0, 0);
        assert!(window.authoritative);
        assert_eq!(window.degraded_reason, None);
    }

    #[test]
    fn request_window_keeps_stale_cursor_degraded_after_drop_count_stops_moving() {
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

        let lost_window = state.request_window_after(0, 0, 0);
        assert!(!lost_window.authoritative);
        assert_eq!(
            lost_window.degraded_reason.as_deref(),
            Some("network_request_ring_overflow")
        );

        let washed_window = state.request_window_after(0, 0, 0);
        assert!(
            !washed_window.authoritative,
            "the same stale cursor must stay degraded even when drop count has stopped moving"
        );
        assert_eq!(
            washed_window.degraded_reason.as_deref(),
            Some("network_request_ring_overflow")
        );
    }

    #[test]
    fn request_window_treats_non_overflow_degradation_as_non_authoritative() {
        let mut state = RuntimeObservatoryState::default();
        state.mark_degraded("listener_install_failed");
        let window = state.request_window_after(0, 0, 0);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("listener_install_failed")
        );
    }

    #[test]
    fn request_record_overflow_does_not_overwrite_stronger_top_level_degraded_reason() {
        let mut state = RuntimeObservatoryState::default();
        state.mark_degraded("listener_install_failed");
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
        assert_eq!(projection.status, RuntimeObservatoryStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("listener_install_failed")
        );
        assert!(projection.dropped_event_count > 0);
    }

    #[test]
    fn request_window_treats_ingress_drop_delta_as_non_authoritative() {
        let mut state = RuntimeObservatoryState::default();
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api/one".to_string(),
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
        let cursor = state.request_cursor();
        state.upsert_request_record(NetworkRequestRecord {
            request_id: "req-2".to_string(),
            sequence: 0,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.com/api/two".to_string(),
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

        let window = state.request_window_after(cursor, 1, 0);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("network_request_ingress_overflow")
        );
        assert_eq!(window.records.len(), 1);
        assert_eq!(window.records[0].request_id, "req-2");
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

        let window: ObservatoryEventWindow = state.event_window_between(0, state.cursor(), 0, 0);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("observatory_timeline_overflow")
        );
        assert!(!window.events.is_empty());
    }

    #[test]
    fn observatory_event_window_keeps_stale_cursor_degraded_after_drop_count_stops_moving() {
        let mut state = RuntimeObservatoryState::default();
        for index in 0..70 {
            state.push_console_error(ConsoleErrorEvent {
                level: "error".to_string(),
                message: format!("console-{index}"),
                source: None,
            });
        }

        let lost_window: ObservatoryEventWindow =
            state.event_window_between(0, state.cursor(), 0, 0);
        assert!(!lost_window.authoritative);
        assert_eq!(
            lost_window.degraded_reason.as_deref(),
            Some("observatory_timeline_overflow")
        );

        let washed_window: ObservatoryEventWindow =
            state.event_window_between(0, state.cursor(), 0, 0);
        assert!(
            !washed_window.authoritative,
            "the same stale cursor must stay degraded even when no new drops arrive"
        );
        assert_eq!(
            washed_window.degraded_reason.as_deref(),
            Some("observatory_timeline_overflow")
        );
    }

    #[test]
    fn observatory_event_window_treats_ingress_drop_delta_as_non_authoritative() {
        let mut state = RuntimeObservatoryState::default();
        state.push_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "before".to_string(),
            source: None,
        });
        let cursor = state.cursor();
        state.push_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "after".to_string(),
            source: None,
        });

        let window: ObservatoryEventWindow =
            state.event_window_between(cursor, state.cursor(), 1, 0);
        assert!(!window.authoritative);
        assert_eq!(
            window.degraded_reason.as_deref(),
            Some("observatory_ingress_overflow")
        );
        assert_eq!(window.events.len(), 1);
    }

    #[test]
    fn observatory_event_window_ignores_request_ring_overflow_when_timeline_authority_holds() {
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

        let cursor = state.cursor();
        state.push_console_error(ConsoleErrorEvent {
            level: "error".to_string(),
            message: "after-request-overflow".to_string(),
            source: None,
        });

        let window: ObservatoryEventWindow =
            state.event_window_between(cursor, state.cursor(), 0, 0);
        assert!(
            window.authoritative,
            "request-record overflow must not invalidate unrelated event-window authority"
        );
        assert_eq!(window.degraded_reason, None);
        assert_eq!(window.events.len(), 1);
    }
}
