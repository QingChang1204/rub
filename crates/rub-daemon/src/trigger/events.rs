use super::*;

pub(super) fn event_kind_for_result_status(status: TriggerStatus) -> TriggerEventKind {
    match status {
        TriggerStatus::Fired => TriggerEventKind::Fired,
        TriggerStatus::Blocked => TriggerEventKind::Blocked,
        TriggerStatus::Degraded => TriggerEventKind::Degraded,
        TriggerStatus::Armed => TriggerEventKind::Resumed,
        TriggerStatus::Paused => TriggerEventKind::Paused,
        TriggerStatus::Expired => TriggerEventKind::Degraded,
    }
}

impl TriggerRuntimeState {
    pub(super) fn push_event(&mut self, mut event: TriggerEventInfo) {
        let sequence = self.next_event_sequence.max(1);
        self.next_event_sequence = sequence + 1;
        event.sequence = sequence;
        self.recent_events.push_back(event);
        while self.recent_events.len() > TRIGGER_EVENT_LIMIT {
            self.recent_events.pop_front();
        }
    }
}
