use super::*;
const SNAPSHOT_SETTLE_DELAY_MS: u64 = 100;
const SNAPSHOT_SETTLE_RETRIES: usize = 6;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExternalDomFenceOutcome {
    Settled,
    IncompleteDueToDeadline,
    Unstable,
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
    state.clear_pending_external_dom_change();
    for attempt in 0..SNAPSHOT_SETTLE_RETRIES {
        if !sleep_full_settle_window(deadline, SNAPSHOT_SETTLE_DELAY_MS).await {
            state.mark_pending_external_dom_change();
            tracing::debug!(
                attempt,
                command = command_name,
                "External DOM settle fence stopped because the authoritative deadline cannot cover another full settle window",
            );
            return ExternalDomFenceOutcome::IncompleteDueToDeadline;
        }
        if !state.take_pending_external_dom_change() {
            return ExternalDomFenceOutcome::Settled;
        }
        if attempt + 1 == SNAPSHOT_SETTLE_RETRIES {
            state.mark_pending_external_dom_change();
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

    state.clear_pending_external_dom_change();

    for attempt in 0..SNAPSHOT_SETTLE_RETRIES {
        let snapshot = if listeners {
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
            state.mark_pending_external_dom_change();
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

        if !state.take_pending_external_dom_change() {
            return Ok(snapshot);
        }

        if attempt + 1 == SNAPSHOT_SETTLE_RETRIES {
            state.mark_pending_external_dom_change();
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

#[cfg(test)]
mod tests {
    use super::{ExternalDomFenceOutcome, settle_external_dom_fence, sleep_full_settle_window};
    use crate::router::TransactionDeadline;
    use crate::session::SessionState;
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
}
