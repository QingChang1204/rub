use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes;
use rub_core::storage::{StorageArea, StorageMutationKind, StorageSnapshot};

use crate::runtime_refresh::refresh_live_runtime_state;
use crate::session::SessionState;

use super::DaemonRouter;
use super::request_args::parse_json_args;

#[derive(Debug)]
enum StorageCommand {
    Get(StorageGetArgs),
    Set(StorageSetArgs),
    Remove(StorageRemoveArgs),
    Clear(StorageClearArgs),
    Export(StorageExportArgs),
    Import(StorageImportArgs),
}

impl StorageCommand {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("export")
        {
            "get" => Ok(Self::Get(parse_json_args(args, "storage get")?)),
            "set" => Ok(Self::Set(parse_json_args(args, "storage set")?)),
            "remove" => Ok(Self::Remove(parse_json_args(args, "storage remove")?)),
            "clear" => Ok(Self::Clear(parse_json_args(args, "storage clear")?)),
            "export" => Ok(Self::Export(parse_json_args(args, "storage export")?)),
            "import" => Ok(Self::Import(parse_json_args(args, "storage import")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown storage subcommand '{other}'"),
            )),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectStorageArgs {
    #[serde(default)]
    area: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageGetArgs {
    #[serde(rename = "sub")]
    _sub: String,
    key: String,
    #[serde(default)]
    area: Option<String>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageSetArgs {
    #[serde(rename = "sub")]
    _sub: String,
    key: String,
    value: String,
    area: String,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageRemoveArgs {
    #[serde(rename = "sub")]
    _sub: String,
    key: String,
    #[serde(default)]
    area: Option<String>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageClearArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default)]
    area: Option<String>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageExportArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageImportArgs {
    #[serde(rename = "sub")]
    _sub: String,
    path: String,
    #[serde(default, rename = "_orchestration")]
    _orchestration: Option<serde_json::Value>,
}

pub(super) async fn cmd_storage(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match StorageCommand::parse(args)? {
        StorageCommand::Get(parsed) => cmd_storage_get(router, args, parsed, state).await,
        StorageCommand::Set(parsed) => cmd_storage_set(router, args, parsed, state).await,
        StorageCommand::Remove(parsed) => cmd_storage_remove(router, args, parsed, state).await,
        StorageCommand::Clear(parsed) => cmd_storage_clear(router, args, parsed, state).await,
        StorageCommand::Export(parsed) => cmd_storage_export(router, args, parsed, state).await,
        StorageCommand::Import(parsed) => cmd_storage_import(router, args, parsed, state).await,
    }
}

pub(super) async fn cmd_inspect_storage(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: InspectStorageArgs = parse_json_args(args, "inspect storage")?;
    let area = parse_storage_area(parsed.area.as_deref())?;
    let key = parsed.key.as_deref();
    let snapshot = live_storage_snapshot(router, args, state).await?;
    let result = build_storage_read_result(&snapshot, area, key)?;
    Ok(storage_payload(
        storage_subject(&snapshot, area, key),
        result,
        serde_json::json!(state.storage_runtime().await),
        None,
    ))
}

async fn cmd_storage_get(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageGetArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let area = parse_storage_area(args.area.as_deref())?;
    let snapshot = live_storage_snapshot(router, raw_args, state).await?;
    let result = build_storage_read_result(&snapshot, area, Some(args.key.as_str()))?;
    Ok(storage_payload(
        storage_subject(&snapshot, area, Some(args.key.as_str())),
        result,
        serde_json::json!(state.storage_runtime().await),
        None,
    ))
}

async fn cmd_storage_set(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageSetArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let key = args.key;
    let value = args.value;
    let area = required_storage_area(Some(args.area.as_str()))?;
    let frame_id = super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
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

async fn cmd_storage_remove(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageRemoveArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let key = args.key;
    let area = parse_storage_area(args.area.as_deref())?;
    let frame_id = super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
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

async fn cmd_storage_clear(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageClearArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let area = parse_storage_area(args.area.as_deref())?;
    let frame_id = super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
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

async fn cmd_storage_export(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: StorageExportArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let snapshot = live_storage_snapshot(router, raw_args, state).await?;
    if let Some(path) = args.path.as_deref() {
        write_snapshot_file(path, &snapshot)?;
        return Ok(storage_payload(
            storage_subject(&snapshot, None, None),
            serde_json::json!({
                "snapshot": snapshot,
            }),
            serde_json::json!(state.storage_runtime().await),
            Some(storage_artifact(path, "output")),
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

async fn cmd_storage_import(
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
    let frame_id = super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
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
        Some(storage_artifact(&path, "input")),
    ))
}

async fn live_storage_snapshot(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<StorageSnapshot, RubError> {
    let frame_id = super::frame_scope::effective_request_frame_id(router, args, state).await?;
    let snapshot = router
        .browser
        .storage_snapshot(frame_id.as_deref(), None)
        .await?;
    state.set_storage_snapshot(snapshot.clone()).await;
    Ok(snapshot)
}

async fn record_storage_commit(
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

fn build_storage_read_result(
    snapshot: &StorageSnapshot,
    area: Option<StorageArea>,
    key: Option<&str>,
) -> Result<serde_json::Value, RubError> {
    let snapshot_json = serde_json::to_value(snapshot).map_err(RubError::from)?;
    match key {
        Some(key) => {
            let matches = lookup_storage_matches(snapshot, area, key);
            if matches.is_empty() {
                return Err(RubError::domain_with_context(
                    ErrorCode::ElementNotFound,
                    format!("No storage entry found for key '{key}'"),
                    serde_json::json!({
                        "key": key,
                        "area": area.map(storage_area_name),
                        "origin": snapshot.origin,
                    }),
                ));
            }
            Ok(serde_json::json!({
                "matches": matches,
                "snapshot": snapshot_json,
            }))
        }
        None => {
            if let Some(area) = area {
                Ok(serde_json::json!({
                    "entries": area_entries(snapshot, area),
                    "snapshot": snapshot_json,
                }))
            } else {
                Ok(serde_json::json!({
                    "snapshot": snapshot_json,
                }))
            }
        }
    }
}

fn storage_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: serde_json::Value,
    artifact: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    });
    if let Some(object) = payload.as_object_mut()
        && let Some(artifact) = artifact
    {
        object.insert("artifact".to_string(), artifact);
    }
    payload
}

fn storage_subject(
    snapshot: &StorageSnapshot,
    area: Option<StorageArea>,
    key: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "storage",
        "origin": snapshot.origin,
        "area": area.map(storage_area_name),
        "key": key,
    })
}

fn storage_artifact(path: &str, direction: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "storage_snapshot",
        "format": "json",
        "path": path,
        "direction": direction,
    })
}

fn lookup_storage_matches(
    snapshot: &StorageSnapshot,
    area: Option<StorageArea>,
    key: &str,
) -> Vec<serde_json::Value> {
    match area {
        Some(area) => area_entries(snapshot, area)
            .get(key)
            .map(|value| {
                vec![serde_json::json!({
                    "area": storage_area_name(area),
                    "value": value,
                })]
            })
            .unwrap_or_default(),
        None => [StorageArea::Local, StorageArea::Session]
            .into_iter()
            .filter_map(|candidate| {
                area_entries(snapshot, candidate).get(key).map(|value| {
                    serde_json::json!({
                        "area": storage_area_name(candidate),
                        "value": value,
                    })
                })
            })
            .collect(),
    }
}

fn area_entries(snapshot: &StorageSnapshot, area: StorageArea) -> &BTreeMap<String, String> {
    match area {
        StorageArea::Local => &snapshot.local_storage,
        StorageArea::Session => &snapshot.session_storage,
    }
}

fn parse_storage_area(value: Option<&str>) -> Result<Option<StorageArea>, RubError> {
    match value {
        None => Ok(None),
        Some("local") => Ok(Some(StorageArea::Local)),
        Some("session") => Ok(Some(StorageArea::Session)),
        Some(other) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown storage area '{other}'. Valid: local, session"),
        )),
    }
}

fn required_storage_area(value: Option<&str>) -> Result<StorageArea, RubError> {
    parse_storage_area(value)?.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "Storage mutation requires --area <local|session>",
        )
    })
}

