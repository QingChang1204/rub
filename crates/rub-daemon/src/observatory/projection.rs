use rub_core::model::{RuntimeObservatoryEvent, RuntimeObservatoryInfo};

use super::RuntimeObservatoryState;

#[derive(Debug, Clone)]
pub(crate) struct NetworkRequestWindow {
    pub(crate) records: Vec<rub_core::model::NetworkRequestRecord>,
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

impl RuntimeObservatoryState {
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
}
