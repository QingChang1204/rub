use super::*;
use rub_core::model::Snapshot;
const SNAPSHOT_SETTLE_DELAY_MS: u64 = 100;
const SNAPSHOT_SETTLE_RETRIES: usize = 6;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExternalDomFenceOutcome {
    Settled,
    IncompleteDueToDeadline,
    Unstable,
}

#[derive(Debug, Clone)]
pub(super) enum DeferredSnapshotPublication {
    Cached(Arc<Snapshot>),
    Fresh(Box<Snapshot>),
}

impl DeferredSnapshotPublication {
    pub(super) fn cached(snapshot: Arc<Snapshot>) -> Self {
        Self::Cached(snapshot)
    }

    pub(super) fn fresh(snapshot: Snapshot) -> Self {
        Self::Fresh(Box::new(snapshot))
    }

    pub(super) fn snapshot(&self) -> &Snapshot {
        match self {
            Self::Cached(snapshot) => snapshot.as_ref(),
            Self::Fresh(snapshot) => snapshot,
        }
    }

    pub(super) async fn publish(self, state: &Arc<SessionState>) -> Arc<Snapshot> {
        match self {
            Self::Cached(snapshot) => snapshot,
            Self::Fresh(snapshot) => state.cache_snapshot(*snapshot).await,
        }
    }
}

pub(super) async fn sleep_full_settle_window(
    deadline: Option<TransactionDeadline>,
    delay_ms: u64,
) -> bool {
    let delay = std::time::Duration::from_millis(delay_ms);
    if let Some(deadline) = deadline {
        let Some(remaining) = deadline.remaining_duration() else {
            return false;
        };
        if remaining <= delay {
            return false;
        }
    }
    tokio::time::sleep(delay).await;
    true
}

pub(super) async fn settle_external_dom_fence(
    state: &Arc<SessionState>,
    command_name: &str,
    deadline: Option<TransactionDeadline>,
) -> ExternalDomFenceOutcome {
    let mut pending_scope = state.take_pending_external_dom_change_scope();
    for attempt in 0..SNAPSHOT_SETTLE_RETRIES {
        if !sleep_full_settle_window(deadline, SNAPSHOT_SETTLE_DELAY_MS).await {
            state.merge_pending_external_dom_change_scope(pending_scope);
            tracing::debug!(
                attempt,
                command = command_name,
                "External DOM settle fence stopped because the authoritative deadline cannot cover another full settle window",
            );
            return ExternalDomFenceOutcome::IncompleteDueToDeadline;
        }
        let observed_scope = state.take_pending_external_dom_change_scope();
        if observed_scope.is_empty() {
            return ExternalDomFenceOutcome::Settled;
        }
        pending_scope.merge(observed_scope);
        if attempt + 1 == SNAPSHOT_SETTLE_RETRIES {
            state.merge_pending_external_dom_change_scope(pending_scope);
            tracing::debug!(
                attempt,
                command = command_name,
                "External DOM settle fence exhausted retries while mutations continued to arrive",
            );
            return ExternalDomFenceOutcome::Unstable;
        }

        tracing::debug!(
            attempt,
            command = command_name,
            "Read-only transaction observed pending external DOM change during settle window; extending fence",
        );
    }

    ExternalDomFenceOutcome::Unstable
}

