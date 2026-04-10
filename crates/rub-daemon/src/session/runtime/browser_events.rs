use super::*;
use crate::dialogs;
use crate::dialogs::DialogOpenedEvent;
use rub_core::model::PendingDialogInfo;
use tokio::time::{Duration, Instant, sleep};

impl BrowserSessionEventSink {
    pub fn new(state: &Arc<SessionState>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BrowserSessionEvent>();
        let worker_state = state.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                worker_state.apply_browser_session_event(event).await;
            }
        });
        Self {
            state: state.clone(),
            tx,
        }
    }

    #[cfg(test)]
    pub fn closed_for_test(state: &Arc<SessionState>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<BrowserSessionEvent>();
        drop(rx);
        Self {
            state: state.clone(),
            tx,
        }
    }

    pub fn enqueue(&self, event: BrowserSessionEvent) {
        let browser_sequence = event.browser_sequence();
        if let Err(dropped) = self.tx.send(event) {
            // The event-processing worker has exited. This only occurs when the
            // session is shutting down or has encountered an irrecoverable error.
            //
            // The removed code here previously spawned a bypass task that called
            // `apply_browser_session_event` directly. That approach was unsound:
            //   1. It bypasses the FIFO ordering the channel provides, risking
            //      a dual-writer violation on shared session state (→ INV-004).
            //   2. It may race with in-flight writes inside the exiting worker.
            //   3. The session cleanup / lifecycle reset owns authoritative state
            //      teardown; injecting events after that fence is incorrect.
            //
            // We still have to advance the same session-owned commit fence for
            // the allocated browser sequence. Otherwise quiescence waiters
            // would be stranded behind a permanent sequence hole.
            self.state.record_browser_event_commit(browser_sequence);
            tracing::warn!(
                event_kind = ?std::mem::discriminant(&dropped.0),
                browser_sequence,
                "BrowserSessionEvent dropped: worker channel closed (session shutting down)"
            );
        }
    }
}

impl SessionState {
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

    async fn apply_browser_session_event(&self, event: BrowserSessionEvent) {
        let browser_sequence = match event {
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
                browser_sequence
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
                browser_sequence
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
                browser_sequence
            }
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence,
                generation,
                status,
                mode,
                download_dir,
                degraded_reason,
            } => {
                let _ = self
                    .set_download_runtime(generation, status, mode, download_dir)
                    .await;
                if let Some(reason) = degraded_reason {
                    self.mark_download_runtime_degraded(generation, reason)
                        .await;
                }
                browser_sequence
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
                browser_sequence
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
                browser_sequence
            }
        };

        self.record_browser_event_commit(browser_sequence);
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
            if observed <= baseline_cursor {
                return;
            }
            if !self
                .wait_for_browser_event_commit_until(observed, deadline)
                .await
            {
                return;
            }
            sleep(quiet_period).await;
            if self.browser_event_cursor() == observed {
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

    /// Mark the download runtime as degraded when browser-side behavior cannot be configured.
    pub async fn mark_download_runtime_degraded(&self, generation: u64, reason: impl Into<String>) {
        self.downloads
            .write()
            .await
            .mark_degraded(generation, reason);
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
