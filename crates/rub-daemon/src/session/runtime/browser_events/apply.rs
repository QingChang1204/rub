use super::*;
use crate::dialogs;
use crate::dialogs::DialogOpenedEvent;
use crate::session::protocol::{
    BROWSER_EVENT_CRITICAL_SOFT_LIMIT, DOWNLOAD_PROGRESS_OVERFLOW_REASON,
};
use rub_core::model::PendingDialogInfo;
use std::sync::atomic::Ordering;
use tokio::time::{Duration, Instant, sleep};

pub(super) struct BrowserEventApplyOutcome {
    pub(super) browser_sequence: u64,
    pub(super) clear_progress_overflow_latch: Option<(u64, u64)>,
}

impl SessionState {
    pub(super) fn record_critical_browser_event_enqueued(&self) {
        let pending = self
            .browser_event_ingress_telemetry
            .critical_pending_count
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        atomic_max_u32(
            &self
                .browser_event_ingress_telemetry
                .critical_max_pending_count,
            pending,
        );
        if pending > BROWSER_EVENT_CRITICAL_SOFT_LIMIT
            && !self
                .browser_event_ingress_telemetry
                .critical_pressure_active
                .swap(true, Ordering::SeqCst)
        {
            self.browser_event_ingress_telemetry
                .critical_soft_limit_cross_count
                .fetch_add(1, Ordering::SeqCst);
            self.browser_event_ingress_telemetry
                .last_critical_soft_limit_cross_uptime_ms
                .store(self.uptime_millis(), Ordering::SeqCst);
        }
    }

    pub(super) fn record_critical_browser_event_dequeued(&self) {
        let remaining = atomic_saturating_decrement_u32(
            &self.browser_event_ingress_telemetry.critical_pending_count,
        );
        if remaining <= BROWSER_EVENT_CRITICAL_SOFT_LIMIT {
            self.browser_event_ingress_telemetry
                .critical_pressure_active
                .store(false, Ordering::SeqCst);
        }
    }

    pub async fn browser_event_ingress_metrics(&self) -> serde_json::Value {
        let uptime_ms = self.uptime_millis();
        let last_critical_soft_limit_cross_uptime_ms = self
            .browser_event_ingress_telemetry
            .last_critical_soft_limit_cross_uptime_ms
            .load(Ordering::SeqCst);
        serde_json::json!({
            "critical": {
                "mode": "lossless_metered_unbounded",
                "soft_limit": BROWSER_EVENT_CRITICAL_SOFT_LIMIT,
                "pending_count": self
                    .browser_event_ingress_telemetry
                    .critical_pending_count
                    .load(Ordering::SeqCst),
                "max_pending_count": self
                    .browser_event_ingress_telemetry
                    .critical_max_pending_count
                    .load(Ordering::SeqCst),
                "pressure_active": self
                    .browser_event_ingress_telemetry
                    .critical_pressure_active
                    .load(Ordering::SeqCst),
                "soft_limit_cross_count": self
                    .browser_event_ingress_telemetry
                    .critical_soft_limit_cross_count
                    .load(Ordering::SeqCst),
                "last_soft_limit_cross_uptime_ms": nonzero_u64(last_critical_soft_limit_cross_uptime_ms),
                "last_soft_limit_cross_age_ms": age_from_uptime(uptime_ms, last_critical_soft_limit_cross_uptime_ms),
            },
            "progress": {
                "mode": "bounded_drop_with_degraded_marker",
                "capacity": BROWSER_EVENT_PROGRESS_INGRESS_LIMIT,
                "drop_count": self.browser_event_ingress_drop_count(),
                "drop_reason": DOWNLOAD_PROGRESS_OVERFLOW_REASON,
            }
        })
    }

