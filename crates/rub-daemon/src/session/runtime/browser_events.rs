use super::*;
use crate::dialogs;
use crate::dialogs::DialogOpenedEvent;
use rub_core::model::PendingDialogInfo;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use tokio::time::{Duration, Instant, sleep};

const DOWNLOAD_PROGRESS_OVERFLOW_REASON: &str = "browser_event_ingress_overflow:download_progress";

impl BrowserSessionEventSink {
    pub fn new(state: &Arc<SessionState>) -> Self {
        Self::new_with_progress_capacity(state, BROWSER_EVENT_PROGRESS_INGRESS_LIMIT)
    }

    fn new_with_progress_capacity(state: &Arc<SessionState>, progress_capacity: usize) -> Self {
        let (critical_tx, critical_rx) =
            tokio::sync::mpsc::unbounded_channel::<BrowserSessionEvent>();
        let (progress_tx, progress_rx) =
            tokio::sync::mpsc::channel::<BrowserSessionEvent>(progress_capacity);
        let worker_state = state.clone();
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let progress_overflow_latched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress_overflow_latched_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let progress_overflow_latched_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let progress_overflow_reopen_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let progress_overflow_reopen_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_progress_overflow_latched = progress_overflow_latched.clone();
        let worker_progress_overflow_latched_generation =
            progress_overflow_latched_generation.clone();
        let worker_progress_overflow_latched_sequence = progress_overflow_latched_sequence.clone();
        let worker_progress_overflow_coordination = progress_overflow_coordination.clone();
        tokio::spawn(async move {
            run_browser_event_worker(
                worker_state,
                critical_rx,
                progress_rx,
                worker_progress_overflow_coordination,
                worker_progress_overflow_latched,
                worker_progress_overflow_latched_generation,
                worker_progress_overflow_latched_sequence,
            )
            .await;
        });
        Self {
            state: state.clone(),
            critical_tx,
            progress_tx,
            progress_overflow_coordination,
            progress_overflow_latched,
            progress_overflow_latched_generation,
            progress_overflow_latched_sequence,
            progress_overflow_reopen_generation,
            progress_overflow_reopen_sequence,
        }
    }