fn storage_area_name(area: StorageArea) -> &'static str {
    match area {
        StorageArea::Local => "local",
        StorageArea::Session => "session",
    }
}

fn write_snapshot_file(path: &str, snapshot: &StorageSnapshot) -> Result<(), RubError> {
    let json = serde_json::to_string_pretty(snapshot).map_err(|error| {
        RubError::Internal(format!("Serialize storage snapshot failed: {error}"))
    })?;
    atomic_write_bytes(Path::new(path), json.as_bytes(), 0o600)
        .map(|_| ())
        .map_err(|error| RubError::Internal(format!("Cannot write file: {error}")))
}

#[cfg(test)]
mod tests {
    use super::{
        InspectStorageArgs, StorageGetArgs, build_storage_read_result, storage_artifact,
        storage_subject,
    };
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn snapshot() -> StorageSnapshot {
        StorageSnapshot {
            origin: "https://example.test".to_string(),
            local_storage: BTreeMap::from([
                ("token".to_string(), "abc".to_string()),
                ("theme".to_string(), "dark".to_string()),
            ]),
            session_storage: BTreeMap::from([("csrf".to_string(), "def".to_string())]),
        }
    }

    #[test]
    fn storage_read_result_reports_matches_across_both_areas() {
        let payload =
            build_storage_read_result(&snapshot(), None, Some("token")).expect("key should exist");
        assert_eq!(
            payload["matches"],
            json!([{ "area": "local", "value": "abc" }])
        );
        assert_eq!(payload["snapshot"]["origin"], "https://example.test");
    }