pub(super) async fn build_stable_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    limit: Option<u32>,
    a11y: bool,
    listeners: bool,
) -> Result<rub_core::model::Snapshot, RubError> {
    let explicit_frame_override = super::frame_scope::orchestration_frame_override(args);
    crate::runtime_refresh::refresh_live_frame_runtime(&router.browser, state).await;
    let selected_frame_id = if let Some(frame_id) = explicit_frame_override {
        super::frame_scope::ensure_request_frame_available(router, frame_id).await?;
        Some(frame_id.to_string())
    } else {
        let frame_runtime = state.frame_runtime().await;
        let selected_frame_id = state.selected_frame_id().await;
        if matches!(
            frame_runtime.status,
            rub_core::model::FrameContextStatus::Stale
        ) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Selected frame context is stale",
                serde_json::json!({
                    "frame_runtime": frame_runtime,
                    "selected_frame_id": selected_frame_id,
                }),
            ));
        }
        if matches!(
            frame_runtime.status,
            rub_core::model::FrameContextStatus::Degraded
        ) {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Selected frame context is unavailable",
                serde_json::json!({
                    "frame_runtime": frame_runtime,
                    "selected_frame_id": selected_frame_id,
                }),
            ));
        }
        selected_frame_id
    };

    let mut publication_guard = PendingSnapshotPublicationGuard::new(state);

    for attempt in 0..SNAPSHOT_SETTLE_RETRIES {
        let mut snapshot = if listeners {
            router
                .browser
                .snapshot_with_listeners_for_frame(selected_frame_id.as_deref(), limit, a11y)
                .await?
        } else if a11y {
            router
                .browser
                .snapshot_with_a11y_for_frame(selected_frame_id.as_deref(), limit)
                .await?
        } else {
            router
                .browser
                .snapshot_for_frame(selected_frame_id.as_deref(), limit)
                .await?
        };

        if !sleep_full_settle_window(Some(deadline), SNAPSHOT_SETTLE_DELAY_MS).await {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Snapshot could not stabilize before the authoritative deadline expired",
                serde_json::json!({
                    "reason": "snapshot_deadline_exhausted",
                    "attempts": attempt + 1,
                    "selected_frame_id": selected_frame_id,
                    "timeout_ms": deadline.timeout_ms,
                    "elapsed_ms": deadline.elapsed_ms(),
                }),
            ));
        }

        let observed_scope = state.take_pending_external_dom_change_scope();
        if observed_scope.is_empty() {
            commit_snapshot_publication_epoch_for_pending_scope(
                state,
                publication_guard.pending_scope(),
                &mut snapshot,
            );
            publication_guard.commit();
            return Ok(snapshot);
        }
        publication_guard.merge(observed_scope);

        if attempt + 1 == SNAPSHOT_SETTLE_RETRIES {
            return Err(RubError::domain_with_context(
                ErrorCode::StaleSnapshot,
                "Snapshot could not stabilize before publish fence",
                serde_json::json!({
                    "reason": "snapshot_unstable",
                    "snapshot_epoch": snapshot.dom_epoch,
                    "attempts": SNAPSHOT_SETTLE_RETRIES,
                    "selected_frame_id": selected_frame_id,
                }),
            ));
        }

        tracing::debug!(
            attempt,
            snapshot_epoch = snapshot.dom_epoch,
            "Snapshot publication observed pending external DOM change during settle window; rebuilding before publish",
        );
    }

    Err(RubError::Internal(
        "Snapshot settle loop exhausted without producing a stable snapshot".to_string(),
    ))
}

struct PendingSnapshotPublicationGuard<'a> {
    state: &'a Arc<SessionState>,
    pending_scope: Option<crate::session::PendingExternalDomChangeState>,
}

impl<'a> PendingSnapshotPublicationGuard<'a> {
    fn new(state: &'a Arc<SessionState>) -> Self {
        Self {
            state,
            pending_scope: Some(state.take_pending_external_dom_change_scope()),
        }
    }

    fn pending_scope(&self) -> &crate::session::PendingExternalDomChangeState {
        self.pending_scope
            .as_ref()
            .expect("pending snapshot publication scope should exist until commit")
    }

    fn merge(&mut self, observed_scope: crate::session::PendingExternalDomChangeState) {
        if let Some(pending_scope) = self.pending_scope.as_mut() {
            pending_scope.merge(observed_scope);
        }
    }

    fn commit(&mut self) {
        self.pending_scope = None;
    }
}

impl Drop for PendingSnapshotPublicationGuard<'_> {
    fn drop(&mut self) {
        if let Some(pending_scope) = self.pending_scope.take() {
            self.state
                .merge_pending_external_dom_change_scope(pending_scope);
        }
    }
}

