use super::*;
use crate::dialogs;
use crate::dialogs::DialogOpenedEvent;
use rub_core::model::PendingDialogInfo;
use tokio::time::{Duration, Instant, sleep};

struct ObservatoryDropCounts {
    total: u64,
    timeline: u64,
    request: u64,
}

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

    fn observatory_drop_counts(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
    ) -> ObservatoryDropCounts {
        let timeline = self
            .observatory_ingress_drop_count()
            .saturating_add(observatory.dropped_timeline_event_count());
        let request = self
            .network_request_ingress_drop_count()
            .saturating_add(observatory.dropped_request_record_count());
        ObservatoryDropCounts {
            total: timeline.saturating_add(request),
            timeline,
            request,
        }
    }

    fn projected_observatory(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
    ) -> RuntimeObservatoryInfo {
        observatory.projection_with_drop_count(self.observatory_drop_counts(observatory).total)
    }

    fn observatory_event_window_from_state(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::observatory::ObservatoryEventWindow {
        observatory.event_window_after(
            cursor,
            self.observatory_drop_counts(observatory).timeline,
            last_observed_drop_count,
        )
    }

    fn observatory_request_window_from_state(
        &self,
        observatory: &crate::observatory::RuntimeObservatoryState,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::observatory::NetworkRequestWindow {
        observatory.request_window_after(
            cursor,
            self.observatory_drop_counts(observatory).request,
            last_observed_drop_count,
        )
    }

    /// Current session-scoped developer integration runtime projection.
    pub async fn integration_runtime(&self) -> IntegrationRuntimeInfo {
        let mut integration = self.integration_runtime.read().await.clone();
        integration.sync_request_rule_count();
        let observatory_guard = self.observatory.read().await;
        let observatory = self.projected_observatory(&observatory_guard);
        let runtime_state = self.runtime_state.read().await.snapshot();
        let state_inspector = runtime_state.state_inspector.clone();
        let readiness = runtime_state.readiness_state.clone();
        let handoff = self.handoff.read().await.projection();
        let (active_surfaces, degraded_surfaces) = derive_integration_runtime_surfaces(
            &integration,
            &observatory,
            &state_inspector,
            &readiness,
            &handoff,
        );

        integration.status = derive_integration_runtime_status(
            integration.status,
            &integration.request_rules,
            observatory.status,
            state_inspector.status,
            readiness.status,
        );
        integration.active_surfaces = active_surfaces;
        integration.degraded_surfaces = degraded_surfaces;
        integration.observatory_ready =
            !matches!(observatory.status, RuntimeObservatoryStatus::Inactive);
        integration.state_inspector_ready =
            !matches!(state_inspector.status, StateInspectorStatus::Inactive);
        integration.readiness_ready = !matches!(readiness.status, ReadinessStatus::Inactive);
        integration.handoff_ready = !matches!(
            handoff.status,
            rub_core::model::HumanVerificationHandoffStatus::Unavailable
        );
        integration
    }

    /// Replace the current developer integration runtime projection.
    pub async fn set_integration_runtime(&self, mut integration: IntegrationRuntimeInfo) {
        integration.sync_request_rule_count();
        *self.integration_runtime.write().await = integration;
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

    /// Current session-scoped runtime observability projection.
    pub async fn frame_runtime(&self) -> FrameRuntimeInfo {
        self.frame_runtime.read().await.projection()
    }

    /// Current session-scoped storage runtime projection.
    pub async fn storage_runtime(&self) -> StorageRuntimeInfo {
        self.storage_runtime.read().await.projection()
    }

    /// Replace the current storage runtime snapshot from the live browser authority.
    pub async fn set_storage_snapshot(&self, snapshot: StorageSnapshot) -> StorageRuntimeInfo {
        self.storage_runtime
            .write()
            .await
            .replace_snapshot(snapshot)
    }

    /// Record one storage mutation in the session-scoped mutation ledger.
    pub async fn record_storage_mutation(
        &self,
        kind: StorageMutationKind,
        origin: String,
        area: Option<StorageArea>,
        key: Option<String>,
    ) -> StorageRuntimeInfo {
        self.storage_runtime
            .write()
            .await
            .record_mutation(kind, origin, area, key)
    }

    /// Mark the storage runtime surface as degraded when the live probe cannot run reliably.
    pub async fn mark_storage_runtime_degraded(&self, reason: impl Into<String>) {
        self.storage_runtime.write().await.mark_degraded(reason);
    }

    /// Current selected frame authority (`None` = top/primary frame).
    pub async fn selected_frame_id(&self) -> Option<String> {
        self.frame_runtime.read().await.selected_frame_id()
    }

    /// Replace the selected frame authority (`None` = top/primary frame).
    pub async fn select_frame(&self, frame_id: Option<String>) {
        self.frame_runtime.write().await.select_frame(frame_id);
    }

    /// Replace the current frame runtime projection.
    pub async fn set_frame_runtime(&self, runtime: FrameRuntimeInfo) {
        self.frame_runtime.write().await.replace(runtime);
    }

    /// Rebuild the frame runtime projection from the current live inventory.
    pub async fn apply_frame_inventory(&self, inventory: &[FrameInventoryEntry]) {
        self.frame_runtime.write().await.apply_inventory(inventory);
    }

    /// Overlay session-scoped current/primary markers onto the live frame inventory.
    pub async fn project_frame_inventory(
        &self,
        inventory: &[FrameInventoryEntry],
    ) -> Vec<FrameInventoryEntry> {
        self.frame_runtime.read().await.project_inventory(inventory)
    }

    /// Mark the frame runtime surface as degraded when the live frame probe fails.
    pub async fn mark_frame_runtime_degraded(&self, reason: impl Into<String>) {
        self.frame_runtime.write().await.mark_degraded(reason);
    }

    /// Current session-scoped runtime observability projection.
    pub async fn observatory(&self) -> RuntimeObservatoryInfo {
        let observatory = self.observatory.read().await;
        self.projected_observatory(&observatory)
    }

    /// Return the current observatory cursor for later event-window correlation.
    pub async fn observatory_cursor(&self) -> u64 {
        self.observatory.read().await.cursor()
    }

    /// Return the current network-request cursor for later request-window correlation.
    pub async fn network_request_cursor(&self) -> u64 {
        self.observatory.read().await.request_cursor()
    }

    /// Return sequenced observatory events recorded after the given cursor.
    pub async fn observatory_events_after(&self, cursor: u64) -> Vec<RuntimeObservatoryEvent> {
        self.observatory.read().await.events_after(cursor)
    }

    pub(crate) async fn observatory_event_window_after(
        &self,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::observatory::ObservatoryEventWindow {
        let observatory = self.observatory.read().await;
        self.observatory_event_window_from_state(&observatory, cursor, last_observed_drop_count)
    }

    /// Record a browser console error into the observability ring buffer.
    pub async fn record_console_error(&self, event: ConsoleErrorEvent) {
        self.observatory.write().await.push_console_error(event);
    }

    /// Record a page-level error into the observability ring buffer.
    pub async fn record_page_error(&self, event: PageErrorEvent) {
        self.observatory.write().await.push_page_error(event);
    }

    /// Record a failed network request into the observability ring buffer.
    pub async fn record_network_failure(&self, event: NetworkFailureEvent) {
        self.observatory.write().await.push_network_failure(event);
    }

    /// Record a request summary into the observability ring buffer.
    pub async fn record_request_summary(&self, event: RequestSummaryEvent) {
        self.observatory.write().await.push_request(event);
    }

    /// Upsert a detailed request lifecycle record into the network inspection registry.
    pub async fn upsert_network_request_record(&self, record: NetworkRequestRecord) {
        self.observatory.write().await.upsert_request_record(record);
        self.network_request_notify.notify_waiters();
    }

    /// Return a bounded view of recent network request records.
    pub async fn network_request_records(
        &self,
        last: Option<usize>,
        url_match: Option<&str>,
        method: Option<&str>,
        status: Option<u16>,
        lifecycle: Option<NetworkRequestLifecycle>,
    ) -> Vec<NetworkRequestRecord> {
        self.observatory
            .read()
            .await
            .request_records(last, url_match, method, status, lifecycle)
    }

    /// Return a single network request record by request identifier.
    pub async fn network_request_record(&self, request_id: &str) -> Option<NetworkRequestRecord> {
        self.observatory.read().await.request_record(request_id)
    }

    /// Return request lifecycle records first observed after the given cursor.
    pub async fn network_request_records_after(&self, cursor: u64) -> Vec<NetworkRequestRecord> {
        self.observatory.read().await.request_records_after(cursor)
    }

    pub async fn network_request_drop_count(&self) -> u64 {
        let observatory = self.observatory.read().await;
        self.observatory_drop_counts(&observatory).request
    }

    pub(crate) async fn network_request_window_after(
        &self,
        cursor: u64,
        last_observed_drop_count: u64,
    ) -> crate::observatory::NetworkRequestWindow {
        let observatory = self.observatory.read().await;
        self.observatory_request_window_from_state(&observatory, cursor, last_observed_drop_count)
    }

    /// Mark the runtime observability surface as degraded.
    pub async fn mark_observatory_degraded(&self, reason: impl Into<String>) {
        self.observatory.write().await.mark_degraded(reason);
        self.network_request_notify.notify_waiters();
    }

    pub async fn mark_observatory_ready(&self) {
        self.observatory.write().await.mark_ready();
    }

    /// Shared notification channel for new network-request record commits.
    pub fn network_request_notifier(&self) -> Arc<tokio::sync::Notify> {
        self.network_request_notify.clone()
    }

    pub fn allocate_runtime_state_sequence(&self) -> u64 {
        self.next_runtime_state_sequence
            .fetch_add(1, Ordering::SeqCst)
    }

    pub async fn runtime_state_snapshot(&self) -> RuntimeStateSnapshot {
        self.runtime_state.read().await.snapshot()
    }

    /// Current session-scoped auth/storage observability projection.
    pub async fn state_inspector(&self) -> StateInspectorInfo {
        self.runtime_state.read().await.state_inspector()
    }

    /// Current session-scoped readiness heuristics projection.
    pub async fn readiness_state(&self) -> ReadinessInfo {
        self.runtime_state.read().await.readiness()
    }

    /// Replace the current runtime-state projection atomically.
    pub async fn publish_runtime_state_snapshot(
        &self,
        sequence: u64,
        snapshot: RuntimeStateSnapshot,
    ) {
        self.runtime_state.write().await.replace(sequence, snapshot);
    }

    /// Mark both runtime-state surfaces as degraded from a shared live-probe failure.
    pub async fn mark_runtime_state_probe_degraded(&self, sequence: u64, reason: impl AsRef<str>) {
        let reason = format!("live_probe_failed:{}", reason.as_ref());
        self.runtime_state
            .write()
            .await
            .mark_degraded(sequence, reason);
    }

    /// Current session-scoped human verification handoff projection.
    pub async fn human_verification_handoff(&self) -> HumanVerificationHandoffInfo {
        self.handoff.read().await.projection()
    }

    /// Current session-scoped accessibility/takeover runtime projection.
    pub async fn takeover_runtime(&self) -> TakeoverRuntimeInfo {
        self.takeover.read().await.projection()
    }

    /// Recompute the canonical takeover runtime from launch policy + handoff authority.
    pub async fn refresh_takeover_runtime(
        &self,
        launch_policy: &rub_core::model::LaunchPolicyInfo,
    ) -> TakeoverRuntimeInfo {
        let handoff = self.handoff.read().await.projection();
        self.takeover.write().await.refresh(launch_policy, &handoff)
    }

    /// Record the last takeover transition outcome without mutating the
    /// underlying launch-policy or handoff authorities.
    pub async fn record_takeover_transition(
        &self,
        kind: TakeoverTransitionKind,
        result: TakeoverTransitionResult,
        reason: Option<String>,
    ) -> TakeoverRuntimeInfo {
        self.takeover
            .write()
            .await
            .record_transition(kind, result, reason)
    }

    /// Mark the takeover runtime surface as degraded when relaunch/resume
    /// continuity fences fail.
    pub async fn mark_takeover_runtime_degraded(&self, reason: impl Into<String>) {
        self.takeover.write().await.mark_degraded(reason);
    }

    /// Clear any takeover-runtime degradation override so the canonical
    /// launch-policy + handoff authority can project the live status again.
    pub async fn clear_takeover_runtime_degraded(&self) {
        self.takeover.write().await.clear_degraded();
    }

    /// Current session-scoped public-web interference runtime projection.
    pub async fn interference_runtime(&self) -> InterferenceRuntimeInfo {
        self.interference.read().await.projection()
    }

    /// Replace the current public-web interference runtime projection.
    pub async fn set_interference_runtime(&self, runtime: InterferenceRuntimeInfo) {
        self.interference.write().await.replace(runtime);
    }

    /// Set the canonical public-web interference mode for this session.
    pub async fn set_interference_mode(
        &self,
        mode: rub_core::model::InterferenceMode,
    ) -> InterferenceRuntimeInfo {
        self.interference.write().await.set_mode(mode)
    }

    /// Prime the canonical interference baseline from the current active tab
    /// when the session has not yet established a primary context.
    pub async fn prime_interference_baseline(&self, tabs: &[TabInfo]) {
        self.interference
            .write()
            .await
            .prime_baseline_from_tabs(tabs);
    }

    /// Adopt the current active tab as the canonical primary context after an
    /// explicit user-driven navigation fence.
    pub async fn adopt_interference_primary_context(&self, tabs: &[TabInfo]) {
        self.interference
            .write()
            .await
            .adopt_primary_context_from_tabs(tabs);
    }

    /// Mark the interference runtime surface as degraded.
    pub async fn mark_interference_runtime_degraded(&self, reason: impl Into<String>) {
        self.interference.write().await.mark_degraded(reason);
    }

    /// Recompute the canonical public-web interference runtime projection from
    /// the current session-scoped runtime surfaces and live tab context.
    pub async fn classify_interference_runtime(&self, tabs: &[TabInfo]) -> InterferenceRuntimeInfo {
        let observatory_guard = self.observatory.read().await;
        let observatory = self.projected_observatory(&observatory_guard);
        let readiness = self.runtime_state.read().await.readiness();
        let handoff = self.handoff.read().await.projection();
        self.interference
            .write()
            .await
            .classify(tabs, &observatory, &readiness, &handoff)
    }

    /// Snapshot the current recovery context used by the safe recovery coordinator.
    pub(crate) async fn interference_recovery_context(&self) -> InterferenceRecoveryContext {
        self.interference.read().await.recovery_context()
    }

    /// Mark an interference recovery attempt as in progress.
    pub(crate) async fn begin_interference_recovery(
        &self,
        action: InterferenceRecoveryAction,
    ) -> InterferenceRuntimeInfo {
        self.interference.write().await.begin_recovery(action)
    }

    /// Mark an interference recovery attempt as completed.
    pub(crate) async fn finish_interference_recovery(
        &self,
        result: InterferenceRecoveryResult,
    ) -> InterferenceRuntimeInfo {
        self.interference.write().await.finish_recovery(result)
    }

    /// Record a completed recovery outcome even when no browser mutation was attempted.
    pub(crate) async fn record_interference_recovery_outcome(
        &self,
        action: Option<InterferenceRecoveryAction>,
        result: InterferenceRecoveryResult,
    ) -> InterferenceRuntimeInfo {
        self.interference
            .write()
            .await
            .record_recovery_outcome(action, result)
    }

    /// Replace the current human verification handoff projection.
    pub async fn set_human_verification_handoff(&self, handoff: HumanVerificationHandoffInfo) {
        self.handoff.write().await.replace(handoff);
    }

    /// Mark the session as capable of human verification handoff.
    pub async fn set_handoff_available(&self, resume_supported: bool) {
        self.handoff.write().await.set_available(resume_supported);
    }

    /// Activate human verification handoff and pause automation.
    pub async fn activate_handoff(&self) {
        self.handoff.write().await.activate();
    }

    /// Complete the current human verification handoff and resume automation.
    pub async fn complete_handoff(&self) {
        self.handoff.write().await.complete();
    }

    /// Whether automation is currently paused for human verification handoff.
    pub async fn is_handoff_active(&self) -> bool {
        self.handoff.read().await.projection().automation_paused
    }

    /// Whether the session is currently held by explicit human control.
    pub async fn has_active_human_control(&self) -> bool {
        if self.is_handoff_active().await {
            return true;
        }

        let takeover = self.takeover.read().await.projection();
        takeover.automation_paused
            || matches!(
                takeover.status,
                rub_core::model::TakeoverRuntimeStatus::Active
            )
    }

    /// Whether the session is idle enough for upgrade/restart coordination.
    pub async fn is_idle_for_upgrade(&self) -> bool {
        self.is_base_idle_for_upgrade() && !self.has_active_human_control().await
    }

    /// Register a session-scoped network rule in the integration runtime.
    pub async fn register_network_rule(&self, spec: NetworkRuleSpec) -> NetworkRule {
        let id = self.next_network_rule_id.fetch_add(1, Ordering::SeqCst);
        let rule = NetworkRule {
            id,
            status: NetworkRuleStatus::Configured,
            spec,
        };
        let mut integration = self.integration_runtime.write().await;
        integration.request_rules.push(rule.clone());
        integration.sync_request_rule_count();
        integration.status = IntegrationRuntimeStatus::Active;
        rule
    }

    /// List all configured network rules in stable registration order.
    pub async fn network_rules(&self) -> Vec<NetworkRule> {
        self.integration_runtime.read().await.request_rules.clone()
    }

    /// Update the runtime attachment status for a configured network rule.
    pub async fn set_network_rule_status(
        &self,
        id: u32,
        status: NetworkRuleStatus,
    ) -> Option<NetworkRule> {
        let mut integration = self.integration_runtime.write().await;
        let rule = integration
            .request_rules
            .iter_mut()
            .find(|rule| rule.id == id)?;
        rule.status = status;
        Some(rule.clone())
    }

    /// Remove a configured network rule.
    pub async fn remove_network_rule(&self, id: u32) -> Option<NetworkRule> {
        let mut integration = self.integration_runtime.write().await;
        let index = integration
            .request_rules
            .iter()
            .position(|rule| rule.id == id)?;
        let removed = integration.request_rules.remove(index);
        integration.sync_request_rule_count();
        if integration.request_rules.is_empty() {
            integration.status = IntegrationRuntimeStatus::Inactive;
        }
        Some(removed)
    }

    /// Clear all configured network rules.
    pub async fn clear_network_rules(&self) -> Vec<NetworkRule> {
        let mut integration = self.integration_runtime.write().await;
        let removed = std::mem::take(&mut integration.request_rules);
        integration.sync_request_rule_count();
        integration.status = IntegrationRuntimeStatus::Inactive;
        removed
    }

    /// Allocate the next stable session-scoped network rule id.
    pub fn next_network_rule_id(&self) -> u32 {
        self.next_network_rule_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Replace the canonical session-scoped network rule list.
    pub async fn replace_network_rules(&self, rules: Vec<NetworkRule>) {
        let mut integration = self.integration_runtime.write().await;
        integration.request_rules = rules;
        integration.sync_request_rule_count();
        integration.status = if integration.request_rules.is_empty() {
            IntegrationRuntimeStatus::Inactive
        } else {
            IntegrationRuntimeStatus::Active
        };
    }
}
