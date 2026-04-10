use super::*;

impl RuntimeObservatoryState {
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
            .filter(|event| event.sequence > cursor)
            .cloned()
            .collect()
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

    pub(super) fn next_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;
        sequence
    }

    pub(super) fn next_request_sequence(&mut self) -> u64 {
        let sequence = self.next_request_sequence.max(1);
        self.next_request_sequence = sequence + 1;
        sequence
    }

    pub(super) fn push_timeline(&mut self, event: RuntimeObservatoryEvent) {
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
