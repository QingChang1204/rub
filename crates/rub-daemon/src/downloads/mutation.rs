use super::*;
use crate::downloads::events::{event_kind_for_state, is_terminal};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

impl DownloadRuntimeState {
    fn backfill_timeline_metadata(
        &mut self,
        guid: &str,
        url: &str,
        suggested_filename: &str,
        frame_id: Option<&str>,
    ) {
        for event in &mut self.timeline {
            if event.download.guid != guid {
                continue;
            }
            if event.download.url.is_none() {
                event.download.url = Some(url.to_string());
            }
            if event.download.suggested_filename.is_none() {
                event.download.suggested_filename = Some(suggested_filename.to_string());
            }
            if event.download.frame_id.is_none() {
                event.download.frame_id = frame_id.map(str::to_string);
            }
        }
    }

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
            self.runtime_event_sequence = 0;
            self.projection = DownloadRuntimeInfo::default();
            self.entries.clear();
            self.entry_event_sequence.clear();
            self.active_order.clear();
            self.completed_order.clear();
            self.timeline.clear();
        }
        true
    }

    fn prepare_runtime_event_order(
        &mut self,
        generation: u64,
        browser_sequence: Option<u64>,
    ) -> bool {
        if !self.prepare_generation(generation) {
            return false;
        }
        let Some(browser_sequence) = browser_sequence.filter(|sequence| *sequence > 0) else {
            return true;
        };
        if browser_sequence <= self.runtime_event_sequence {
            return false;
        }
        self.runtime_event_sequence = browser_sequence;
        true
    }

    pub fn set_runtime(
        &mut self,
        generation: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
    ) -> DownloadRuntimeInfo {
        self.apply_runtime_event(generation, None, status, mode, download_dir, None)
            .projection
    }

    pub fn apply_runtime_event_sequenced(
        &mut self,
        generation: u64,
        browser_sequence: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
        degraded_reason: Option<String>,
    ) -> DownloadRuntimeMutationOutcome {
        self.apply_runtime_event(
            generation,
            Some(browser_sequence),
            status,
            mode,
            download_dir,
            degraded_reason,
        )
    }

    fn apply_runtime_event(
        &mut self,
        generation: u64,
        browser_sequence: Option<u64>,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
        degraded_reason: Option<String>,
    ) -> DownloadRuntimeMutationOutcome {
        if !self.prepare_runtime_event_order(generation, browser_sequence) {
            return DownloadRuntimeMutationOutcome {
                projection: self.projection(),
                applied: false,
            };
        }
        self.projection.status = if degraded_reason.is_some() {
            DownloadRuntimeStatus::Degraded
        } else {
            status
        };
        self.projection.mode = mode;
        self.projection.download_dir = download_dir;
        self.projection.degraded_reason = degraded_reason;
        DownloadRuntimeMutationOutcome {
            projection: self.projection(),
            applied: true,
        }
    }

    pub fn mark_degraded(
        &mut self,
        generation: u64,
        reason: impl Into<String>,
    ) -> DownloadRuntimeInfo {
        self.mark_degraded_sequenced(generation, None, reason)
            .projection
    }

    pub fn mark_degraded_browser_event(
        &mut self,
        generation: u64,
        browser_sequence: u64,
        reason: impl Into<String>,
    ) -> DownloadRuntimeInfo {
        self.mark_degraded_sequenced(generation, Some(browser_sequence), reason)
            .projection
    }

    fn mark_degraded_sequenced(
        &mut self,
        generation: u64,
        browser_sequence: Option<u64>,
        reason: impl Into<String>,
    ) -> DownloadRuntimeMutationOutcome {
        if !self.prepare_runtime_event_order(generation, browser_sequence) {
            return DownloadRuntimeMutationOutcome {
                projection: self.projection(),
                applied: false,
            };
        }
        self.projection.status = DownloadRuntimeStatus::Degraded;
        self.projection.degraded_reason = Some(reason.into());
        DownloadRuntimeMutationOutcome {
            projection: self.projection(),
            applied: true,
        }
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
        let existing_state = self.entries.get(&guid).map(|entry| entry.state);
        if sequence <= last_sequence {
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
        if existing_state.is_some_and(|state| !matches!(state, DownloadState::Started)) {
            {
                let entry = self
                    .entries
                    .get_mut(&guid)
                    .expect("existing download entry should remain available");
                entry.url = Some(url.clone());
                entry.suggested_filename = Some(suggested_filename.clone());
                entry.frame_id = frame_id.clone();
                if entry.started_at.is_empty() {
                    entry.started_at = started_at;
                }
            }
            self.entry_event_sequence.insert(guid.clone(), sequence);
            self.backfill_timeline_metadata(&guid, &url, &suggested_filename, frame_id.as_deref());
            let snapshot = self
                .entries
                .get(&guid)
                .cloned()
                .expect("existing download entry should remain available");
            if self
                .projection
                .last_download
                .as_ref()
                .is_some_and(|download| download.guid == guid)
            {
                self.projection.last_download = Some(snapshot.clone());
            }
            return snapshot;
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
