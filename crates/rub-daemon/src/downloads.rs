use std::collections::{HashMap, VecDeque};

use rub_core::model::{
    DownloadEntry, DownloadEvent, DownloadEventKind, DownloadMode, DownloadRuntimeInfo,
    DownloadRuntimeStatus, DownloadState,
};

mod events;
mod mutation;
mod projection;

const ACTIVE_DOWNLOAD_LIMIT: usize = 32;
const COMPLETED_DOWNLOAD_LIMIT: usize = 64;
const DOWNLOAD_EVENT_LIMIT: usize = 128;

#[derive(Debug, Default)]
pub struct DownloadRuntimeState {
    next_sequence: u64,
    runtime_event_sequence: u64,
    projection: DownloadRuntimeInfo,
    entries: HashMap<String, DownloadEntry>,
    entry_event_sequence: HashMap<String, u64>,
    active_order: VecDeque<String>,
    completed_order: VecDeque<String>,
    timeline: VecDeque<DownloadEvent>,
    dropped_event_count: u64,
    last_evicted_sequence: u64,
    current_generation: u64,
}

#[derive(Debug, Clone)]
pub struct DownloadRuntimeMutationOutcome {
    pub projection: DownloadRuntimeInfo,
    pub applied: bool,
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

impl DownloadRuntimeState {}

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
    fn late_started_event_backfills_metadata_without_regressing_progress_state() {
        let mut state = DownloadRuntimeState::default();
        state.record_progress(DownloadProgressEvent {
            generation: 0,
            sequence: 10,
            guid: "guid-1".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 64,
            total_bytes: Some(128),
            final_path: None,
        });

        let projection = state.record_started(
            0,
            11,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            Some("frame-main".to_string()),
        );

        assert_eq!(projection.state, DownloadState::InProgress);
        assert_eq!(projection.received_bytes, 64);
        assert_eq!(projection.total_bytes, Some(128));
        assert_eq!(
            projection.url.as_deref(),
            Some("https://example.test/report.csv")
        );
        assert_eq!(projection.suggested_filename.as_deref(), Some("report.csv"));
        assert_eq!(projection.frame_id.as_deref(), Some("frame-main"));
        assert_eq!(
            state
                .projection()
                .last_download
                .as_ref()
                .map(|entry| entry.state),
            Some(DownloadState::InProgress)
        );
        let events = state.events_after(0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, rub_core::model::DownloadEventKind::Progress);
        assert_eq!(
            events[0].download.url.as_deref(),
            Some("https://example.test/report.csv")
        );
        assert_eq!(
            events[0].download.suggested_filename.as_deref(),
            Some("report.csv")
        );
        assert_eq!(events[0].download.frame_id.as_deref(), Some("frame-main"));
    }

    #[test]
    fn late_started_event_backfills_metadata_for_terminal_downloads() {
        let mut state = DownloadRuntimeState::default();
        state.record_progress(DownloadProgressEvent {
            generation: 0,
            sequence: 10,
            guid: "guid-completed".to_string(),
            state: DownloadState::Completed,
            received_bytes: 128,
            total_bytes: Some(128),
            final_path: Some("/tmp/rub-downloads/guid-completed".to_string()),
        });

        let completed = state.record_started(
            0,
            11,
            "guid-completed".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            Some("frame-main".to_string()),
        );

        assert_eq!(completed.state, DownloadState::Completed);
        assert_eq!(
            completed.url.as_deref(),
            Some("https://example.test/report.csv")
        );
        assert_eq!(completed.suggested_filename.as_deref(), Some("report.csv"));
        assert_eq!(completed.frame_id.as_deref(), Some("frame-main"));

        state.record_progress(DownloadProgressEvent {
            generation: 0,
            sequence: 20,
            guid: "guid-canceled".to_string(),
            state: DownloadState::Canceled,
            received_bytes: 64,
            total_bytes: Some(128),
            final_path: None,
        });

        let canceled = state.record_started(
            0,
            21,
            "guid-canceled".to_string(),
            "https://example.test/cancel.csv".to_string(),
            "cancel.csv".to_string(),
            Some("frame-cancel".to_string()),
        );

        assert_eq!(canceled.state, DownloadState::Canceled);
        assert_eq!(
            canceled.url.as_deref(),
            Some("https://example.test/cancel.csv")
        );
        assert_eq!(canceled.suggested_filename.as_deref(), Some("cancel.csv"));
        assert_eq!(canceled.frame_id.as_deref(), Some("frame-cancel"));
        let events = state.events_after(0);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].download.url.as_deref(),
            Some("https://example.test/report.csv")
        );
        assert_eq!(
            events[0].download.suggested_filename.as_deref(),
            Some("report.csv")
        );
        assert_eq!(events[0].download.frame_id.as_deref(), Some("frame-main"));
        assert_eq!(
            events[1].download.url.as_deref(),
            Some("https://example.test/cancel.csv")
        );
        assert_eq!(
            events[1].download.suggested_filename.as_deref(),
            Some("cancel.csv")
        );
        assert_eq!(events[1].download.frame_id.as_deref(), Some("frame-cancel"));
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
