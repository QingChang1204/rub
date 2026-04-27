use std::collections::BTreeMap;
use std::path::Path;

use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::atomic_write_bytes_until;
use rub_core::storage::{StorageArea, StorageSnapshot};

use crate::router::artifacts::{
    INPUT_ARTIFACT_DURABILITY, annotate_file_artifact_state, output_artifact_durability,
};

pub(super) fn build_storage_read_result(
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

pub(super) fn storage_payload(
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

pub(super) fn storage_subject(
    snapshot: &StorageSnapshot,
    area: Option<StorageArea>,
    key: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "storage",
        "origin": snapshot.origin,
        "tab_target_id": snapshot.tab_target_id,
        "frame_id": snapshot.frame_id,
        "area": area.map(storage_area_name),
        "key": key,
    })
}

pub(super) fn output_storage_artifact(
    path: &str,
    commit_outcome: rub_core::fs::FileCommitOutcome,
) -> serde_json::Value {
    storage_artifact(path, "output", output_artifact_durability(commit_outcome))
}

pub(super) fn input_storage_artifact(path: &str) -> serde_json::Value {
    storage_artifact(path, "input", INPUT_ARTIFACT_DURABILITY)
}

pub(super) fn storage_artifact(path: &str, direction: &str, durability: &str) -> serde_json::Value {
    let mut artifact = serde_json::json!({
        "kind": "storage_snapshot",
        "format": "json",
        "path": path,
        "direction": direction,
    });
    let (artifact_authority, upstream_truth) = match direction {
        "output" => ("router.storage_export_artifact", "storage_snapshot_result"),
        "input" => ("router.storage_import_artifact", "storage_import_result"),
        _ => ("router.storage_artifact", "storage_result"),
    };
    annotate_file_artifact_state(
        &mut artifact,
        artifact_authority,
        upstream_truth,
        durability,
    );
    artifact
}

pub(super) fn write_snapshot_file(
    path: &str,
    snapshot: &StorageSnapshot,
    deadline: std::time::Instant,
) -> Result<rub_core::fs::FileCommitOutcome, RubError> {
    let json = serde_json::to_string_pretty(snapshot).map_err(|error| {
        RubError::Internal(format!("Serialize storage snapshot failed: {error}"))
    })?;
    atomic_write_bytes_until(Path::new(path), json.as_bytes(), 0o600, deadline).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            RubError::domain_with_context(
                ErrorCode::IpcTimeout,
                "storage export timed out before artifact publication could commit",
                serde_json::json!({
                    "reason": "storage_export_artifact_commit_timed_out",
                    "path": path,
                }),
            )
        } else {
            RubError::Internal(format!("Cannot write file: {error}"))
        }
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

fn storage_area_name(area: StorageArea) -> &'static str {
    match area {
        StorageArea::Local => "local",
        StorageArea::Session => "session",
    }
}

#[cfg(test)]
mod tests {
    use super::write_snapshot_file;
    use rub_core::error::ErrorCode;
    use rub_core::storage::StorageSnapshot;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    fn snapshot() -> StorageSnapshot {
        StorageSnapshot {
            origin: "https://example.test".to_string(),
            tab_target_id: Some("tab-1".to_string()),
            frame_id: Some("frame-1".to_string()),
            local_storage: BTreeMap::from([("token".to_string(), "abc".to_string())]),
            session_storage: BTreeMap::new(),
        }
    }

    #[test]
    fn storage_export_fails_closed_before_artifact_publication_after_deadline() {
        let root =
            std::env::temp_dir().join(format!("rub-storage-export-timeout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = root.join("storage.json");

        let error = write_snapshot_file(
            path.to_str().expect("utf8 path"),
            &snapshot(),
            Instant::now() - Duration::from_millis(1),
        )
        .expect_err("expired deadline must block storage artifact publication")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcTimeout);
        let context = error.context.expect("timeout error context");
        assert_eq!(
            context["reason"],
            "storage_export_artifact_commit_timed_out"
        );
        assert!(
            !path.exists(),
            "storage export must not publish a file after commit deadline expiry"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
