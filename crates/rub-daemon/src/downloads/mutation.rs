use super::*;
use crate::downloads::events::{event_kind_for_state, is_terminal};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

impl DownloadRuntimeState {
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
            self.next_sequence = 0;
            self.projection = DownloadRuntimeInfo::default();
            self.entries.clear();
            self.entry_event_sequence.clear();
            self.active_order.clear();
            self.completed_order.clear();
            self.timeline.clear();
        }
        true
    }

    pub fn set_runtime(
        &mut self,
        generation: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
    ) -> DownloadRuntimeInfo {
        if !self.prepare_generation(generation) {
            return self.projection();
        }
        self.projection.status = status;
        self.projection.mode = mode;
        self.projection.download_dir = download_dir;
        self.projection.degraded_reason = None;
        self.projection()
    }

    pub fn mark_degraded(
        &mut self,
        generation: u64,
        reason: impl Into<String>,
    ) -> DownloadRuntimeInfo {
        if !self.prepare_generation(generation) {
            return self.projection();
        }
        self.projection.status = DownloadRuntimeStatus::Degraded;
        self.projection.degraded_reason = Some(reason.into());
        self.projection()
    }

    pub fn record_started(
        &mut self,
        generation: u64,
        sequence: u64,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    ) -> DownloadEntry {
        if !self.prepare_generation(generation) {
            return self
                .entries
                .get(&guid)
                .cloned()
                .unwrap_or_else(|| DownloadEntry {
                    guid,
                    state: DownloadState::Started,
                    url: Some(url),
                    suggested_filename: Some(suggested_filename),
                    final_path: None,
                    mime_hint: None,
                    received_bytes: 0,
                    total_bytes: None,
                    started_at: rfc3339_now(),
                    completed_at: None,
                    frame_id,
                    trigger_command_id: None,
                });
        }
        let started_at = rfc3339_now();
        let last_sequence = self.entry_event_sequence.get(&guid).copied().unwrap_or(0);
        if sequence <= last_sequence
            || self
                .entries
                .get(&guid)
                .is_some_and(|entry| is_terminal(entry.state))
        {
            return self
                .entries
                .get(&guid)
                .cloned()
                .unwrap_or_else(|| DownloadEntry {
                    guid: guid.clone(),
                    state: DownloadState::Started,
                    url: Some(url.clone()),
                    suggested_filename: Some(suggested_filename.clone()),
                    final_path: None,
                    mime_hint: None,
                    received_bytes: 0,
                    total_bytes: None,
                    started_at: started_at.clone(),
                    completed_at: None,
                    frame_id: frame_id.clone(),
                    trigger_command_id: None,
                });
        }
        let entry = self
            .entries
            .entry(guid.clone())
            .or_insert_with(|| DownloadEntry {
                guid: guid.clone(),
                state: DownloadState::Started,
                url: None,
                suggested_filename: None,
                final_path: None,
                mime_hint: None,
                received_bytes: 0,
                total_bytes: None,
                started_at: started_at.clone(),
                completed_at: None,
                frame_id: None,
                trigger_command_id: None,
            });
        entry.state = DownloadState::Started;
        entry.url = Some(url);
        entry.suggested_filename = Some(suggested_filename);
        entry.frame_id = frame_id;
        if entry.started_at.is_empty() {
            entry.started_at = started_at;
        }
        self.entry_event_sequence.insert(guid.clone(), sequence);
        let snapshot = entry.clone();
        self.move_to_active(guid.clone());
        self.projection.last_download = Some(snapshot.clone());
        self.record_event(DownloadEventKind::Started, snapshot.clone());
        snapshot
    }

    pub fn record_progress(&mut self, event: DownloadProgressEvent) -> Option<DownloadEntry> {
        if !self.prepare_generation(event.generation) {
            return self.entries.get(&event.guid).cloned();
        }
        let last_sequence = self
            .entry_event_sequence
            .get(&event.guid)
            .copied()
            .unwrap_or(0);
        if event.sequence <= last_sequence {
            return self.entries.get(&event.guid).cloned();
        }
        let now = rfc3339_now();
        let entry = self
            .entries
            .entry(event.guid.clone())
            .or_insert_with(|| DownloadEntry {
                guid: event.guid.clone(),
                state: DownloadState::Started,
                url: None,
                suggested_filename: None,
                final_path: None,
                mime_hint: None,
                received_bytes: 0,
                total_bytes: None,
                started_at: now.clone(),
                completed_at: None,
                frame_id: None,
                trigger_command_id: None,
            });
        if is_terminal(entry.state) && !is_terminal(event.state) {
            return Some(entry.clone());
        }
        entry.state = event.state;
        entry.received_bytes = entry.received_bytes.max(event.received_bytes);
        if event.total_bytes.is_some() {
            entry.total_bytes = event.total_bytes;
        }
        if event.final_path.is_some() {
            entry.final_path = event.final_path;
        }
        if is_terminal(event.state) {
            entry.completed_at = Some(now);
        }
        self.entry_event_sequence
            .insert(event.guid.clone(), event.sequence);
        let snapshot = entry.clone();
        if is_terminal(event.state) {
            self.move_to_completed(event.guid);
        } else {
            self.move_to_active(event.guid);
        }
        self.projection.last_download = Some(snapshot.clone());
        self.record_event(event_kind_for_state(event.state), snapshot.clone());
        Some(snapshot)
    }

    fn move_to_active(&mut self, guid: String) {
        self.active_order.retain(|existing| existing != &guid);
        self.completed_order.retain(|existing| existing != &guid);
        self.active_order.push_back(guid);
        while self.active_order.len() > ACTIVE_DOWNLOAD_LIMIT {
            if let Some(oldest) = self.active_order.pop_front()
                && !self.completed_order.iter().any(|guid| guid == &oldest)
            {
                self.entries.remove(&oldest);
                self.entry_event_sequence.remove(&oldest);
            }
        }
    }

    fn move_to_completed(&mut self, guid: String) {
        self.active_order.retain(|existing| existing != &guid);
        self.completed_order.retain(|existing| existing != &guid);
        self.completed_order.push_back(guid);
        while self.completed_order.len() > COMPLETED_DOWNLOAD_LIMIT {
            if let Some(oldest) = self.completed_order.pop_front()
                && !self.active_order.iter().any(|guid| guid == &oldest)
            {
                self.entries.remove(&oldest);
                self.entry_event_sequence.remove(&oldest);
            }
        }
    }
}

fn rfc3339_now() -> String {
    // Rfc3339 formatting of OffsetDateTime::now_utc() is infallible in
    // practice. Sentinel is non-epoch to make format failures visible
    // rather than silently injecting a valid-looking "1970" timestamp.
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "TIMESTAMP_FORMAT_ERROR".to_string())
}
