use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::storage::{StorageArea, StorageMutationKind, StorageSnapshot};

use crate::session::SessionState;

use super::args::{
    StorageClearArgs, StorageImportArgs, StorageRemoveArgs, StorageSetArgs, parse_storage_area,
    required_storage_area,
};
use super::commit::{
    StoragePartialCommit, record_storage_commit, record_storage_partial_commit,
    storage_partial_commit_from_error, storage_partial_commit_from_snapshot,
};
use super::projection::{
    input_storage_artifact, output_storage_artifact, storage_payload, storage_subject,
    write_snapshot_file,
};
use super::{DaemonRouter, TransactionDeadline};

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
        .await
        .map_err(|error| {
            let partial = storage_partial_commit_from_error(&error);
            (error, partial)
        });
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err((error, Some(partial))) => {
            record_storage_partial_commit(
                router,
                state,
                partial,
                StorageMutationKind::Set,
                Some(area),
                Some(key.clone()),
            )
            .await;
            return Err(error);
        }
        Err((error, None)) => return Err(error),
    };
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
        match router
            .browser
            .remove_storage_item(frame_id.as_deref(), None, area, &key)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(partial) = storage_partial_commit_from_error(&error) {
                    record_storage_partial_commit(
                        router,
                        state,
                        partial,
                        StorageMutationKind::Remove,
                        Some(area),
                        Some(key.clone()),
                    )
                    .await;
                }
                return Err(error);
            }
        }
    } else {
        match remove_storage_key_from_all_areas(router, frame_id.as_deref(), &key).await {
            Ok(snapshot) => snapshot,
            Err((error, Some(partial))) => {
                record_storage_partial_commit(
                    router,
                    state,
                    partial,
                    StorageMutationKind::Remove,
                    None,
                    Some(key.clone()),
                )
                .await;
                return Err(error);
            }
            Err((error, None)) => return Err(error),
        }
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
) -> Result<StorageSnapshot, (RubError, Option<StoragePartialCommit>)> {
    let previous = router
        .browser
        .storage_snapshot(frame_id, None)
        .await
        .map_err(|error| (error, None))?;
    let local_previous = previous.local_storage.get(key).cloned();
    let after_local = router
        .browser
        .remove_storage_item(
            frame_id,
            Some(previous.origin.as_str()),
            StorageArea::Local,
            key,
        )
        .await
        .map_err(|error| {
            let partial = storage_partial_commit_from_error(&error);
            (error, partial)
        })?;
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
            let session_partial =
                storage_remove_session_failure_partial(&error, &after_local, false);
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
                Ok(()) => Err((error, session_partial)),
                Err(rollback_error) => {
                    let partial =
                        storage_remove_session_failure_partial(&error, &after_local, true);
                    Err((
                        RubError::domain_with_context(
                            ErrorCode::BrowserCrashed,
                            format!(
                                "storage remove failed after partially removing local storage: {error}"
                            ),
                            serde_json::json!({
                                "storage_mutation_committed": true,
                                "current_origin": after_local.origin,
                                "tab_target_id": after_local.tab_target_id,
                                "frame_id": after_local.frame_id,
                                "key": key,
                                "rollback_failed": true,
                                "rollback_error": rollback_error.into_envelope(),
                            }),
                        ),
                        partial,
                    ))
                }
            }
        }
    }
}

fn storage_remove_session_failure_partial(
    error: &RubError,
    after_local: &StorageSnapshot,
    rollback_failed: bool,
) -> Option<StoragePartialCommit> {
    let session_partial = storage_partial_commit_from_error(error);
    if rollback_failed {
        session_partial.or_else(|| Some(storage_partial_commit_from_snapshot(after_local)))
    } else {
        session_partial
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
        .await;
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if let Some(partial) = storage_partial_commit_from_error(&error) {
                record_storage_partial_commit(
                    router,
                    state,
                    partial,
                    StorageMutationKind::Clear,
                    area,
                    None,
                )
                .await;
            }
            return Err(error);
        }
    };
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
        .await;
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if let Some(partial) = storage_partial_commit_from_error(&error) {
                record_storage_partial_commit(
                    router,
                    state,
                    partial,
                    StorageMutationKind::Import,
                    None,
                    None,
                )
                .await;
            }
            return Err(error);
        }
    };
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
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let snapshot = super::commit::live_storage_snapshot(router, raw_args, state).await?;
    if let Some(path) = args.path.as_deref() {
        let commit_outcome = write_snapshot_file(path, &snapshot, deadline.deadline_instant())?;
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

#[cfg(test)]
mod tests {
    use super::storage_remove_session_failure_partial;
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::storage::StorageSnapshot;
    use std::collections::BTreeMap;

    fn snapshot_after_local_remove() -> StorageSnapshot {
        StorageSnapshot {
            origin: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            local_storage: BTreeMap::new(),
            session_storage: BTreeMap::new(),
        }
    }

    #[test]
    fn storage_remove_rollback_success_does_not_fabricate_partial_commit() {
        let error = RubError::domain(ErrorCode::BrowserCrashed, "session storage remove failed");

        let partial =
            storage_remove_session_failure_partial(&error, &snapshot_after_local_remove(), false);

        assert_eq!(
            partial, None,
            "compensated local storage removal is not an outstanding partial commit"
        );
    }

    #[test]
    fn storage_remove_rollback_failure_preserves_uncompensated_partial_commit() {
        let error = RubError::domain(ErrorCode::BrowserCrashed, "session storage remove failed");

        let partial =
            storage_remove_session_failure_partial(&error, &snapshot_after_local_remove(), true)
                .expect("failed rollback leaves the local removal as partial commit truth");

        assert_eq!(partial.origin, "https://example.test");
        assert_eq!(partial.tab_target_id.as_deref(), Some("tab-1"));
        assert_eq!(partial.frame_id.as_deref(), Some("frame-1"));
    }

    #[test]
    fn storage_remove_preserves_session_partial_commit_even_when_rollback_succeeds() {
        let error = RubError::domain_with_context(
            ErrorCode::BrowserCrashed,
            "session storage remove committed but snapshot failed",
            serde_json::json!({
                "storage_mutation_committed": true,
                "current_origin": "https://session.example",
                "tab_target_id": "tab-session",
                "frame_id": "frame-session",
            }),
        );

        let partial =
            storage_remove_session_failure_partial(&error, &snapshot_after_local_remove(), false)
                .expect("browser-reported session partial commit remains authoritative");

        assert_eq!(partial.origin, "https://session.example");
        assert_eq!(partial.tab_target_id.as_deref(), Some("tab-session"));
        assert_eq!(partial.frame_id.as_deref(), Some("frame-session"));
    }
}