fn commit_snapshot_publication_epoch_for_pending_scope(
    state: &Arc<SessionState>,
    pending_scope: &crate::session::PendingExternalDomChangeState,
    snapshot: &mut Snapshot,
) -> Option<u64> {
    if pending_scope.is_empty() {
        None
    } else {
        let epoch = state.increment_epoch();
        snapshot.dom_epoch = epoch;
        Some(epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeferredSnapshotPublication, ExternalDomFenceOutcome, PendingSnapshotPublicationGuard,
        commit_snapshot_publication_epoch_for_pending_scope, settle_external_dom_fence,
        sleep_full_settle_window,
    };
    use crate::router::TransactionDeadline;
    use crate::session::SessionState;
    use rub_core::model::{FrameContextInfo, ScrollPosition, Snapshot, SnapshotProjection};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn settle_window_skips_partial_wait_when_deadline_is_too_close() {
        let waited = tokio::time::timeout(
            Duration::from_millis(30),
            sleep_full_settle_window(Some(TransactionDeadline::new(50)), 100),
        )
        .await
        .expect("settle wait should return immediately when the remaining deadline cannot cover the full window");
        assert!(!waited);
    }

    #[tokio::test]
    async fn settle_window_waits_when_deadline_covers_full_window() {
        let waited = tokio::time::timeout(
            Duration::from_millis(60),
            sleep_full_settle_window(Some(TransactionDeadline::new(50)), 10),
        )
        .await
        .expect("settle wait should sleep when the remaining deadline covers the full window");
        assert!(waited);
    }

    #[tokio::test]
    async fn settle_window_rejects_exact_boundary_without_partial_sleep() {
        let waited = tokio::time::timeout(
            Duration::from_millis(30),
            sleep_full_settle_window(Some(TransactionDeadline::new(100)), 100),
        )
        .await
        .expect("exact-boundary settle wait should return immediately");
        assert!(
            !waited,
            "deadline equal to the settle window must fail closed instead of partially sleeping"
        );
    }

    #[tokio::test]
    async fn settle_external_dom_fence_preserves_marker_when_deadline_is_too_close() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-fence"),
            None,
        ));
        state.mark_pending_external_dom_change();

        let outcome =
            settle_external_dom_fence(&state, "open", Some(TransactionDeadline::new(50))).await;

        assert_eq!(outcome, ExternalDomFenceOutcome::IncompleteDueToDeadline);
        assert!(state.has_pending_external_dom_change());
    }

    #[tokio::test(start_paused = true)]
    async fn settle_external_dom_fence_preserves_between_retry_mutations() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-fence-retry"),
            None,
        ));
        let task_state = state.clone();
        let fence =
            tokio::spawn(async move { settle_external_dom_fence(&task_state, "open", None).await });

        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_millis(50)).await;
        state.mark_pending_external_dom_change();

        tokio::time::advance(Duration::from_millis(50)).await;
        tokio::task::yield_now().await;

        state.mark_pending_external_dom_change();

        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert!(
            !fence.is_finished(),
            "a mutation that lands between settle retries must force another full quiet window"
        );

        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        let outcome = fence.await.expect("settle fence task should complete");
        assert_eq!(outcome, ExternalDomFenceOutcome::Settled);
        assert!(!state.has_pending_external_dom_change());
    }

    #[tokio::test]
    async fn settle_external_dom_fence_stops_before_partial_second_window() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-fence-near-timeout"),
            None,
        ));
        let task_state = state.clone();
        let fence = tokio::spawn(async move {
            settle_external_dom_fence(&task_state, "observe", Some(TransactionDeadline::new(150)))
                .await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        state.mark_pending_external_dom_change();

        let outcome = tokio::time::timeout(Duration::from_millis(250), fence)
            .await
            .expect("settle fence should complete before test timeout")
            .expect("settle fence task should complete");
        assert_eq!(outcome, ExternalDomFenceOutcome::IncompleteDueToDeadline);
        assert!(
            state.has_pending_external_dom_change(),
            "near-timeout settle must preserve the pending marker instead of pretending stability"
        );
    }

    #[tokio::test]
    async fn settle_external_dom_fence_preserves_target_scoped_pending_authority() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-fence-scoped"),
            None,
        ));
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        state.observe_external_dom_change(Some("target-1"));

        let outcome =
            settle_external_dom_fence(&state, "open", Some(TransactionDeadline::new(50))).await;

        assert_eq!(outcome, ExternalDomFenceOutcome::IncompleteDueToDeadline);
        let pending_scope = state.take_pending_external_dom_change_scope();
        assert!(pending_scope.affects_target(Some("target-1")));
        assert!(!pending_scope.affects_target(Some("target-2")));
    }

    fn snapshot(snapshot_id: &str, dom_epoch: u64) -> Snapshot {
        Snapshot {
            snapshot_id: snapshot_id.to_string(),
            dom_epoch,
            frame_context: FrameContextInfo {
                frame_id: "frame-main".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            frame_lineage: vec!["frame-main".to_string()],
            url: "https://example.test".to_string(),
            title: "Example".to_string(),
            elements: Vec::new(),
            total_count: 0,
            truncated: false,
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-04-18T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 0,
                backend_traversal_count: 0,
                resolved_ref_count: 0,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: None,
        }
    }

    #[tokio::test]
    async fn pending_dom_drift_commits_new_epoch_at_snapshot_publish_fence() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-publication-epoch"),
            None,
        ));
        state.mark_pending_external_dom_change();
        let pending_scope = state.take_pending_external_dom_change_scope();
        let mut snapshot = snapshot("snap-publication", 0);

        let committed_epoch = commit_snapshot_publication_epoch_for_pending_scope(
            &state,
            &pending_scope,
            &mut snapshot,
        );

        assert_eq!(committed_epoch, Some(1));
        assert_eq!(state.current_epoch(), 1);
        assert_eq!(snapshot.dom_epoch, 1);
        assert!(
            !state.has_pending_external_dom_change(),
            "snapshot publication should consume the preexisting pending authority only when the publish fence commits"
        );
    }

    #[tokio::test]
    async fn pending_dom_drift_remerge_preserves_pending_authority_before_publish_commit() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-publication-epoch-remerge"),
            None,
        ));
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        state.observe_external_dom_change(Some("target-1"));
        let pending_scope = state.take_pending_external_dom_change_scope();

        state.merge_pending_external_dom_change_scope(pending_scope);

        assert_eq!(state.current_epoch(), 0);
        assert!(
            state.pending_external_dom_change_affects_target(Some("target-1")),
            "failed snapshot publication must keep the pending fallback authority until a later publish fence commits a new epoch"
        );
    }

    #[tokio::test]
    async fn pending_snapshot_publication_guard_remerges_scope_on_error_drop() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-publication-guard-error"),
            None,
        ));
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        state.observe_external_dom_change(Some("target-1"));

        {
            let _guard = PendingSnapshotPublicationGuard::new(&state);
            assert!(
                !state.has_pending_external_dom_change(),
                "guard owns pending authority until snapshot publish either commits or fails"
            );
        }

        assert_eq!(state.current_epoch(), 0);
        assert!(
            state.pending_external_dom_change_affects_target(Some("target-1")),
            "early snapshot failure must restore the target-scoped fallback authority"
        );
        assert!(
            !state.pending_external_dom_change_affects_target(Some("target-2")),
            "fallback authority must preserve target scope when it is restored"
        );
    }

    #[tokio::test]
    async fn pending_snapshot_publication_guard_consumes_scope_only_on_commit() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-publication-guard-commit"),
            None,
        ));
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        state.observe_external_dom_change(Some("target-1"));

        {
            let mut guard = PendingSnapshotPublicationGuard::new(&state);
            guard.commit();
        }

        assert_eq!(state.current_epoch(), 0);
        assert!(
            !state.has_pending_external_dom_change(),
            "committed snapshot publication consumes the pending fallback authority"
        );
    }

    #[tokio::test]
    async fn fresh_snapshot_stays_uncached_until_publish() {
        let state = Arc::new(SessionState::new(
            "default",
            PathBuf::from("/tmp/rub-router-snapshot-deferred-publish"),
            None,
        ));
        let staged = DeferredSnapshotPublication::fresh(snapshot("snap-deferred", 0));

        assert!(
            state.get_snapshot("snap-deferred").await.is_none(),
            "fresh snapshots must not enter cache before the success fence is crossed"
        );

        let published = staged.publish(&state).await;

        assert_eq!(published.snapshot_id, "snap-deferred");
        assert!(state.get_snapshot("snap-deferred").await.is_some());
    }
}