    #[test]
    fn storage_read_result_reports_single_area_entries() {
        let payload = build_storage_read_result(&snapshot(), Some(StorageArea::Session), None)
            .expect("session entries should serialize");
        assert_eq!(payload["entries"], json!({ "csrf": "def" }));
        assert_eq!(
            payload["snapshot"]["session_storage"],
            json!({ "csrf": "def" })
        );
    }

    #[test]
    fn storage_read_result_reports_full_snapshot_and_canonical_snapshot_field() {
        let payload =
            build_storage_read_result(&snapshot(), None, None).expect("snapshot should serialize");
        assert_eq!(
            payload["snapshot"]["local_storage"],
            json!({ "theme": "dark", "token": "abc" })
        );
        assert_eq!(
            payload["snapshot"]["session_storage"],
            json!({ "csrf": "def" })
        );
    }

    #[test]
    fn storage_read_result_errors_when_key_is_missing() {
        let error = build_storage_read_result(&snapshot(), None, Some("missing"))
            .expect_err("missing key should error");
        assert_eq!(error.into_envelope().code, ErrorCode::ElementNotFound);
    }

    #[test]
    fn storage_subject_is_canonical_machine_facing_shape() {
        let subject = storage_subject(&snapshot(), Some(StorageArea::Local), Some("token"));
        assert_eq!(subject["kind"], "storage");
        assert_eq!(subject["origin"], "https://example.test");
        assert_eq!(subject["area"], "local");
        assert_eq!(subject["key"], "token");
    }

    #[test]
    fn storage_artifact_projects_directional_file_reference() {
        let artifact = storage_artifact("/tmp/storage.json", "output");
        assert_eq!(artifact["kind"], "storage_snapshot");
        assert_eq!(artifact["format"], "json");
        assert_eq!(artifact["path"], "/tmp/storage.json");
        assert_eq!(artifact["direction"], "output");
    }

    #[test]
    fn typed_storage_payload_rejects_unknown_fields() {
        let error = parse_json_args::<StorageGetArgs>(
            &json!({
                "sub": "get",
                "key": "token",
                "extra": true,
            }),
            "storage get",
        )
        .expect_err("unknown storage fields should fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn inspect_storage_payload_accepts_hidden_orchestration_frame_override() {
        let parsed = parse_json_args::<InspectStorageArgs>(
            &json!({
                "area": "local",
                "_orchestration": { "frame_id": "frame-1" },
            }),
            "inspect storage",
        )
        .expect("hidden orchestration payload should remain accepted");
        assert_eq!(parsed.area.as_deref(), Some("local"));
    }
}