    fn next_dialog_event_sequence(&self) -> u64 {
        self.next_dialog_event_sequence
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1)
    }

    fn next_download_event_sequence(&self) -> u64 {
        self.next_download_event_sequence
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1)
    }

    pub(super) async fn apply_browser_session_event(
        &self,
        event: BrowserSessionEvent,
    ) -> BrowserEventApplyOutcome {
        match event {
            BrowserSessionEvent::DialogRuntime {
                browser_sequence,
                generation,
                status,
                degraded_reason,
            } => {
                let _ = self.set_dialog_runtime(generation, status).await;
                if let Some(reason) = degraded_reason {
                    self.mark_dialog_runtime_degraded(generation, reason).await;
                }
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: None,
                }
            }
            BrowserSessionEvent::DialogOpened {
                browser_sequence,
                generation,
                kind,
                message,
                url,
                tab_target_id,
                frame_id,
                default_prompt,
                has_browser_handler,
            } => {
                self.record_dialog_opened_sequenced(DialogOpenedEvent {
                    generation,
                    sequence: self.next_dialog_event_sequence(),
                    pending: PendingDialogInfo {
                        kind,
                        message,
                        url,
                        tab_target_id,
                        frame_id,
                        default_prompt,
                        has_browser_handler,
                        opened_at: dialogs::rfc3339_now(),
                    },
                })
                .await;
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: None,
                }
            }
            BrowserSessionEvent::DialogClosed {
                browser_sequence,
                generation,
                accepted,
                user_input,
            } => {
                self.record_dialog_closed_sequenced(
                    generation,
                    self.next_dialog_event_sequence(),
                    accepted,
                    user_input,
                )
                .await;
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: None,
                }
            }
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence,
                generation,
                status,
                mode,
                download_dir,
                degraded_reason,
            } => {
                let clears_progress_overflow_latch = degraded_reason
                    .as_ref()
                    .is_none()
                    .then_some((generation, browser_sequence));
                let applied = self
                    .apply_download_runtime_event_sequenced(
                        generation,
                        browser_sequence,
                        status,
                        mode,
                        download_dir,
                        degraded_reason,
                    )
                    .await;
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: clears_progress_overflow_latch
                        .filter(|_| applied.applied),
                }
            }
            BrowserSessionEvent::DownloadRuntimeDegradedMarker {
                browser_sequence,
                generation,
                reason,
            } => {
                self.mark_download_runtime_degraded_browser_event(
                    generation,
                    browser_sequence,
                    reason,
                )
                .await;
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: None,
                }
            }
            BrowserSessionEvent::DownloadStarted {
                browser_sequence,
                generation,
                guid,
                url,
                suggested_filename,
                frame_id,
            } => {
                self.record_download_started_sequenced(
                    generation,
                    self.next_download_event_sequence(),
                    guid,
                    url,
                    suggested_filename,
                    frame_id,
                )
                .await;
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: None,
                }
            }
            BrowserSessionEvent::DownloadProgress {
                browser_sequence,
                generation,
                guid,
                state,
                received_bytes,
                total_bytes,
                final_path,
            } => {
                self.record_download_progress_event(crate::downloads::DownloadProgressEvent {
                    generation,
                    sequence: self.next_download_event_sequence(),
                    guid,
                    state,
                    received_bytes,
                    total_bytes,
                    final_path,
                })
                .await;
                BrowserEventApplyOutcome {
                    browser_sequence,
                    clear_progress_overflow_latch: None,
                }
            }
        }
    }

    pub async fn wait_for_browser_event_quiescence_since(
        &self,
        baseline_cursor: u64,
        timeout: Duration,
        quiet_period: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            let observed = self.browser_event_cursor();
            if observed > baseline_cursor
                && !self
                    .wait_for_browser_event_commit_until(observed, deadline)
                    .await
            {
                return;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            sleep(quiet_period.min(remaining)).await;
            if self.browser_event_cursor() <= observed {
                return;
            }
        }
    }

    async fn wait_for_browser_event_commit_until(&self, cursor: u64, deadline: Instant) -> bool {
        loop {
            if self.committed_browser_event_cursor() >= cursor {
                return true;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let notifier = self.browser_event_notifier();
            let notified = notifier.notified();
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return self.committed_browser_event_cursor() >= cursor;
            }
        }
    }

    /// Current session-scoped runtime observability projection.
    pub async fn dialog_runtime(&self) -> DialogRuntimeInfo {
        self.dialogs.read().await.projection()
    }

    /// Replace the browser-side dialog runtime projection for this session.
    pub async fn set_dialog_runtime(
        &self,
        generation: u64,
        status: DialogRuntimeStatus,
    ) -> DialogRuntimeInfo {
        self.dialogs.write().await.set_runtime(generation, status)
    }

    /// Replace the session-scoped dialog runtime projection from browser authority.
    pub async fn set_dialog_projection(
        &self,
        generation: u64,
        runtime: DialogRuntimeInfo,
    ) -> DialogRuntimeInfo {
        self.dialogs
            .write()
            .await
            .replace_projection(generation, runtime)
    }

    /// Mark the dialog runtime as degraded when page-level listeners cannot be installed.
    pub async fn mark_dialog_runtime_degraded(&self, generation: u64, reason: impl Into<String>) {
        self.dialogs.write().await.mark_degraded(generation, reason);
    }

    /// Record a pending JavaScript dialog opening on the active page.
    pub async fn record_dialog_opened(
        &self,
        kind: DialogKind,
        message: String,
        url: String,
        frame_id: Option<String>,
        default_prompt: Option<String>,
        has_browser_handler: bool,
    ) {
        self.record_dialog_opened_sequenced(DialogOpenedEvent {
            generation: 0,
            sequence: self
                .next_dialog_event_sequence
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1),
            pending: PendingDialogInfo {
                kind,
                message,
                url,
                tab_target_id: None,
                frame_id,
                default_prompt,
                has_browser_handler,
                opened_at: dialogs::rfc3339_now(),
            },
        })
        .await;
    }

    pub async fn record_dialog_opened_sequenced(&self, event: DialogOpenedEvent) {
        self.dialogs.write().await.record_opened(event);
    }

    /// Record a JavaScript dialog closing event after an explicit accept/dismiss action.
    pub async fn record_dialog_closed(&self, accepted: bool, user_input: String) {
        self.record_dialog_closed_sequenced(
            0,
            self.next_dialog_event_sequence
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1),
            accepted,
            user_input,
        )
        .await;
    }

    pub async fn record_dialog_closed_sequenced(
        &self,
        generation: u64,
        sequence: u64,
        accepted: bool,
        user_input: String,
    ) {
        self.dialogs
            .write()
            .await
            .record_closed(generation, sequence, accepted, user_input);
    }

    /// Current session-scoped browser download projection.
    pub async fn download_runtime(&self) -> DownloadRuntimeInfo {
        self.downloads.read().await.projection()
    }

    /// Current download event cursor for interaction/download correlation windows.
    pub async fn download_cursor(&self) -> u64 {
        self.downloads.read().await.cursor()
    }

    /// Sequenced download runtime events recorded after the given cursor.
    pub async fn download_events_after(&self, cursor: u64) -> Vec<DownloadEvent> {
        self.downloads.read().await.events_after(cursor)
    }

    /// Current download-ingress drop count for interaction window authority checks.
    pub fn download_event_drop_count(&self) -> u64 {
        self.browser_event_ingress_drop_count()
    }

    /// Windowed download evidence after one interaction baseline.
    ///
    /// The current browser-event ingress only drops bounded in-progress download
    /// progress events. That means a delta in `download_event_drop_count()`
    /// invalidates the download window for this interaction even if some sequenced
    /// events were still published successfully.
    pub(crate) async fn download_event_window_after(
        &self,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::session::protocol::DownloadEventWindow {
        let events = self.download_events_after(cursor).await;
        let current_drop_count = self.download_event_drop_count();
        let authoritative = current_drop_count == last_observed_drop_count;
        let degraded_reason = if authoritative {
            None
        } else {
            self.download_runtime().await.degraded_reason.or_else(|| {
                Some(crate::session::protocol::DOWNLOAD_PROGRESS_OVERFLOW_REASON.to_string())
            })
        };
        crate::session::protocol::DownloadEventWindow {
            events,
            authoritative,
            degraded_reason,
        }
    }

    /// Return a projected download entry by GUID when present.
    pub async fn download_entry(&self, guid: &str) -> Option<DownloadEntry> {
        self.downloads.read().await.get(guid)
    }

    /// Replace the browser-side download behavior projection for this session.
    pub async fn set_download_runtime(
        &self,
        generation: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
    ) -> DownloadRuntimeInfo {
        self.downloads
            .write()
            .await
            .set_runtime(generation, status, mode, download_dir)
    }

    pub async fn apply_download_runtime_event_sequenced(
        &self,
        generation: u64,
        browser_sequence: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
        degraded_reason: Option<String>,
    ) -> crate::downloads::DownloadRuntimeMutationOutcome {
        self.downloads.write().await.apply_runtime_event_sequenced(
            generation,
            browser_sequence,
            status,
            mode,
            download_dir,
            degraded_reason,
        )
    }

    /// Mark the download runtime as degraded when browser-side behavior cannot be configured.
    pub async fn mark_download_runtime_degraded(&self, generation: u64, reason: impl Into<String>) {
        self.downloads
            .write()
            .await
            .mark_degraded(generation, reason);
    }

    pub async fn mark_download_runtime_degraded_browser_event(
        &self,
        generation: u64,
        browser_sequence: u64,
        reason: impl Into<String>,
    ) {
        self.downloads.write().await.mark_degraded_browser_event(
            generation,
            browser_sequence,
            reason,
        );
    }

    /// Record a browser download start event.
    pub async fn record_download_started(
        &self,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    ) {
        self.record_download_started_sequenced(
            0,
            self.next_download_event_sequence
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1),
            guid,
            url,
            suggested_filename,
            frame_id,
        )
        .await;
    }

    pub async fn record_download_started_sequenced(
        &self,
        generation: u64,
        sequence: u64,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    ) {
        self.downloads.write().await.record_started(
            generation,
            sequence,
            guid,
            url,
            suggested_filename,
            frame_id,
        );
    }

    /// Record a browser download progress/terminal event.
    pub async fn record_download_progress(
        &self,
        guid: &str,
        state: DownloadState,
        received_bytes: u64,
        total_bytes: Option<u64>,
        final_path: Option<String>,
    ) {
        self.record_download_progress_event(crate::downloads::DownloadProgressEvent {
            generation: 0,
            sequence: self
                .next_download_event_sequence
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1),
            guid: guid.to_string(),
            state,
            received_bytes,
            total_bytes,
            final_path,
        })
        .await;
    }

    pub async fn record_download_progress_event(
        &self,
        event: crate::downloads::DownloadProgressEvent,
    ) {
        let _ = self.downloads.write().await.record_progress(event);
    }
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn age_from_uptime(current_uptime_ms: u64, event_uptime_ms: u64) -> Option<u64> {
    (event_uptime_ms != 0).then_some(current_uptime_ms.saturating_sub(event_uptime_ms))
}

fn atomic_max_u32(target: &std::sync::atomic::AtomicU32, candidate: u32) {
    let mut current = target.load(Ordering::SeqCst);
    while candidate > current {
        match target.compare_exchange(current, candidate, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn atomic_saturating_decrement_u32(target: &std::sync::atomic::AtomicU32) -> u32 {
    let mut current = target.load(Ordering::SeqCst);
    loop {
        if current == 0 {
            return 0;
        }
        match target.compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return current - 1,
            Err(observed) => current = observed,
        }
    }
}
