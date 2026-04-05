use std::collections::{HashMap, VecDeque};

use rub_core::model::{
    DownloadEntry, DownloadEvent, DownloadEventKind, DownloadMode, DownloadRuntimeInfo,
    DownloadRuntimeStatus, DownloadState,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const ACTIVE_DOWNLOAD_LIMIT: usize = 32;
const COMPLETED_DOWNLOAD_LIMIT: usize = 64;
const DOWNLOAD_EVENT_LIMIT: usize = 128;

#[derive(Debug, Default)]
pub struct DownloadRuntimeState {
    next_sequence: u64,
    projection: DownloadRuntimeInfo,
    entries: HashMap<String, DownloadEntry>,
    entry_event_sequence: HashMap<String, u64>,
    active_order: VecDeque<String>,
    completed_order: VecDeque<String>,
    timeline: VecDeque<DownloadEvent>,
    current_generation: u64,
}

#[derive(Debug)]
pub struct DownloadProgressEvent {
    pub generation: u64,
    pub sequence: u64,
    pub guid: String,
    pub state: DownloadState,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub final_path: Option<String>,
}

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

    pub fn projection(&self) -> DownloadRuntimeInfo {
        DownloadRuntimeInfo {
            status: self.projection.status,
            mode: self.projection.mode,
            download_dir: self.projection.download_dir.clone(),
            active_downloads: self
                .active_order
                .iter()
                .filter_map(|guid| self.entries.get(guid))
                .cloned()
                .collect(),
            completed_downloads: self
                .completed_order
                .iter()
                .filter_map(|guid| self.entries.get(guid))
                .cloned()
                .collect(),
            last_download: self.projection.last_download.clone(),
            degraded_reason: self.projection.degraded_reason.clone(),
        }
    }

    pub fn cursor(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    pub fn events_after(&self, cursor: u64) -> Vec<DownloadEvent> {
        self.timeline
            .iter()
            .filter(|event| event.sequence > cursor)
            .cloned()
            .collect()
    }

    pub fn get(&self, guid: &str) -> Option<DownloadEntry> {
        self.entries.get(guid).cloned()
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

    fn record_event(&mut self, kind: DownloadEventKind, download: DownloadEntry) {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;
        self.timeline.push_back(DownloadEvent {
            sequence,
            kind,
            download,
        });
        while self.timeline.len() > DOWNLOAD_EVENT_LIMIT {
            self.timeline.pop_front();
        }
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

fn event_kind_for_state(state: DownloadState) -> DownloadEventKind {
    match state {
        DownloadState::Started => DownloadEventKind::Started,
        DownloadState::InProgress => DownloadEventKind::Progress,
        DownloadState::Completed => DownloadEventKind::Completed,
        DownloadState::Failed => DownloadEventKind::Failed,
        DownloadState::Canceled => DownloadEventKind::Canceled,
    }
}

fn is_terminal(state: DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Completed | DownloadState::Failed | DownloadState::Canceled
    )
}

fn rfc3339_now() -> String {
    // Rfc3339 formatting of OffsetDateTime::now_utc() is infallible in
    // practice. Sentinel is non-epoch to make format failures visible
    // rather than silently injecting a valid-looking "1970" timestamp.
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "TIMESTAMP_FORMAT_ERROR".to_string())
}

#[cfg(test)]
mod tests {
    use super::{DownloadProgressEvent, DownloadRuntimeState};
    use rub_core::model::{DownloadMode, DownloadRuntimeStatus, DownloadState};

    #[test]
    fn download_runtime_state_tracks_started_and_completed_downloads() {
        let mut state = DownloadRuntimeState::default();
        state.set_runtime(
            0,
            DownloadRuntimeStatus::Active,
            DownloadMode::Managed,
            Some("/tmp/rub-downloads".to_string()),
        );
        let started = state.record_started(
            0,
            1,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            Some("main-frame".to_string()),
        );
        assert_eq!(started.state, DownloadState::Started);

        let completed = state
            .record_progress(DownloadProgressEvent {
                generation: 0,
                sequence: 2,
                guid: "guid-1".to_string(),
                state: DownloadState::Completed,
                received_bytes: 128,
                total_bytes: Some(128),
                final_path: Some("/tmp/rub-downloads/guid-1".to_string()),
            })
            .expect("download should exist");
        assert_eq!(completed.state, DownloadState::Completed);

        let projection = state.projection();
        assert!(projection.active_downloads.is_empty());
        assert_eq!(projection.completed_downloads.len(), 1);
        assert_eq!(
            projection
                .last_download
                .as_ref()
                .map(|entry| entry.guid.as_str()),
            Some("guid-1")
        );
        assert_eq!(state.events_after(0).len(), 2);
    }

    #[test]
    fn mark_degraded_preserves_existing_entries() {
        let mut state = DownloadRuntimeState::default();
        state.record_started(
            0,
            1,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            None,
        );
        state.mark_degraded(0, "download_behavior_failed");

        let projection = state.projection();
        assert_eq!(projection.status, DownloadRuntimeStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("download_behavior_failed")
        );
        assert_eq!(projection.active_downloads.len(), 1);
    }

    #[test]
    fn terminal_zero_byte_updates_preserve_observed_progress() {
        let mut state = DownloadRuntimeState::default();
        state.record_started(
            0,
            1,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            None,
        );
        state.record_progress(DownloadProgressEvent {
            generation: 0,
            sequence: 2,
            guid: "guid-1".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 4096,
            total_bytes: Some(8192),
            final_path: None,
        });

        let canceled = state
            .record_progress(DownloadProgressEvent {
                generation: 0,
                sequence: 3,
                guid: "guid-1".to_string(),
                state: DownloadState::Canceled,
                received_bytes: 0,
                total_bytes: Some(8192),
                final_path: None,
            })
            .expect("download should exist");

        assert_eq!(canceled.state, DownloadState::Canceled);
        assert_eq!(canceled.received_bytes, 4096);
        assert_eq!(canceled.total_bytes, Some(8192));
    }

    #[test]
    fn stale_started_event_does_not_revive_terminal_download() {
        let mut state = DownloadRuntimeState::default();
        state.record_started(
            0,
            10,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            None,
        );
        state.record_progress(DownloadProgressEvent {
            generation: 0,
            sequence: 11,
            guid: "guid-1".to_string(),
            state: DownloadState::Completed,
            received_bytes: 128,
            total_bytes: Some(128),
            final_path: Some("/tmp/rub-downloads/guid-1".to_string()),
        });

        let projection = state.record_started(
            0,
            9,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            None,
        );

        assert_eq!(projection.state, DownloadState::Completed);
        assert_eq!(
            state.get("guid-1").as_ref().map(|entry| entry.state),
            Some(DownloadState::Completed)
        );
    }

    #[test]
    fn older_generation_progress_is_ignored_after_runtime_switch() {
        let mut state = DownloadRuntimeState::default();
        state.set_runtime(
            1,
            DownloadRuntimeStatus::Active,
            DownloadMode::Managed,
            Some("/tmp/rub-downloads-a".to_string()),
        );
        state.record_started(
            1,
            1,
            "guid-a".to_string(),
            "https://example.test/a".to_string(),
            "a.txt".to_string(),
            None,
        );

        state.set_runtime(
            2,
            DownloadRuntimeStatus::Active,
            DownloadMode::Managed,
            Some("/tmp/rub-downloads-b".to_string()),
        );
        let stale = state.record_progress(DownloadProgressEvent {
            generation: 1,
            sequence: 2,
            guid: "guid-a".to_string(),
            state: DownloadState::Completed,
            received_bytes: 128,
            total_bytes: Some(128),
            final_path: Some("/tmp/rub-downloads-a/guid-a".to_string()),
        });

        assert!(stale.is_none());
        let projection = state.projection();
        assert_eq!(projection.active_downloads.len(), 0);
        assert_eq!(projection.completed_downloads.len(), 0);
        assert_eq!(
            projection.download_dir.as_deref(),
            Some("/tmp/rub-downloads-b")
        );
    }
}
