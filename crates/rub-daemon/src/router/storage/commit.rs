use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::storage::{StorageArea, StorageMutationKind, StorageSnapshot};

use crate::router::DaemonRouter;
use crate::runtime_refresh::refresh_live_runtime_state;
use crate::session::SessionState;
use crate::storage_runtime::StorageMutationRuntimeContext;

pub(super) async fn live_storage_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<StorageSnapshot, RubError> {
    let frame_id =
        crate::router::frame_scope::effective_request_frame_id(router, args, state).await?;
    let snapshot = router
        .browser
        .storage_snapshot(frame_id.as_deref(), None)
        .await?;
    state.set_storage_snapshot(snapshot.clone()).await;
    Ok(snapshot)
}

pub(super) async fn record_storage_commit(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    snapshot: StorageSnapshot,
    kind: StorageMutationKind,
    area: Option<StorageArea>,
    key: Option<String>,
) {
    let origin = snapshot.origin.clone();
    let tab_target_id = snapshot.tab_target_id.clone();
    let frame_id = snapshot.frame_id.clone();
    state.set_storage_snapshot(snapshot).await;
    state
        .record_storage_mutation(
            kind,
            origin,
            StorageMutationRuntimeContext {
                tab_target_id,
                frame_id,
                area,
                key,
                commit_status: Some("snapshot_committed".to_string()),
            },
        )
        .await;
    refresh_live_runtime_state(&router.browser, state).await;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoragePartialCommit {
    pub(super) origin: String,
    pub(super) tab_target_id: Option<String>,
    pub(super) frame_id: Option<String>,
}

pub(super) fn storage_partial_commit_from_error(error: &RubError) -> Option<StoragePartialCommit> {
    let RubError::Domain(envelope) = error else {
        return None;
    };
    let context = envelope.context.as_ref()?;
    if context
        .get("storage_mutation_committed")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let origin = context
        .get("current_origin")
        .and_then(serde_json::Value::as_str)?
        .to_string();
    Some(StoragePartialCommit {
        origin,
        tab_target_id: context
            .get("tab_target_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        frame_id: context
            .get("frame_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

pub(super) fn storage_partial_commit_from_snapshot(
    snapshot: &StorageSnapshot,
) -> StoragePartialCommit {
    StoragePartialCommit {
        origin: snapshot.origin.clone(),
        tab_target_id: snapshot.tab_target_id.clone(),
        frame_id: snapshot.frame_id.clone(),
    }
}

pub(super) async fn record_storage_partial_commit(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    partial: StoragePartialCommit,
    kind: StorageMutationKind,
    area: Option<StorageArea>,
    key: Option<String>,
) {
    state
        .record_storage_mutation(
            kind,
            partial.origin,
            StorageMutationRuntimeContext {
                tab_target_id: partial.tab_target_id,
                frame_id: partial.frame_id,
                area,
                key,
                commit_status: Some("mutation_committed_snapshot_unavailable".to_string()),
            },
        )
        .await;
    state
        .mark_storage_runtime_degraded("storage_mutation_committed_snapshot_unavailable")
        .await;
    refresh_live_runtime_state(&router.browser, state).await;
}

#[cfg(test)]
mod tests {
    use super::{storage_partial_commit_from_error, storage_partial_commit_from_snapshot};
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::storage::StorageSnapshot;
    use std::collections::BTreeMap;

    #[test]
    fn storage_partial_commit_from_error_extracts_authority_context() {
        let error = RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Storage operation failed after mutation commit",
            serde_json::json!({
                "storage_mutation_committed": true,
                "current_origin": "https://example.test",
                "tab_target_id": "tab-1",
                "frame_id": "frame-1",
            }),
        );

        let partial = storage_partial_commit_from_error(&error)
            .expect("committed storage mutation error should carry partial commit authority");

        assert_eq!(partial.origin, "https://example.test");
        assert_eq!(partial.tab_target_id.as_deref(), Some("tab-1"));
        assert_eq!(partial.frame_id.as_deref(), Some("frame-1"));
    }

    #[test]
    fn storage_partial_commit_from_snapshot_preserves_authority_context() {
        let partial = storage_partial_commit_from_snapshot(&StorageSnapshot {
            origin: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            local_storage: BTreeMap::new(),
            session_storage: BTreeMap::new(),
        });

        assert_eq!(partial.origin, "https://example.test");
        assert_eq!(partial.tab_target_id.as_deref(), Some("tab-1"));
        assert_eq!(partial.frame_id.as_deref(), Some("frame-1"));
    }
}
