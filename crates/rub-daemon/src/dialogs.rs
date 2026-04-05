use rub_core::model::{
    DialogKind, DialogResolutionInfo, DialogRuntimeInfo, DialogRuntimeStatus, PendingDialogInfo,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Default)]
pub struct DialogRuntimeState {
    projection: DialogRuntimeInfo,
    last_event_sequence: u64,
    current_generation: u64,
}

#[derive(Debug)]
pub struct DialogOpenedEvent {
    pub generation: u64,
    pub sequence: u64,
    pub pending: PendingDialogInfo,
}

impl DialogRuntimeState {
    fn effective_generation(&self, generation: u64) -> u64 {
        if generation == 0 {
            self.current_generation
        } else {
            generation
        }
    }

    fn prepare_generation(&mut self, generation: u64) -> bool {
        let generation = self.effective_generation(generation);
        if generation < self.current_generation {
            return false;
        }
        if generation > self.current_generation {
            self.current_generation = generation;
            self.projection = DialogRuntimeInfo::default();
            self.last_event_sequence = 0;
        }
        true
    }

    pub fn projection(&self) -> DialogRuntimeInfo {
        self.projection.clone()
    }

    pub fn replace_projection(
        &mut self,
        generation: u64,
        projection: DialogRuntimeInfo,
    ) -> DialogRuntimeInfo {
        if !self.prepare_generation(generation) {
            return self.projection();
        }
        self.projection = projection;
        self.projection()
    }

    pub fn set_runtime(
        &mut self,
        generation: u64,
        status: DialogRuntimeStatus,
    ) -> DialogRuntimeInfo {
        if !self.prepare_generation(generation) {
            return self.projection();
        }
        self.projection.status = status;
        if !matches!(status, DialogRuntimeStatus::Degraded) {
            self.projection.degraded_reason = None;
        }
        self.projection()
    }

    pub fn mark_degraded(
        &mut self,
        generation: u64,
        reason: impl Into<String>,
    ) -> DialogRuntimeInfo {
        if !self.prepare_generation(generation) {
            return self.projection();
        }
        self.projection.status = DialogRuntimeStatus::Degraded;
        self.projection.degraded_reason = Some(reason.into());
        self.projection()
    }

    fn apply_runtime_status(&mut self, status: DialogRuntimeStatus) {
        if self.projection.degraded_reason.is_some() {
            self.projection.status = DialogRuntimeStatus::Degraded;
        } else {
            self.projection.status = status;
        }
    }

    pub fn record_opened(&mut self, event: DialogOpenedEvent) -> DialogRuntimeInfo {
        if !self.prepare_generation(event.generation) {
            return self.projection();
        }
        if event.sequence <= self.last_event_sequence {
            return self.projection();
        }
        self.last_event_sequence = event.sequence;
        let pending = event.pending;
        self.apply_runtime_status(DialogRuntimeStatus::Active);
        self.projection.last_result = None;
        self.projection.last_dialog = Some(pending.clone());
        self.projection.pending_dialog = Some(pending);
        self.projection()
    }

    pub fn record_closed(
        &mut self,
        generation: u64,
        sequence: u64,
        accepted: bool,
        user_input: String,
    ) -> DialogRuntimeInfo {
        if !self.prepare_generation(generation) {
            return self.projection();
        }
        if sequence <= self.last_event_sequence {
            return self.projection();
        }
        self.last_event_sequence = sequence;
        let prompt_input = self
            .projection
            .last_dialog
            .as_ref()
            .filter(|dialog| matches!(dialog.kind, DialogKind::Prompt))
            .map(|_| user_input);
        self.apply_runtime_status(DialogRuntimeStatus::Inactive);
        self.projection.pending_dialog = None;
        self.projection.last_result = Some(DialogResolutionInfo {
            accepted,
            user_input: prompt_input,
            closed_at: rfc3339_now(),
        });
        self.projection()
    }
}

pub fn rfc3339_now() -> String {
    // Rfc3339 formatting of OffsetDateTime::now_utc() is infallible in
    // practice. Sentinel is non-epoch to make format failures visible
    // rather than silently injecting a valid-looking "1970" timestamp.
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "TIMESTAMP_FORMAT_ERROR".to_string())
}

#[cfg(test)]
mod tests {
    use super::{DialogOpenedEvent, DialogRuntimeState};
    use rub_core::model::{
        DialogKind, DialogResolutionInfo, DialogRuntimeInfo, DialogRuntimeStatus, PendingDialogInfo,
    };

