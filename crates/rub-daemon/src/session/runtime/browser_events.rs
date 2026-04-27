use super::*;

mod apply;
mod ingress;
mod worker;

#[cfg(test)]
mod tests {
    use super::ingress::should_emit_progress_overflow_marker;
    use super::worker::drain_ready_browser_events;
    use super::{BrowserSessionEvent, BrowserSessionEventSink};
    use crate::session::SessionState;
    use crate::session::protocol::{
        BROWSER_EVENT_CRITICAL_SOFT_LIMIT, DOWNLOAD_PROGRESS_OVERFLOW_REASON,
    };
    use rub_core::model::{
        DialogKind, DialogRuntimeInfo, DialogRuntimeStatus, DownloadMode, DownloadRuntimeStatus,
        DownloadState, PendingDialogInfo,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use tokio::time::Duration;

    #[tokio::test]
    async fn download_runtime_clear_event_resets_progress_overflow_latch() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-overflow-reset"),
            None,
        ));
        state
            .mark_download_runtime_degraded(7, "browser_event_ingress_overflow:download_progress")
            .await;

        let browser_sequence = state.allocate_browser_event_sequence();
        let mut pending = BTreeMap::new();
        pending.insert(
            browser_sequence,
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence,
                generation: 7,
                runtime: Box::new(rub_core::model::DownloadRuntimeInfo {
                    status: DownloadRuntimeStatus::Active,
                    mode: DownloadMode::Managed,
                    ..rub_core::model::DownloadRuntimeInfo::default()
                }),
            },
        );
        let progress_overflow_latched = Arc::new(AtomicBool::new(true));
        let progress_overflow_latched_generation = Arc::new(AtomicU64::new(7));
        let progress_overflow_latched_sequence = Arc::new(AtomicU64::new(browser_sequence - 1));
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let mut next_sequence = 1;

        drain_ready_browser_events(
            &state,
            &mut pending,
            &mut next_sequence,
            &progress_overflow_coordination,
            &progress_overflow_latched,
            &progress_overflow_latched_generation,
            &progress_overflow_latched_sequence,
        )
        .await;

        assert!(!progress_overflow_latched.load(Ordering::SeqCst));
        assert_eq!(
            progress_overflow_latched_generation.load(Ordering::SeqCst),
            0
        );
        assert_eq!(progress_overflow_latched_sequence.load(Ordering::SeqCst), 0);
        assert_eq!(state.committed_browser_event_cursor(), browser_sequence);
        assert!(state.download_runtime().await.degraded_reason.is_none());
    }

    #[test]
    fn progress_overflow_marker_reopens_for_new_generation_only() {
        assert!(should_emit_progress_overflow_marker(false, 0, 0, 7, 0, 0));
        assert!(!should_emit_progress_overflow_marker(true, 7, 10, 7, 0, 0));
        assert!(should_emit_progress_overflow_marker(true, 7, 10, 8, 0, 0));
        assert!(should_emit_progress_overflow_marker(true, 7, 10, 7, 7, 11));
        assert!(!should_emit_progress_overflow_marker(true, 7, 12, 7, 7, 11));
    }

    #[tokio::test]
    async fn stale_overflow_degraded_write_cannot_override_newer_runtime_clear() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-overflow-order"),
            None,
        ));

        state
            .mark_download_runtime_degraded_browser_event(
                7,
                10,
                "browser_event_ingress_overflow:download_progress",
            )
            .await;
        let degraded = state.download_runtime().await;
        assert_eq!(degraded.status, DownloadRuntimeStatus::Degraded);

        let applied = state
            .apply_download_runtime_event_sequenced(
                7,
                11,
                rub_core::model::DownloadRuntimeInfo {
                    status: DownloadRuntimeStatus::Active,
                    mode: DownloadMode::Managed,
                    ..rub_core::model::DownloadRuntimeInfo::default()
                },
            )
            .await;
        assert!(applied.applied);
        state
            .mark_download_runtime_degraded_browser_event(
                7,
                10,
                "browser_event_ingress_overflow:download_progress",
            )
            .await;

        let runtime = state.download_runtime().await;
        assert_eq!(runtime.status, DownloadRuntimeStatus::Active);
        assert!(runtime.degraded_reason.is_none());
    }

    #[tokio::test]
    async fn download_runtime_event_can_restore_active_download_projection_truth() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-download-runtime-replay"),
            None,
        ));
        let browser_sequence = state.allocate_browser_event_sequence();
        let mut pending = BTreeMap::new();
        pending.insert(
            browser_sequence,
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence,
                generation: 11,
                runtime: Box::new(rub_core::model::DownloadRuntimeInfo {
                    status: DownloadRuntimeStatus::Active,
                    mode: DownloadMode::Managed,
                    download_dir: Some("/tmp/rub-downloads".to_string()),
                    active_downloads: vec![rub_core::model::DownloadEntry {
                        guid: "guid-runtime-replay".to_string(),
                        state: DownloadState::InProgress,
                        url: Some("https://example.test/report.csv".to_string()),
                        suggested_filename: Some("report.csv".to_string()),
                        final_path: None,
                        mime_hint: None,
                        received_bytes: 64,
                        total_bytes: Some(128),
                        started_at: "2026-04-22T00:00:00Z".to_string(),
                        completed_at: None,
                        frame_id: Some("frame-main".to_string()),
                        trigger_command_id: None,
                    }],
                    completed_downloads: Vec::new(),
                    last_download: Some(rub_core::model::DownloadEntry {
                        guid: "guid-runtime-replay".to_string(),
                        state: DownloadState::InProgress,
                        url: Some("https://example.test/report.csv".to_string()),
                        suggested_filename: Some("report.csv".to_string()),
                        final_path: None,
                        mime_hint: None,
                        received_bytes: 64,
                        total_bytes: Some(128),
                        started_at: "2026-04-22T00:00:00Z".to_string(),
                        completed_at: None,
                        frame_id: Some("frame-main".to_string()),
                        trigger_command_id: None,
                    }),
                    degraded_reason: None,
                }),
            },
        );
        let progress_overflow_latched = Arc::new(AtomicBool::new(false));
        let progress_overflow_latched_generation = Arc::new(AtomicU64::new(0));
        let progress_overflow_latched_sequence = Arc::new(AtomicU64::new(0));
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let mut next_sequence = 1;

        drain_ready_browser_events(
            &state,
            &mut pending,
            &mut next_sequence,
            &progress_overflow_coordination,
            &progress_overflow_latched,
            &progress_overflow_latched_generation,
            &progress_overflow_latched_sequence,
        )
        .await;

        let runtime = state.download_runtime().await;
        assert_eq!(runtime.status, DownloadRuntimeStatus::Active);
        assert_eq!(runtime.active_downloads.len(), 1);
        assert_eq!(runtime.active_downloads[0].guid, "guid-runtime-replay");
        assert_eq!(runtime.active_downloads[0].received_bytes, 64);
        assert_eq!(
            state
                .download_entry("guid-runtime-replay")
                .await
                .as_ref()
                .map(|download| download.state),
            Some(DownloadState::InProgress)
        );
    }

    #[tokio::test]
    async fn overflow_marker_commits_runtime_state_inside_browser_event_fence() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-overflow-fence"),
            None,
        ));
        let browser_sequence = state.allocate_browser_event_sequence();
        let mut pending = BTreeMap::new();
        pending.insert(
            browser_sequence,
            BrowserSessionEvent::DownloadRuntimeDegradedMarker {
                browser_sequence,
                generation: 9,
                reason: DOWNLOAD_PROGRESS_OVERFLOW_REASON.to_string(),
            },
        );
        let progress_overflow_latched = Arc::new(AtomicBool::new(true));
        let progress_overflow_latched_generation = Arc::new(AtomicU64::new(9));
        let progress_overflow_latched_sequence = Arc::new(AtomicU64::new(browser_sequence));
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let mut next_sequence = 1;

        drain_ready_browser_events(
            &state,
            &mut pending,
            &mut next_sequence,
            &progress_overflow_coordination,
            &progress_overflow_latched,
            &progress_overflow_latched_generation,
            &progress_overflow_latched_sequence,
        )
        .await;

        assert_eq!(state.committed_browser_event_cursor(), browser_sequence);
        assert_eq!(
            state.download_runtime().await.degraded_reason.as_deref(),
            Some(DOWNLOAD_PROGRESS_OVERFLOW_REASON)
        );
    }

    #[tokio::test]
    async fn new_generation_overflow_is_not_masked_by_previous_generation_latch() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-overflow-generation"),
            None,
        ));
        let sink = BrowserSessionEventSink::saturated_progress_for_test(&state);
        sink.progress_overflow_latched.store(true, Ordering::SeqCst);
        sink.progress_overflow_latched_generation
            .store(7, Ordering::SeqCst);
        sink.progress_overflow_latched_sequence
            .store(1, Ordering::SeqCst);

        let browser_sequence = state.allocate_browser_event_sequence();
        sink.enqueue(BrowserSessionEvent::DownloadProgress {
            browser_sequence,
            generation: 8,
            guid: "guid-next-generation".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 1,
            total_bytes: Some(10),
            final_path: None,
        });

        state
            .wait_for_browser_event_quiescence_since(
                browser_sequence.saturating_sub(1),
                Duration::from_millis(100),
                Duration::from_millis(5),
            )
            .await;

        assert_eq!(
            state.download_runtime().await.degraded_reason.as_deref(),
            Some(DOWNLOAD_PROGRESS_OVERFLOW_REASON)
        );
        assert_eq!(
            sink.progress_overflow_latched_generation
                .load(Ordering::SeqCst),
            8
        );
    }

    #[tokio::test]
    async fn same_generation_overflow_reopens_after_later_clear_is_enqueued() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-overflow-same-generation"),
            None,
        ));
        let sink = BrowserSessionEventSink::saturated_progress_for_test(&state);

        let first_overflow_sequence = state.allocate_browser_event_sequence();
        sink.enqueue(BrowserSessionEvent::DownloadProgress {
            browser_sequence: first_overflow_sequence,
            generation: 7,
            guid: "guid-first-overflow".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 1,
            total_bytes: Some(10),
            final_path: None,
        });

        state
            .wait_for_browser_event_quiescence_since(
                first_overflow_sequence.saturating_sub(1),
                Duration::from_millis(100),
                Duration::from_millis(5),
            )
            .await;

        let clear_sequence = state.allocate_browser_event_sequence();
        sink.enqueue(BrowserSessionEvent::DownloadRuntime {
            browser_sequence: clear_sequence,
            generation: 7,
            runtime: Box::new(rub_core::model::DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                ..rub_core::model::DownloadRuntimeInfo::default()
            }),
        });

        let second_overflow_sequence = state.allocate_browser_event_sequence();
        sink.enqueue(BrowserSessionEvent::DownloadProgress {
            browser_sequence: second_overflow_sequence,
            generation: 7,
            guid: "guid-second-overflow".to_string(),
            state: DownloadState::InProgress,
            received_bytes: 2,
            total_bytes: Some(10),
            final_path: None,
        });

        state
            .wait_for_browser_event_quiescence_since(
                first_overflow_sequence.saturating_sub(1),
                Duration::from_millis(100),
                Duration::from_millis(5),
            )
            .await;

        assert_eq!(
            sink.progress_overflow_latched_sequence
                .load(Ordering::SeqCst),
            second_overflow_sequence
        );
        assert_eq!(
            state.download_runtime().await.degraded_reason.as_deref(),
            Some(DOWNLOAD_PROGRESS_OVERFLOW_REASON)
        );
    }

    #[tokio::test]
    async fn older_clear_does_not_reset_newer_same_generation_overflow_latch() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-overflow-older-clear"),
            None,
        ));
        let clear_sequence = state.allocate_browser_event_sequence();
        let mut pending = BTreeMap::new();
        pending.insert(
            clear_sequence,
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence: clear_sequence,
                generation: 7,
                runtime: Box::new(rub_core::model::DownloadRuntimeInfo {
                    status: DownloadRuntimeStatus::Active,
                    mode: DownloadMode::Managed,
                    ..rub_core::model::DownloadRuntimeInfo::default()
                }),
            },
        );
        let progress_overflow_latched = Arc::new(AtomicBool::new(true));
        let progress_overflow_latched_generation = Arc::new(AtomicU64::new(7));
        let progress_overflow_latched_sequence =
            Arc::new(AtomicU64::new(clear_sequence.saturating_add(1)));
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let mut next_sequence = 1;

        drain_ready_browser_events(
            &state,
            &mut pending,
            &mut next_sequence,
            &progress_overflow_coordination,
            &progress_overflow_latched,
            &progress_overflow_latched_generation,
            &progress_overflow_latched_sequence,
        )
        .await;

        assert!(progress_overflow_latched.load(Ordering::SeqCst));
        assert_eq!(
            progress_overflow_latched_generation.load(Ordering::SeqCst),
            7
        );
        assert_eq!(
            progress_overflow_latched_sequence.load(Ordering::SeqCst),
            clear_sequence.saturating_add(1)
        );
    }

    #[tokio::test]
    async fn drain_resyncs_past_committed_sequence_holes() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-sequence-hole"),
            None,
        ));
        let first_sequence = state.allocate_browser_event_sequence();
        let second_sequence = state.allocate_browser_event_sequence();
        state.record_browser_event_commit(first_sequence);

        let mut pending = BTreeMap::new();
        pending.insert(
            second_sequence,
            BrowserSessionEvent::DownloadRuntimeDegradedMarker {
                browser_sequence: second_sequence,
                generation: 3,
                reason: DOWNLOAD_PROGRESS_OVERFLOW_REASON.to_string(),
            },
        );
        let progress_overflow_latched = Arc::new(AtomicBool::new(false));
        let progress_overflow_latched_generation = Arc::new(AtomicU64::new(0));
        let progress_overflow_latched_sequence = Arc::new(AtomicU64::new(0));
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let mut next_sequence = first_sequence;

        drain_ready_browser_events(
            &state,
            &mut pending,
            &mut next_sequence,
            &progress_overflow_coordination,
            &progress_overflow_latched,
            &progress_overflow_latched_generation,
            &progress_overflow_latched_sequence,
        )
        .await;

        assert_eq!(state.committed_browser_event_cursor(), second_sequence);
        assert_eq!(
            state.download_runtime().await.degraded_reason.as_deref(),
            Some(DOWNLOAD_PROGRESS_OVERFLOW_REASON)
        );
    }

    #[tokio::test]
    async fn critical_ingress_pressure_resets_after_queue_drains_below_soft_limit() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-critical-meter"),
            None,
        ));

        for _ in 0..=BROWSER_EVENT_CRITICAL_SOFT_LIMIT {
            state.record_critical_browser_event_enqueued();
        }
        assert!(
            state
                .browser_event_ingress_telemetry
                .critical_pressure_active
                .load(Ordering::SeqCst)
        );

        state.record_critical_browser_event_dequeued();

        assert!(
            !state
                .browser_event_ingress_telemetry
                .critical_pressure_active
                .load(Ordering::SeqCst)
        );
        assert_eq!(
            state
                .browser_event_ingress_telemetry
                .critical_pending_count
                .load(Ordering::SeqCst),
            BROWSER_EVENT_CRITICAL_SOFT_LIMIT
        );
    }

    #[tokio::test]
    async fn metered_critical_test_sink_tracks_pending_depth_without_worker() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-critical-pending"),
            None,
        ));
        let sink = BrowserSessionEventSink::metered_critical_for_test(&state);

        sink.enqueue(BrowserSessionEvent::DialogRuntime {
            browser_sequence: state.allocate_browser_event_sequence(),
            generation: 1,
            runtime: Box::new(DialogRuntimeInfo {
                status: DialogRuntimeStatus::Active,
                ..DialogRuntimeInfo::default()
            }),
        });

        let metrics = state.browser_event_ingress_metrics().await;
        assert_eq!(metrics["critical"]["pending_count"], serde_json::json!(1));
        assert_eq!(
            metrics["critical"]["max_pending_count"],
            serde_json::json!(1)
        );
    }

    #[tokio::test]
    async fn dialog_events_preserve_browser_sequence_order_across_arrival_reordering() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-dialog-order"),
            None,
        ));
        let sink = BrowserSessionEventSink::new(&state);
        let opened_sequence = state.allocate_browser_event_sequence();
        let closed_sequence = state.allocate_browser_event_sequence();

        sink.enqueue(BrowserSessionEvent::DialogClosed {
            browser_sequence: closed_sequence,
            generation: 3,
            accepted: true,
            user_input: String::new(),
        });
        sink.enqueue(BrowserSessionEvent::DialogOpened {
            browser_sequence: opened_sequence,
            generation: 3,
            kind: DialogKind::Alert,
            message: "Hello".to_string(),
            url: "https://example.test/dialog".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            default_prompt: None,
            has_browser_handler: true,
        });

        state
            .wait_for_browser_event_quiescence_since(
                opened_sequence.saturating_sub(1),
                Duration::from_millis(100),
                Duration::from_millis(5),
            )
            .await;

        let runtime = state.dialog_runtime().await;
        assert_eq!(runtime.status, DialogRuntimeStatus::Inactive);
        assert!(runtime.pending_dialog.is_none());
        assert_eq!(
            runtime
                .last_dialog
                .as_ref()
                .map(|dialog| dialog.message.as_str()),
            Some("Hello")
        );
        assert_eq!(
            runtime.last_result.as_ref().map(|result| result.accepted),
            Some(true)
        );
    }

    #[tokio::test]
    async fn dialog_runtime_event_replaces_projection_with_failed_rebuild_fallback_truth() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-browser-event-dialog-runtime"),
            None,
        ));
        let sink = BrowserSessionEventSink::new(&state);
        let sequence = state.allocate_browser_event_sequence();

        sink.enqueue(BrowserSessionEvent::DialogRuntime {
            browser_sequence: sequence,
            generation: 4,
            runtime: Box::new(DialogRuntimeInfo {
                status: DialogRuntimeStatus::Degraded,
                degraded_reason: Some("browser_authority_rebuild_failed".to_string()),
                pending_dialog: Some(PendingDialogInfo {
                    kind: DialogKind::Alert,
                    message: "Pending after failed rebuild".to_string(),
                    default_prompt: None,
                    url: "https://example.test/dialog".to_string(),
                    has_browser_handler: false,
                    opened_at: "2026-04-15T00:00:00Z".to_string(),
                    frame_id: Some("frame-1".to_string()),
                    tab_target_id: Some("tab-1".to_string()),
                }),
                last_dialog: Some(PendingDialogInfo {
                    kind: DialogKind::Alert,
                    message: "Pending after failed rebuild".to_string(),
                    default_prompt: None,
                    url: "https://example.test/dialog".to_string(),
                    has_browser_handler: false,
                    opened_at: "2026-04-15T00:00:00Z".to_string(),
                    frame_id: Some("frame-1".to_string()),
                    tab_target_id: Some("tab-1".to_string()),
                }),
                ..DialogRuntimeInfo::default()
            }),
        });

        state
            .wait_for_browser_event_quiescence_since(
                sequence.saturating_sub(1),
                Duration::from_millis(100),
                Duration::from_millis(5),
            )
            .await;

        let runtime = state.dialog_runtime().await;
        assert_eq!(runtime.status, DialogRuntimeStatus::Degraded);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("browser_authority_rebuild_failed")
        );
        assert_eq!(
            runtime
                .pending_dialog
                .as_ref()
                .map(|dialog| dialog.message.as_str()),
            Some("Pending after failed rebuild")
        );
        assert_eq!(
            runtime
                .last_dialog
                .as_ref()
                .map(|dialog| dialog.message.as_str()),
            Some("Pending after failed rebuild")
        );
    }
}
