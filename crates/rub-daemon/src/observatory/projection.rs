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
        let degraded_reason =
            top_level_observatory_degraded_reason(self.degraded_reason.as_deref())
                .map(str::to_string);
        let status = if degraded_reason.is_none()
            && matches!(
                self.status,
                rub_core::model::RuntimeObservatoryStatus::Degraded
            ) {
            rub_core::model::RuntimeObservatoryStatus::Active
        } else {
            self.status
        };
        RuntimeObservatoryInfo {
            status,
            recent_console_errors: self.console_errors.iter().cloned().collect(),
            recent_page_errors: self.page_errors.iter().cloned().collect(),
            recent_network_failures: self.network_failures.iter().cloned().collect(),
            recent_requests: self.requests.iter().cloned().collect(),
            dropped_event_count,
            degraded_reason,
        }
    }

    pub(crate) fn event_window_between(
        &self,
        cursor: u64,
        end_cursor: u64,
        ingress_drop_count: u64,
        last_observed_ingress_drop_count: u64,
    ) -> ObservatoryEventWindow {
        let events = self.events_between(cursor, end_cursor);
        let cursor_lost_to_eviction = cursor < self.last_evicted_timeline_sequence;
        let ingress_loss_moved = ingress_drop_count > last_observed_ingress_drop_count;
        let degraded_reason = if cursor_lost_to_eviction {
            Some("observatory_timeline_overflow".to_string())
        } else if ingress_loss_moved {
            Some("observatory_ingress_overflow".to_string())
        } else if self
            .degraded_reason
            .as_deref()
            .is_some_and(|reason| reason != "network_request_ring_overflow")
        {
            self.degraded_reason.clone()
        } else {
            None
        };
        ObservatoryEventWindow {
            events,
            authoritative: degraded_reason.is_none(),
            degraded_reason,
        }
    }
}

fn top_level_observatory_degraded_reason(reason: Option<&str>) -> Option<&str> {
    match reason {
        Some("network_request_ring_overflow") => None,
        _ => reason,
    }
}
