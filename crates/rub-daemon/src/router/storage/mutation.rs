use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::storage::{StorageArea, StorageMutationKind, StorageSnapshot};

use crate::session::SessionState;

use super::DaemonRouter;
use super::args::{
    StorageClearArgs, StorageImportArgs, StorageRemoveArgs, StorageSetArgs, parse_storage_area,
    required_storage_area,
};
use super::commit::record_storage_commit;
use super::projection::{
    input_storage_artifact, output_storage_artifact, storage_payload, storage_subject,
    write_snapshot_file,
};

pub(super) async fn cmd_storage_set(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageSetArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let key = args.key;
    let value = args.value;
    let area = required_storage_area(Some(args.area.as_str()))?;
    let frame_id =
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
    let snapshot = router
        .browser
        .set_storage_item(frame_id.as_deref(), None, area, &key, &value)
        .await?;
    record_storage_commit(
        router,
        state,
        snapshot.clone(),
        StorageMutationKind::Set,
        Some(area),
        Some(key.clone()),
    )
    .await;
    Ok(storage_payload(
        storage_subject(&snapshot, Some(area), Some(key.as_str())),
        serde_json::json!({
            "value": value,
            "snapshot": snapshot,
        }),
        serde_json::json!(state.storage_runtime().await),
        None,
    ))
}

pub(super) async fn cmd_storage_remove(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageRemoveArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let key = args.key;
    let area = parse_storage_area(args.area.as_deref())?;
    let frame_id =
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
    let snapshot = if let Some(area) = area {
        router
            .browser
            .remove_storage_item(frame_id.as_deref(), None, area, &key)
            .await?
    } else {
        remove_storage_key_from_all_areas(router, frame_id.as_deref(), &key).await?
    };
    record_storage_commit(
        router,
        state,
        snapshot.clone(),
        StorageMutationKind::Remove,
        area,
        Some(key.clone()),
    )
    .await;
    Ok(storage_payload(
        storage_subject(&snapshot, area, Some(key.as_str())),
        serde_json::json!({
            "removed": true,
            "snapshot": snapshot,
        }),
        serde_json::json!(state.storage_runtime().await),
        None,
    ))
}

async fn remove_storage_key_from_all_areas(
    router: &DaemonRouter,
    frame_id: Option<&str>,
    key: &str,
) -> Result<StorageSnapshot, RubError> {
    let previous = router.browser.storage_snapshot(frame_id, None).await?;
    let local_previous = previous.local_storage.get(key).cloned();
    let after_local = router
        .browser
        .remove_storage_item(
            frame_id,
            Some(previous.origin.as_str()),
            StorageArea::Local,
            key,
        )
        .await?;
    match router
        .browser
        .remove_storage_item(
            frame_id,
            Some(after_local.origin.as_str()),
            StorageArea::Session,
            key,
        )
        .await
    {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => {
            let rollback = match local_previous {
                Some(ref value) => router
                    .browser
                    .set_storage_item(
                        frame_id,
                        Some(after_local.origin.as_str()),
                        StorageArea::Local,
                        key,
                        value,
                    )
                    .await
                    .map(|_| ()),
                None => Ok(()),
            };
            match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(RubError::domain_with_context(
                    ErrorCode::BrowserCrashed,
                    format!(
                        "storage remove failed after partially removing local storage: {error}"
                    ),
                    serde_json::json!({
                        "key": key,
                        "rollback_failed": true,
                        "rollback_error": rollback_error.into_envelope(),
                    }),
                )),
            }
        }
    }
}

pub(super) async fn cmd_storage_clear(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageClearArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let area = parse_storage_area(args.area.as_deref())?;
    let frame_id =
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
    let snapshot = router
        .browser
        .clear_storage(frame_id.as_deref(), None, area)
        .await?;
    record_storage_commit(
        router,
        state,
        snapshot.clone(),
        StorageMutationKind::Clear,
        area,
        None,
    )
    .await;
    Ok(storage_payload(
        storage_subject(&snapshot, area, None),
        serde_json::json!({
            "cleared": true,
            "snapshot": snapshot,
        }),
        serde_json::json!(state.storage_runtime().await),
        None,
    ))
}

pub(super) async fn cmd_storage_import(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageImportArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let path = args.path;
    let data = tokio::fs::read_to_string(&path).await.map_err(|error| {
        RubError::domain(
            ErrorCode::FileNotFound,
            format!("Cannot read storage snapshot file: {error}"),
        )
    })?;
    let snapshot: StorageSnapshot = serde_json::from_str(&data).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid storage snapshot JSON: {error}"),
        )
    })?;
    let frame_id =
        super::super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
    let snapshot = router
        .browser
        .replace_storage(
            frame_id.as_deref(),
            Some(snapshot.origin.as_str()),
            &snapshot,
        )
        .await?;
    record_storage_commit(
        router,
        state,
        snapshot.clone(),
        StorageMutationKind::Import,
        None,
        None,
    )
    .await;
    Ok(storage_payload(
        storage_subject(&snapshot, None, None),
        serde_json::json!({
            "imported": true,
            "snapshot": snapshot,
        }),
        serde_json::json!(state.storage_runtime().await),
        Some(input_storage_artifact(&path)),
    ))
}

pub(super) async fn cmd_storage_export(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: super::args::StorageExportArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let snapshot = super::commit::live_storage_snapshot(router, raw_args, state).await?;
    if let Some(path) = args.path.as_deref() {
        let commit_outcome = write_snapshot_file(path, &snapshot)?;
        return Ok(storage_payload(
            storage_subject(&snapshot, None, None),
            serde_json::json!({
                "snapshot": snapshot,
            }),
            serde_json::json!(state.storage_runtime().await),
            Some(output_storage_artifact(path, commit_outcome)),
        ));
    }
    Ok(storage_payload(
        storage_subject(&snapshot, None, None),
        serde_json::json!({
            "snapshot": snapshot,
        }),
        serde_json::json!(state.storage_runtime().await),
        None,
    ))
}
