use super::*;
use crate::session::protocol::DOWNLOAD_PROGRESS_OVERFLOW_REASON;
use std::sync::atomic::Ordering;

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
            super::worker::run_browser_event_worker(
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
            super::worker::run_browser_event_worker(
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

    #[cfg(test)]
    pub fn metered_critical_for_test(state: &Arc<SessionState>) -> Self {
        let (critical_tx, critical_rx) =
            tokio::sync::mpsc::unbounded_channel::<BrowserSessionEvent>();
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<BrowserSessionEvent>(1);
        std::mem::forget(critical_rx);
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
        runtime: rub_core::model::DownloadRuntimeInfo,
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
                runtime: Box::new(runtime),
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
                runtime,
                ..
            } if runtime.degraded_reason.is_none() => Some((*generation, *browser_sequence)),
            _ => None,
        };
        let mut critical_enqueue_recorded = false;
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
            // Critical ingress policy:
            // 1. Dialog/runtime/degraded-marker events remain lossless at the channel boundary;
            //    we meter pending depth instead of dropping them under pressure.
            // 2. The pending-depth telemetry is authoritative only for the unbounded critical
            //    channel backlog. Once the worker receives an event, that pressure is discharged
            //    from this metric even though the event may still wait in the ordered pending map.
            // 3. The only accepted drop on this path is channel closure during shutdown, and that
            //    closure still commits the browser_sequence so quiescence cannot stall.
            self.state.record_critical_browser_event_enqueued();
            critical_enqueue_recorded = true;
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
        } else {
            self.state.record_critical_browser_event_enqueued();
            critical_enqueue_recorded = true;
            if self.critical_tx.send(event).is_ok() {
                return;
            }
        }
        if critical_enqueue_recorded {
            self.state.record_critical_browser_event_dequeued();
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
                runtime, ..
            } if runtime.degraded_reason.is_none()
        )
}

pub(super) fn should_emit_progress_overflow_marker(
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
