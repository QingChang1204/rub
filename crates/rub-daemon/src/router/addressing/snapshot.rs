use crate::runtime_refresh::refresh_live_frame_runtime;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::Snapshot;
use std::sync::Arc;

use super::super::snapshot::{DeferredSnapshotPublication, build_stable_snapshot};
use super::super::{DaemonRouter, TransactionDeadline};

pub(super) async fn load_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    prefer_a11y: bool,
) -> Result<Arc<Snapshot>, RubError> {
    if let Some(snapshot_id) = args.get("snapshot_id").and_then(|value| value.as_str()) {
        let requested_frame_id =
            if let Some(frame_id) = super::super::frame_scope::orchestration_frame_override(args) {
                super::super::frame_scope::ensure_request_frame_available(router, frame_id).await?;
                Some(frame_id.to_string())
            } else {
                None
            };
        refresh_live_frame_runtime(&router.browser, state).await;
        return load_cached_snapshot(state, snapshot_id, requested_frame_id.as_deref()).await;
    }

    let snapshot =
        build_stable_snapshot(router, args, state, deadline, Some(0), prefer_a11y, false).await?;
    let snapshot = state.cache_snapshot(snapshot).await;
    Ok(snapshot)
}

pub(super) async fn load_snapshot_deferred(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    prefer_a11y: bool,
) -> Result<DeferredSnapshotPublication, RubError> {
    if let Some(snapshot_id) = args.get("snapshot_id").and_then(|value| value.as_str()) {
        let requested_frame_id =
            if let Some(frame_id) = super::super::frame_scope::orchestration_frame_override(args) {
                super::super::frame_scope::ensure_request_frame_available(router, frame_id).await?;
                Some(frame_id.to_string())
            } else {
                None
            };
        refresh_live_frame_runtime(&router.browser, state).await;
        let snapshot =
            load_cached_snapshot(state, snapshot_id, requested_frame_id.as_deref()).await?;
        return Ok(DeferredSnapshotPublication::cached(snapshot));
    }

    let snapshot =
        build_stable_snapshot(router, args, state, deadline, Some(0), prefer_a11y, false).await?;
    Ok(DeferredSnapshotPublication::fresh(snapshot))
}

async fn load_cached_snapshot(
    state: &Arc<SessionState>,
    snapshot_id: &str,
    requested_frame_id: Option<&str>,
) -> Result<Arc<Snapshot>, RubError> {
    let snapshot = state.get_snapshot(snapshot_id).await.ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::StaleSnapshot,
            format!("Snapshot {snapshot_id} is unknown or evicted"),
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "current_epoch": state.current_epoch(),
            }),
        )
    })?;
    ensure_cached_snapshot_authority(state, snapshot_id, &snapshot, requested_frame_id).await?;
    Ok(snapshot)
}

