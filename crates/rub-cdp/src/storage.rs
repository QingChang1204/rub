use std::collections::BTreeMap;
use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::storage::{StorageArea, StorageSnapshot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StorageOperation<'a> {
    Snapshot,
    Set {
        area: StorageArea,
        key: &'a str,
        value: &'a str,
    },
    Remove {
        area: StorageArea,
        key: &'a str,
    },
    Clear {
        area: Option<StorageArea>,
    },
    Replace {
        snapshot: &'a StorageSnapshot,
    },
}

#[derive(Debug, Deserialize)]
struct StoragePayload {
    ok: bool,
    origin: Option<String>,
    error: Option<String>,
    #[serde(default)]
    local_storage: BTreeMap<String, String>,
    #[serde(default)]
    session_storage: BTreeMap<String, String>,
}

pub async fn capture_storage_snapshot(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    expected_origin: Option<&str>,
) -> Result<StorageSnapshot, RubError> {
    execute_storage_operation(page, frame_id, expected_origin, StorageOperation::Snapshot).await
}

pub async fn set_storage_item(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    expected_origin: Option<&str>,
    area: StorageArea,
    key: &str,
    value: &str,
) -> Result<StorageSnapshot, RubError> {
    execute_storage_operation(
        page,
        frame_id,
        expected_origin,
        StorageOperation::Set { area, key, value },
    )
    .await
}

pub async fn remove_storage_item(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    expected_origin: Option<&str>,
    area: StorageArea,
    key: &str,
) -> Result<StorageSnapshot, RubError> {
    execute_storage_operation(
        page,
        frame_id,
        expected_origin,
        StorageOperation::Remove { area, key },
    )
    .await
}

pub async fn clear_storage(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    expected_origin: Option<&str>,
    area: Option<StorageArea>,
) -> Result<StorageSnapshot, RubError> {
    execute_storage_operation(
        page,
        frame_id,
        expected_origin,
        StorageOperation::Clear { area },
    )
    .await
}

pub async fn replace_storage(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    expected_origin: Option<&str>,
    snapshot: &StorageSnapshot,
) -> Result<StorageSnapshot, RubError> {
    execute_storage_operation(
        page,
        frame_id,
        expected_origin,
        StorageOperation::Replace { snapshot },
    )
    .await
}

async fn execute_storage_operation(
    page: &Arc<Page>,
    frame_id: Option<&str>,
    expected_origin: Option<&str>,
    operation: StorageOperation<'_>,
) -> Result<StorageSnapshot, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(page, frame_id).await?;
    let script = storage_operation_script(&operation, expected_origin)?;
    let payload: StoragePayload = serde_json::from_str(
        &crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            &script,
        )
        .await?,
    )
    .map_err(|error| RubError::Internal(format!("Parse storage payload failed: {error}")))?;

    if !payload.ok {
        return Err(storage_payload_error(
            payload,
            expected_origin,
            frame_context.frame.frame_id,
        ));
    }

    let origin = payload.origin.ok_or_else(|| {
        RubError::Internal("Storage payload omitted origin for successful operation".to_string())
    })?;
    Ok(StorageSnapshot {
        origin,
        local_storage: payload.local_storage,
        session_storage: payload.session_storage,
    })
}

fn storage_payload_error(
    payload: StoragePayload,
    expected_origin: Option<&str>,
    frame_id: String,
) -> RubError {
    let error = payload
        .error
        .unwrap_or_else(|| "storage_operation_failed".to_string());
    let current_origin = payload.origin;
    let context = serde_json::json!({
        "frame_id": frame_id,
        "expected_origin": expected_origin,
        "current_origin": current_origin,
        "storage_error": error,
    });

    if let Some(current_origin) = error.strip_prefix("origin_mismatch:") {
        let expected_origin = expected_origin.unwrap_or("<unspecified>");
        return RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Storage runtime origin mismatch: expected '{expected_origin}', current '{current_origin}'"
            ),
            context,
        );
    }

    if let Some(snapshot_origin) = error.strip_prefix("snapshot_origin_mismatch:") {
        return RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Imported storage snapshot origin '{snapshot_origin}' does not match the current page origin"
            ),
            context,
        );
    }

    if error == "opaque_origin" || error.contains("SecurityError") {
        return RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Web storage is unavailable for the current frame/origin",
            context,
        );
    }

    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!("Storage operation failed: {error}"),
        context,
    )
}

