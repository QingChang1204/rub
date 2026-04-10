use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::storage::{StorageArea, StorageMutationKind, StorageSnapshot};

use crate::router::DaemonRouter;
use crate::runtime_refresh::refresh_live_runtime_state;
use crate::session::SessionState;

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
    state.set_storage_snapshot(snapshot).await;
    state.record_storage_mutation(kind, origin, area, key).await;
    refresh_live_runtime_state(&router.browser, state).await;
}