    #[cfg(test)]
    pub fn closed_for_test(state: &Arc<SessionState>) -> Self {
        let (critical_tx, critical_rx) =
            tokio::sync::mpsc::unbounded_channel::<BrowserSessionEvent>();
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<BrowserSessionEvent>(1);
        drop(critical_rx);
        drop(progress_rx);
        Self {
            state: state.clone(),
            critical_tx,
            progress_tx,
            progress_overflow_coordination: Arc::new(std::sync::Mutex::new(())),
            progress_overflow_latched: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress_overflow_latched_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            progress_overflow_latched_sequence: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            progress_overflow_reopen_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            progress_overflow_reopen_sequence: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    pub fn with_progress_capacity_for_test(
        state: &Arc<SessionState>,
        progress_capacity: usize,
    ) -> Self {
        Self::new_with_progress_capacity(state, progress_capacity)
    }

    #[cfg(test)]
    pub fn saturated_progress_for_test(state: &Arc<SessionState>) -> Self {
        let (critical_tx, critical_rx) =
            tokio::sync::mpsc::unbounded_channel::<BrowserSessionEvent>();
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<BrowserSessionEvent>(1);
        let (worker_progress_tx, worker_progress_rx) =
            tokio::sync::mpsc::channel::<BrowserSessionEvent>(1);
        progress_tx
            .try_send(BrowserSessionEvent::DownloadProgress {
                browser_sequence: 0,
                generation: 0,
                guid: "__saturated__".to_string(),
                state: rub_core::model::DownloadState::InProgress,
                received_bytes: 0,
                total_bytes: None,
                final_path: None,
            })
            .expect("test queue should accept seed event");
        drop(worker_progress_tx);
        let worker_state = state.clone();
        let progress_overflow_coordination = Arc::new(std::sync::Mutex::new(()));
        let progress_overflow_latched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress_overflow_latched_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let progress_overflow_latched_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let progress_overflow_reopen_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let progress_overflow_reopen_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_progress_overflow_latched = progress_overflow_latched.clone();
        let worker_progress_overflow_latched_generation =
            progress_overflow_latched_generation.clone();
        let worker_progress_overflow_latched_sequence = progress_overflow_latched_sequence.clone();
        let worker_progress_overflow_coordination = progress_overflow_coordination.clone();
        tokio::spawn(async move {
            run_browser_event_worker(
                worker_state,
                critical_rx,
                worker_progress_rx,
                worker_progress_overflow_coordination,
                worker_progress_overflow_latched,
                worker_progress_overflow_latched_generation,
                worker_progress_overflow_latched_sequence,
            )
            .await;
        });
        std::mem::forget(progress_rx);
        Self {
            state: state.clone(),
            critical_tx,
            progress_tx,
            progress_overflow_coordination,
            progress_overflow_latched,
            progress_overflow_latched_generation,
            progress_overflow_latched_sequence,
            progress_overflow_reopen_generation,
            progress_overflow_reopen_sequence,
        }
    }

    pub fn enqueue(&self, event: BrowserSessionEvent) {
        let coordination_guard = requires_download_progress_coordination(&event).then(|| {
            self.progress_overflow_coordination
                .lock()
                .expect("progress overflow coordination lock poisoned")
        });
        self.enqueue_with_optional_coordination(event, coordination_guard.is_some());
    }

    pub fn enqueue_download_runtime(
        &self,
        generation: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
        degraded_reason: Option<String>,
    ) {
        let _coordination_guard = self
            .progress_overflow_coordination
            .lock()
            .expect("progress overflow coordination lock poisoned");
        let browser_sequence = self.state.allocate_browser_event_sequence();
        self.enqueue_with_optional_coordination(
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence,
                generation,
                status,
                mode,
                download_dir,
                degraded_reason,
            },
            true,
        );
    }

    pub fn enqueue_download_started(
        &self,
        generation: u64,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    ) {
        let _coordination_guard = self
            .progress_overflow_coordination
            .lock()
            .expect("progress overflow coordination lock poisoned");
        let browser_sequence = self.state.allocate_browser_event_sequence();
        self.enqueue_with_optional_coordination(
            BrowserSessionEvent::DownloadStarted {
                browser_sequence,
                generation,
                guid,
                url,
                suggested_filename,
                frame_id,
            },
            true,
        );
    }

    pub fn enqueue_download_progress(
        &self,
        generation: u64,
        guid: String,
        state: DownloadState,
        received_bytes: u64,
        total_bytes: Option<u64>,
        final_path: Option<String>,
    ) {
        let _coordination_guard = self
            .progress_overflow_coordination
            .lock()
            .expect("progress overflow coordination lock poisoned");
        let browser_sequence = self.state.allocate_browser_event_sequence();
        self.enqueue_with_optional_coordination(
            BrowserSessionEvent::DownloadProgress {
                browser_sequence,
                generation,
                guid,
                state,
                received_bytes,
                total_bytes,
                final_path,
            },
            true,
        );
    }

    fn enqueue_with_optional_coordination(
        &self,
        event: BrowserSessionEvent,
        coordination_already_held: bool,
    ) {
        // Coordination invariants:
        // 1. In-progress DownloadProgress may be dropped at bounded ingress, but any dropped
        //    browser_sequence must be marked committed immediately so the worker never waits on
        //    an event that will never arrive.
        // 2. Same-generation overflow marker emission and DownloadRuntime(clear) reopen state
        //    share one mutex, so the latch/reopen atomics describe a single total order.
        // 3. The worker later uses committed_browser_event_cursor() to resync across any holes
        //    created by dropped overflow events instead of stalling on a missing sequence.
        let browser_sequence = event.browser_sequence();
        let runtime_clear_reopen = match &event {
            BrowserSessionEvent::DownloadRuntime {
                browser_sequence,
                generation,
                degraded_reason,
                ..
            } if degraded_reason.is_none() => Some((*generation, *browser_sequence)),
            _ => None,
        };
        if event.uses_bounded_progress_ingress() {
            let generation = match &event {
                BrowserSessionEvent::DownloadProgress { generation, .. } => *generation,
                _ => 0,
            };
            match self.progress_tx.try_send(event) {
                Ok(()) => return,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    let _coordination = (!coordination_already_held).then(|| {
                        self.progress_overflow_coordination
                            .lock()
                            .expect("progress overflow coordination lock poisoned")
                    });
                    let dropped_count = self.state.record_browser_event_ingress_overflow();
                    let overflow_latched = self.progress_overflow_latched.load(Ordering::SeqCst);
                    let latched_generation = self
                        .progress_overflow_latched_generation
                        .load(Ordering::SeqCst);
                    let latched_sequence = self
                        .progress_overflow_latched_sequence
                        .load(Ordering::SeqCst);
                    let reopen_generation = self
                        .progress_overflow_reopen_generation
                        .load(Ordering::SeqCst);
                    let reopen_sequence = self
                        .progress_overflow_reopen_sequence
                        .load(Ordering::SeqCst);
                    if should_emit_progress_overflow_marker(
                        overflow_latched,
                        latched_generation,
                        latched_sequence,
                        generation,
                        reopen_generation,
                        reopen_sequence,
                    ) {
                        if self
                            .critical_tx
                            .send(BrowserSessionEvent::DownloadRuntimeDegradedMarker {
                                browser_sequence,
                                generation,
                                reason: DOWNLOAD_PROGRESS_OVERFLOW_REASON.to_string(),
                            })
                            .is_ok()
                        {
                            self.progress_overflow_latched.store(true, Ordering::SeqCst);
                            self.progress_overflow_latched_generation
                                .store(generation, Ordering::SeqCst);
                            self.progress_overflow_latched_sequence
                                .store(browser_sequence, Ordering::SeqCst);
                        } else {
                            self.state.record_browser_event_commit(browser_sequence);
                        }
                    } else {
                        self.state.record_browser_event_commit(browser_sequence);
                    }
                    tracing::warn!(
                        browser_sequence,
                        dropped_count,
                        "BrowserSessionEvent dropped: bounded progress ingress overflow"
                    );
                    return;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
            }
        } else if let Some((generation, browser_sequence)) = runtime_clear_reopen {
            let _coordination = (!coordination_already_held).then(|| {
                self.progress_overflow_coordination
                    .lock()
                    .expect("progress overflow coordination lock poisoned")
            });
            self.progress_overflow_reopen_generation
                .store(generation, Ordering::SeqCst);
            self.progress_overflow_reopen_sequence
                .store(browser_sequence, Ordering::SeqCst);
            if self.critical_tx.send(event).is_ok() {
                return;
            }
        } else if self.critical_tx.send(event).is_ok() {
            return;
        }

        if let Some((generation, browser_sequence)) = runtime_clear_reopen {
            let _coordination = (!coordination_already_held).then(|| {
                self.progress_overflow_coordination
                    .lock()
                    .expect("progress overflow coordination lock poisoned")
            });
            if self
                .progress_overflow_reopen_generation
                .load(Ordering::SeqCst)
                == generation
                && self
                    .progress_overflow_reopen_sequence
                    .load(Ordering::SeqCst)
                    == browser_sequence
            {
                self.progress_overflow_reopen_generation
                    .store(0, Ordering::SeqCst);
                self.progress_overflow_reopen_sequence
                    .store(0, Ordering::SeqCst);
            }
        }

        self.state.record_browser_event_commit(browser_sequence);
        tracing::warn!(
            browser_sequence,
            "BrowserSessionEvent dropped: worker channel closed (session shutting down)"
        );
    }
}

fn requires_download_progress_coordination(event: &BrowserSessionEvent) -> bool {
    event.uses_bounded_progress_ingress()
        || matches!(
            event,
            BrowserSessionEvent::DownloadRuntime {
                degraded_reason, ..
            } if degraded_reason.is_none()
        )
}

async fn run_browser_event_worker(
    state: Arc<SessionState>,
    mut critical_rx: tokio::sync::mpsc::UnboundedReceiver<BrowserSessionEvent>,
    mut progress_rx: tokio::sync::mpsc::Receiver<BrowserSessionEvent>,
    progress_overflow_coordination: Arc<std::sync::Mutex<()>>,
    progress_overflow_latched: Arc<std::sync::atomic::AtomicBool>,
    progress_overflow_latched_generation: Arc<std::sync::atomic::AtomicU64>,
    progress_overflow_latched_sequence: Arc<std::sync::atomic::AtomicU64>,
) {
    let mut pending = BTreeMap::<u64, BrowserSessionEvent>::new();
    let mut next_sequence = state.committed_browser_event_cursor().saturating_add(1);
    let mut critical_open = true;
    let mut progress_open = true;

    loop {
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
        if !critical_open && !progress_open && pending.is_empty() {
            return;
        }

        tokio::select! {
            event = async {
                if critical_open {
                    critical_rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => match event {
                Some(event) => insert_pending_browser_event(&state, &mut pending, event),
                None => critical_open = false,
            },
            event = async {
                if progress_open {
                    progress_rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => match event {
                Some(event) => insert_pending_browser_event(&state, &mut pending, event),
                None => progress_open = false,
            }
        }
    }
}

fn insert_pending_browser_event(
    state: &Arc<SessionState>,
    pending: &mut BTreeMap<u64, BrowserSessionEvent>,
    event: BrowserSessionEvent,
) {
    let browser_sequence = event.browser_sequence();
    if browser_sequence <= state.committed_browser_event_cursor() {
        return;
    }
    pending.insert(browser_sequence, event);
}

async fn drain_ready_browser_events(
    state: &Arc<SessionState>,
    pending: &mut BTreeMap<u64, BrowserSessionEvent>,
    next_sequence: &mut u64,
    progress_overflow_coordination: &Arc<std::sync::Mutex<()>>,
    progress_overflow_latched: &Arc<std::sync::atomic::AtomicBool>,
    progress_overflow_latched_generation: &Arc<std::sync::atomic::AtomicU64>,
    progress_overflow_latched_sequence: &Arc<std::sync::atomic::AtomicU64>,
) {
    loop {
        let committed = state.committed_browser_event_cursor();
        let expected = committed.saturating_add(1);
        // Committed holes are authoritative. If enqueue-time overflow handling already marked a
        // sequence committed, the worker must advance to the next uncommitted sequence instead of
        // waiting forever for an event that was intentionally dropped before it reached pending.
        if expected > *next_sequence {
            *next_sequence = expected;
        }
        let Some(event) = pending.remove(next_sequence) else {
            return;
        };
        let outcome = state.apply_browser_session_event(event).await;
        if let Some((clear_generation, clear_sequence)) = outcome.clear_progress_overflow_latch {
            let _coordination = progress_overflow_coordination
                .lock()
                .expect("progress overflow coordination lock poisoned");
            let latched_generation = progress_overflow_latched_generation.load(Ordering::SeqCst);
            let latched_sequence = progress_overflow_latched_sequence.load(Ordering::SeqCst);
            if progress_overflow_latched.load(Ordering::SeqCst)
                && latched_generation == clear_generation
                && clear_sequence > latched_sequence
            {
                progress_overflow_latched.store(false, Ordering::SeqCst);
                progress_overflow_latched_generation.store(0, Ordering::SeqCst);
                progress_overflow_latched_sequence.store(0, Ordering::SeqCst);
            }
        }
        state.record_browser_event_commit(outcome.browser_sequence);
        *next_sequence = next_sequence.saturating_add(1);
    }
}

struct BrowserEventApplyOutcome {
    browser_sequence: u64,
    clear_progress_overflow_latch: Option<(u64, u64)>,
}

fn should_emit_progress_overflow_marker(
    overflow_latched: bool,
    latched_generation: u64,
    latched_sequence: u64,
    generation: u64,
    reopen_generation: u64,
    reopen_sequence: u64,
) -> bool {
    // Same-generation reopen is only valid after a later DownloadRuntime(clear) crossed the
    // coordinated fence; reopen_sequence therefore reopens the latch only when it outruns the
    // last emitted marker sequence for that generation.
    !overflow_latched
        || latched_generation != generation
        || (reopen_generation == generation && reopen_sequence > latched_sequence)
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

    async fn apply_browser_session_event(
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

#[cfg(test)]
mod tests {
    use super::{
        BrowserSessionEvent, BrowserSessionEventSink, DOWNLOAD_PROGRESS_OVERFLOW_REASON,
        drain_ready_browser_events, should_emit_progress_overflow_marker,
    };
    use crate::session::SessionState;
    use rub_core::model::{DownloadMode, DownloadRuntimeStatus, DownloadState};
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
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                download_dir: None,
                degraded_reason: None,
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
                DownloadRuntimeStatus::Active,
                DownloadMode::Managed,
                None,
                None,
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
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: None,
            degraded_reason: None,
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
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                download_dir: None,
                degraded_reason: None,
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
}