fn storage_operation_script(
    operation: &StorageOperation<'_>,
    expected_origin: Option<&str>,
) -> Result<String, RubError> {
    let operation = serde_json::to_string(operation).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize storage operation: {error}"),
        )
    })?;
    let expected_origin = serde_json::to_string(&expected_origin).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize expected storage origin: {error}"),
        )
    })?;

    Ok(format!(
        r#"JSON.stringify((() => {{
            const operation = {operation};
            const expectedOrigin = {expected_origin};

            const currentOrigin = () => {{
                try {{
                    return String(window.location && window.location.origin ? window.location.origin : '');
                }} catch (_) {{
                    return null;
                }}
            }};

            const storageForArea = (area) => {{
                if (area === 'local') return window.localStorage;
                if (area === 'session') return window.sessionStorage;
                throw new Error(`unsupported_storage_area:${{area}}`);
            }};

            const snapshotArea = (area) => {{
                const storage = storageForArea(area);
                const entries = {{}};
                for (let index = 0; index < storage.length; index += 1) {{
                    const key = storage.key(index);
                    if (key !== null) {{
                        entries[String(key)] = String(storage.getItem(key) ?? '');
                    }}
                }}
                return entries;
            }};

            const snapshot = () => {{
                const origin = currentOrigin();
                return {{
                    ok: true,
                    origin,
                    local_storage: snapshotArea('local'),
                    session_storage: snapshotArea('session'),
                }};
            }};

            const clearArea = (area) => {{
                storageForArea(area).clear();
            }};

            const ensureOrigin = (origin) => {{
                if (expectedOrigin !== null && origin !== expectedOrigin) {{
                    throw new Error(`origin_mismatch:${{origin}}`);
                }}
                if (origin === null || origin === 'null') {{
                    throw new Error('opaque_origin');
                }}
            }};

            try {{
                const origin = currentOrigin();
                ensureOrigin(origin);

                switch (operation.kind) {{
                    case 'snapshot':
                        break;
                    case 'set':
                        storageForArea(operation.area).setItem(String(operation.key), String(operation.value));
                        break;
                    case 'remove':
                        storageForArea(operation.area).removeItem(String(operation.key));
                        break;
                    case 'clear':
                        if (operation.area) {{
                            clearArea(operation.area);
                        }} else {{
                            clearArea('local');
                            clearArea('session');
                        }}
                        break;
                    case 'replace':
                        if (!operation.snapshot || operation.snapshot.origin !== origin) {{
                            throw new Error(`snapshot_origin_mismatch:${{operation.snapshot && operation.snapshot.origin ? operation.snapshot.origin : 'unknown'}}`);
                        }}
                        clearArea('local');
                        clearArea('session');
                        for (const [key, value] of Object.entries(operation.snapshot.local_storage || {{}})) {{
                            window.localStorage.setItem(String(key), String(value));
                        }}
                        for (const [key, value] of Object.entries(operation.snapshot.session_storage || {{}})) {{
                            window.sessionStorage.setItem(String(key), String(value));
                        }}
                        break;
                    default:
                        throw new Error(`unsupported_storage_operation:${{operation.kind}}`);
                }}

                return snapshot();
            }} catch (error) {{
                return {{
                    ok: false,
                    origin: currentOrigin(),
                    error: String(error && error.message ? error.message : error),
                    local_storage: {{}},
                    session_storage: {{}},
                }};
            }}
        }})())"#
    ))
}

#[cfg(test)]
mod tests {
    use super::storage_operation_script;
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use std::collections::BTreeMap;

    #[test]
    fn storage_operation_script_serializes_replace_snapshot_origin_guard() {
        let mut local = BTreeMap::new();
        local.insert("token".to_string(), "abc".to_string());
        let script = storage_operation_script(
            &super::StorageOperation::Replace {
                snapshot: &StorageSnapshot {
                    origin: "https://example.test".to_string(),
                    local_storage: local,
                    session_storage: BTreeMap::new(),
                },
            },
            Some("https://example.test"),
        )
        .expect("replace storage script should serialize");

        assert!(script.contains("\"kind\":\"replace\""));
        assert!(script.contains("\"origin\":\"https://example.test\""));
        assert!(script.contains("snapshot_origin_mismatch"));
    }

    #[test]
    fn storage_operation_script_serializes_set_operation() {
        let script = storage_operation_script(
            &super::StorageOperation::Set {
                area: StorageArea::Local,
                key: "token",
                value: "abc",
            },
            None,
        )
        .expect("set storage script should serialize");

        assert!(script.contains("\"kind\":\"set\""));
        assert!(script.contains("\"area\":\"local\""));
        assert!(script.contains("\"key\":\"token\""));
    }
}
