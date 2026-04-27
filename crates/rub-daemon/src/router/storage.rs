use std::sync::Arc;

use crate::router::request_args::parse_json_args;
use rub_core::error::RubError;

use crate::session::SessionState;

use super::{DaemonRouter, TransactionDeadline};

mod args;
mod commit;
mod mutation;
mod projection;

use self::args::{InspectStorageArgs, StorageCommand, StorageGetArgs, parse_storage_area};
use self::commit::live_storage_snapshot;
use self::mutation::{
    cmd_storage_clear, cmd_storage_export, cmd_storage_import, cmd_storage_remove, cmd_storage_set,
};
#[cfg(test)]
use self::projection::storage_artifact;
use self::projection::{build_storage_read_result, storage_payload, storage_subject};

pub(super) async fn cmd_storage(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match StorageCommand::parse(args)? {
        StorageCommand::Get(parsed) => cmd_storage_get(router, args, parsed, state).await,
        StorageCommand::Set(parsed) => cmd_storage_set(router, args, parsed, state).await,
        StorageCommand::Remove(parsed) => cmd_storage_remove(router, args, parsed, state).await,
        StorageCommand::Clear(parsed) => cmd_storage_clear(router, args, parsed, state).await,
        StorageCommand::Export(parsed) => {
            cmd_storage_export(router, args, parsed, deadline, state).await
        }
        StorageCommand::Import(parsed) => cmd_storage_import(router, args, parsed, state).await,
    }
}

pub(crate) fn semantic_replay_args(args: &serde_json::Value) -> Option<serde_json::Value> {
    let mut projected = serde_json::Map::new();
    match StorageCommand::parse(args).ok()? {
        StorageCommand::Get(parsed) => {
            projected.insert("sub".to_string(), serde_json::json!("get"));
            projected.insert("key".to_string(), serde_json::json!(parsed.key));
            projected.insert("area".to_string(), serde_json::json!(parsed.area));
        }
        StorageCommand::Set(parsed) => {
            projected.insert("sub".to_string(), serde_json::json!("set"));
            projected.insert("key".to_string(), serde_json::json!(parsed.key));
            projected.insert("value".to_string(), serde_json::json!(parsed.value));
            projected.insert("area".to_string(), serde_json::json!(parsed.area));
        }
        StorageCommand::Remove(parsed) => {
            projected.insert("sub".to_string(), serde_json::json!("remove"));
            projected.insert("key".to_string(), serde_json::json!(parsed.key));
            projected.insert("area".to_string(), serde_json::json!(parsed.area));
        }
        StorageCommand::Clear(parsed) => {
            projected.insert("sub".to_string(), serde_json::json!("clear"));
            projected.insert("area".to_string(), serde_json::json!(parsed.area));
        }
        StorageCommand::Export(parsed) => {
            projected.insert("sub".to_string(), serde_json::json!("export"));
            projected.insert("path".to_string(), serde_json::json!(parsed.path));
        }
        StorageCommand::Import(parsed) => {
            projected.insert("sub".to_string(), serde_json::json!("import"));
            projected.insert("path".to_string(), serde_json::json!(parsed.path));
        }
    }
    if let Some(orchestration) = super::frame_scope::semantic_replay_orchestration_metadata(args) {
        projected.insert("_orchestration".to_string(), orchestration);
    }
    Some(serde_json::Value::Object(projected))
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

#[cfg(test)]
mod tests {
    use super::args::{InspectStorageArgs, StorageExportArgs, StorageGetArgs, StorageImportArgs};
    use super::{build_storage_read_result, storage_artifact, storage_subject};
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn snapshot() -> StorageSnapshot {
        StorageSnapshot {
            origin: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
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
        let artifact = storage_artifact("/tmp/storage.json", "output", "durable");
        assert_eq!(artifact["kind"], "storage_snapshot");
        assert_eq!(artifact["format"], "json");
        assert_eq!(artifact["path"], "/tmp/storage.json");
        assert_eq!(artifact["direction"], "output");
        assert_eq!(
            artifact["artifact_state"]["truth_level"],
            "command_artifact"
        );
        assert_eq!(
            artifact["artifact_state"]["artifact_authority"],
            "router.storage_export_artifact"
        );
        assert_eq!(
            artifact["artifact_state"]["upstream_truth"],
            "storage_snapshot_result"
        );
        assert_eq!(artifact["artifact_state"]["durability"], "durable");
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

    #[test]
    fn inspect_storage_routing_sub_field_is_stripped_before_reaching_inspect_storage_args() {
        // Documentation test: confirm that InspectStorageArgs correctly rejects "sub".
        // This verifies that cmd_inspect's strip_inspect_routing_key is required —
        // if it were removed, inspect storage would fail with INVALID_INPUT.
        let error = parse_json_args::<InspectStorageArgs>(
            &json!({ "sub": "storage", "area": "local" }),
            "inspect storage",
        )
        .expect_err(
            "InspectStorageArgs must reject 'sub' — stripping is cmd_inspect's responsibility",
        );
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn inspect_storage_still_rejects_genuinely_unknown_fields() {
        // Guard: ensure the schema stays strict for all unknown fields.
        let error = parse_json_args::<InspectStorageArgs>(
            &json!({ "area": "local", "mystery": true }),
            "inspect storage",
        )
        .expect_err("unknown inspect storage fields must still be rejected")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn storage_export_import_payloads_accept_path_state_metadata() {
        let export = parse_json_args::<StorageExportArgs>(
            &json!({
                "sub": "export",
                "path": "/tmp/storage.json",
                "path_state": {
                    "path_authority": "cli.storage.export.path"
                }
            }),
            "storage export",
        )
        .expect("storage export payload should accept display-only path metadata");
        assert_eq!(export.path.as_deref(), Some("/tmp/storage.json"));

        let import = parse_json_args::<StorageImportArgs>(
            &json!({
                "sub": "import",
                "path": "/tmp/storage.json",
                "path_state": {
                    "path_authority": "cli.storage.import.path"
                }
            }),
            "storage import",
        )
        .expect("storage import payload should accept display-only path metadata");
        assert_eq!(import.path, "/tmp/storage.json");
    }
}