    #[test]
    fn dialog_runtime_tracks_open_and_close_lifecycle() {
        let mut state = DialogRuntimeState::default();
        state.set_runtime(0, DialogRuntimeStatus::Active);
        let opened = state.record_opened(prompt_event(0, 1, "Enter value"));
        assert!(opened.pending_dialog.is_some());
        assert_eq!(
            opened
                .pending_dialog
                .as_ref()
                .and_then(|dialog| dialog.tab_target_id.as_deref()),
            Some("target-1")
        );

        let closed = state.record_closed(0, 2, true, "typed".to_string());
        assert!(closed.pending_dialog.is_none());
        assert_eq!(
            closed
                .last_result
                .as_ref()
                .and_then(|result| result.user_input.as_deref()),
            Some("typed")
        );
    }

    #[test]
    fn replace_projection_swaps_in_browser_authority_snapshot() {
        let mut state = DialogRuntimeState::default();
        let projection = DialogRuntimeInfo {
            status: DialogRuntimeStatus::Active,
            pending_dialog: None,
            last_dialog: Some(PendingDialogInfo {
                kind: DialogKind::Alert,
                message: "Hello".to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "2026-01-01T00:00:00Z".to_string(),
            }),
            last_result: Some(DialogResolutionInfo {
                accepted: true,
                user_input: None,
                closed_at: "2026-01-01T00:00:01Z".to_string(),
            }),
            degraded_reason: None,
        };

        let replaced = state.replace_projection(0, projection.clone());
        assert_eq!(replaced, projection);
    }

    #[test]
    fn stale_open_event_does_not_republish_closed_dialog() {
        let mut state = DialogRuntimeState::default();
        state.record_opened(alert_event(0, 10, "Hello"));
        state.record_closed(0, 11, true, "".to_string());

        let projection = state.record_opened(alert_event(0, 9, "Late"));

        assert!(projection.pending_dialog.is_none());
        assert!(projection.last_result.is_some());
    }

    #[test]
    fn newer_generation_resets_projection_and_rejects_stale_events() {
        let mut state = DialogRuntimeState::default();
        state.record_opened(alert_event(1, 5, "generation-1"));
        state.record_closed(1, 6, true, "".to_string());

        let projection = state.record_opened(alert_event(2, 1, "generation-2"));
        assert_eq!(
            projection
                .pending_dialog
                .as_ref()
                .map(|dialog| dialog.message.as_str()),
            Some("generation-2")
        );

        let stale = state.record_opened(alert_event(1, 7, "late-generation-1"));
        assert_eq!(
            stale
                .pending_dialog
                .as_ref()
                .map(|dialog| dialog.message.as_str()),
            Some("generation-2")
        );
    }

    #[test]
    fn degraded_runtime_stays_degraded_across_open_and_close_events() {
        let mut state = DialogRuntimeState::default();
        state.mark_degraded(0, "listener_failed");

        let opened = state.record_opened(alert_event(0, 1, "Hello"));
        assert_eq!(opened.status, DialogRuntimeStatus::Degraded);
        assert_eq!(opened.degraded_reason.as_deref(), Some("listener_failed"));

        let closed = state.record_closed(0, 2, true, "".to_string());
        assert_eq!(closed.status, DialogRuntimeStatus::Degraded);
        assert_eq!(closed.degraded_reason.as_deref(), Some("listener_failed"));
    }

    fn prompt_event(generation: u64, sequence: u64, message: &str) -> DialogOpenedEvent {
        DialogOpenedEvent {
            generation,
            sequence,
            pending: PendingDialogInfo {
                kind: DialogKind::Prompt,
                message: message.to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: Some("frame-1".to_string()),
                default_prompt: Some("seed".to_string()),
                has_browser_handler: true,
                opened_at: "1970-01-01T00:00:00Z".to_string(),
            },
        }
    }

    fn alert_event(generation: u64, sequence: u64, message: &str) -> DialogOpenedEvent {
        DialogOpenedEvent {
            generation,
            sequence,
            pending: PendingDialogInfo {
                kind: DialogKind::Alert,
                message: message.to_string(),
                url: "https://example.com".to_string(),
                tab_target_id: Some("target-1".to_string()),
                frame_id: None,
                default_prompt: None,
                has_browser_handler: true,
                opened_at: "1970-01-01T00:00:00Z".to_string(),
            },
        }
    }
}