async fn ensure_cached_snapshot_authority(
    state: &Arc<SessionState>,
    snapshot_id: &str,
    snapshot: &Snapshot,
    requested_frame_id: Option<&str>,
) -> Result<(), RubError> {
    let current_epoch = state.current_epoch();
    if snapshot.dom_epoch != current_epoch {
        return Err(RubError::domain_with_context(
            ErrorCode::StaleSnapshot,
            format!(
                "Snapshot {snapshot_id} is stale: snapshot epoch {} != current epoch {}",
                snapshot.dom_epoch, current_epoch
            ),
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "snapshot_epoch": snapshot.dom_epoch,
                "current_epoch": current_epoch,
            }),
        ));
    }

    if state.pending_external_dom_change_affects_target(snapshot.frame_context.target_id.as_deref())
    {
        return Err(RubError::domain_with_context(
            ErrorCode::StaleSnapshot,
            format!(
                "Snapshot {snapshot_id} cannot be reused because external DOM drift is pending for its authority target",
            ),
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "authority_state": "pending_external_dom_change",
                "target_id": snapshot.frame_context.target_id.clone(),
            }),
        ));
    }

    if let Some(requested_frame_id) = requested_frame_id {
        if snapshot.frame_context.frame_id == requested_frame_id {
            return Ok(());
        }
        return Err(RubError::domain_with_context(
            ErrorCode::StaleSnapshot,
            format!("Snapshot {snapshot_id} cannot satisfy the explicit frame-scoped request"),
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "authority_state": "explicit_frame_scope_mismatch",
            }),
        ));
    }

    let frame_runtime = state.frame_runtime().await;
    if matches!(
        frame_runtime.status,
        rub_core::model::FrameContextStatus::Stale
    ) {
        return Err(RubError::domain_with_context(
            ErrorCode::StaleSnapshot,
            format!(
                "Snapshot {snapshot_id} cannot be used because the selected frame context is stale"
            ),
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "authority_state": "selected_frame_context_stale",
            }),
        ));
    }
    let current_frame_id = frame_runtime
        .current_frame
        .as_ref()
        .map(|frame| frame.frame_id.as_str());
    if current_frame_id != Some(snapshot.frame_context.frame_id.as_str()) {
        return Err(RubError::domain_with_context(
            ErrorCode::StaleSnapshot,
            format!(
                "Snapshot {snapshot_id} cannot be used because the selected frame context changed after the snapshot was captured",
            ),
            serde_json::json!({
                "snapshot_id": snapshot_id,
                "authority_state": "selected_frame_context_drifted",
            }),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_cached_snapshot_authority;
    use crate::session::SessionState;
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, ScrollPosition, Snapshot,
        SnapshotProjection,
    };
    use std::sync::Arc;

    fn test_state(name: &str) -> Arc<SessionState> {
        Arc::new(SessionState::new(
            "default",
            std::env::temp_dir().join(format!(
                "rub-addressing-snapshot-{name}-{}",
                uuid::Uuid::now_v7()
            )),
            None,
        ))
    }

    fn frame_context(frame_id: &str) -> FrameContextInfo {
        FrameContextInfo {
            frame_id: frame_id.to_string(),
            name: Some(frame_id.to_string()),
            parent_frame_id: None,
            target_id: Some("target-1".to_string()),
            url: Some("https://example.test".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        }
    }

    fn cached_snapshot(snapshot_id: &str, dom_epoch: u64, frame_id: &str) -> Snapshot {
        Snapshot {
            snapshot_id: snapshot_id.to_string(),
            dom_epoch,
            frame_context: frame_context(frame_id),
            frame_lineage: vec![frame_id.to_string()],
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
            timestamp: "2026-04-10T00:00:00Z".to_string(),
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

    async fn set_current_frame(state: &Arc<SessionState>, frame_id: &str) {
        state
            .set_frame_runtime(FrameRuntimeInfo {
                status: FrameContextStatus::Top,
                current_frame: Some(frame_context(frame_id)),
                primary_frame: Some(frame_context(frame_id)),
                frame_lineage: vec![frame_id.to_string()],
                degraded_reason: None,
            })
            .await;
    }

    async fn set_stale_frame_runtime(state: &Arc<SessionState>) {
        state
            .set_frame_runtime(FrameRuntimeInfo {
                status: FrameContextStatus::Stale,
                current_frame: None,
                primary_frame: None,
                frame_lineage: Vec::new(),
                degraded_reason: Some("frame context unavailable".to_string()),
            })
            .await;
    }

    #[tokio::test]
    async fn cached_snapshot_authority_rejects_epoch_drift() {
        let state = test_state("epoch-drift");
        let snapshot = cached_snapshot("snap-1", 0, "frame-main");
        state.cache_snapshot(snapshot.clone()).await;
        state.increment_epoch();
        set_current_frame(&state, "frame-main").await;

        let error = ensure_cached_snapshot_authority(&state, "snap-1", &snapshot, None)
            .await
            .expect_err("epoch drift must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("snapshot_epoch")),
            Some(&serde_json::json!(0))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("current_epoch")),
            Some(&serde_json::json!(state.current_epoch()))
        );
    }

    #[tokio::test]
    async fn cached_snapshot_authority_rejects_frame_context_drift() {
        let state = test_state("frame-drift");
        let snapshot = cached_snapshot("snap-1", state.current_epoch(), "frame-main");
        state.cache_snapshot(snapshot.clone()).await;
        set_current_frame(&state, "frame-child").await;

        let error = ensure_cached_snapshot_authority(&state, "snap-1", &snapshot, None)
            .await
            .expect_err("frame drift must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("authority_state")),
            Some(&serde_json::json!("selected_frame_context_drifted"))
        );
        assert!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("frame_runtime"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn cached_snapshot_authority_rejects_pending_dom_drift_for_same_target() {
        let state = test_state("pending-dom-drift");
        let snapshot = cached_snapshot("snap-1", state.current_epoch(), "frame-main");
        state.cache_snapshot(snapshot.clone()).await;
        set_current_frame(&state, "frame-main").await;
        state
            .observe_external_dom_change(Some("target-1"))
            .expect("idle same-target drift should advance epoch");
        let snapshot = cached_snapshot("snap-2", state.current_epoch(), "frame-main");
        state.cache_snapshot(snapshot.clone()).await;
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        state.observe_external_dom_change(Some("target-1"));

        let error = ensure_cached_snapshot_authority(&state, "snap-2", &snapshot, None)
            .await
            .expect_err("pending same-target drift must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("authority_state")),
            Some(&serde_json::json!("pending_external_dom_change"))
        );
    }

    #[tokio::test]
    async fn cached_snapshot_authority_allows_unrelated_target_pending_dom_drift() {
        let state = test_state("pending-dom-drift-unrelated");
        let snapshot = cached_snapshot("snap-1", state.current_epoch(), "frame-main");
        state.cache_snapshot(snapshot.clone()).await;
        set_current_frame(&state, "frame-main").await;
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        state.observe_external_dom_change(Some("target-2"));

        ensure_cached_snapshot_authority(&state, "snap-1", &snapshot, None)
            .await
            .expect("unrelated target drift must not globally invalidate snapshot authority");
    }

    #[tokio::test]
    async fn cached_snapshot_authority_accepts_matching_explicit_frame_override() {
        let state = test_state("explicit-frame-match");
        let snapshot = cached_snapshot("snap-1", state.current_epoch(), "frame-child");
        state.cache_snapshot(snapshot.clone()).await;
        set_current_frame(&state, "frame-main").await;

        ensure_cached_snapshot_authority(&state, "snap-1", &snapshot, Some("frame-child"))
            .await
            .expect("matching explicit frame override should remain authoritative");
    }

    #[tokio::test]
    async fn cached_snapshot_authority_rejects_mismatching_explicit_frame_override() {
        let state = test_state("explicit-frame-mismatch");
        let snapshot = cached_snapshot("snap-1", state.current_epoch(), "frame-child");
        state.cache_snapshot(snapshot.clone()).await;
        set_current_frame(&state, "frame-child").await;

        let error =
            ensure_cached_snapshot_authority(&state, "snap-1", &snapshot, Some("frame-main"))
                .await
                .expect_err("explicit frame mismatch must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("authority_state")),
            Some(&serde_json::json!("explicit_frame_scope_mismatch"))
        );
        assert!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("requested_frame_id"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn cached_snapshot_authority_rejects_stale_selected_frame_without_runtime_leak() {
        let state = test_state("stale-frame-context");
        let snapshot = cached_snapshot("snap-1", state.current_epoch(), "frame-main");
        state.cache_snapshot(snapshot.clone()).await;
        set_stale_frame_runtime(&state).await;

        let error = ensure_cached_snapshot_authority(&state, "snap-1", &snapshot, None)
            .await
            .expect_err("stale selected frame context must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("authority_state")),
            Some(&serde_json::json!("selected_frame_context_stale"))
        );
        assert!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("frame_runtime"))
                .is_none()
        );
    }
}
