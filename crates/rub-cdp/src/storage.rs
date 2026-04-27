use std::collections::BTreeMap;
use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::FrameContextInfo;
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

impl StorageOperation<'_> {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Set { .. } => "set",
            Self::Remove { .. } => "remove",
            Self::Clear { .. } => "clear",
            Self::Replace { .. } => "replace",
        }
    }

    fn is_mutating(&self) -> bool {
        !matches!(self, Self::Snapshot)
    }
}

#[derive(Debug, Deserialize)]
struct StoragePayload {
    ok: bool,
    origin: Option<String>,
    error: Option<String>,
    #[serde(default)]
    mutation_committed: bool,
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
            operation.kind_name(),
            page.target_id().as_ref().to_string(),
            frame_context.frame.frame_id.clone(),
        ));
    }

    revalidate_storage_frame_authority(
        page,
        frame_id,
        &frame_context.frame,
        expected_origin,
        payload.origin.as_deref(),
        operation.kind_name(),
        operation.is_mutating(),
    )
    .await?;

    let origin = payload.origin.ok_or_else(|| {
        RubError::Internal("Storage payload omitted origin for successful operation".to_string())
    })?;
    Ok(StorageSnapshot {
        origin,
        tab_target_id: Some(page.target_id().as_ref().to_string()),
        frame_id: Some(frame_context.frame.frame_id),
        local_storage: payload.local_storage,
        session_storage: payload.session_storage,
    })
}

fn storage_payload_error(
    payload: StoragePayload,
    expected_origin: Option<&str>,
    operation_kind: &'static str,
    tab_target_id: String,
    frame_id: String,
) -> RubError {
    let error = payload
        .error
        .unwrap_or_else(|| "storage_operation_failed".to_string());
    let current_origin = payload.origin;
    let mutation_committed = payload.mutation_committed;
    let context = serde_json::json!({
        "tab_target_id": tab_target_id,
        "frame_id": frame_id,
        "expected_origin": expected_origin,
        "current_origin": current_origin,
        "operation_kind": operation_kind,
        "storage_mutation_committed": mutation_committed,
        "partial_commit": mutation_committed.then(|| serde_json::json!({
            "kind": "storage_mutation",
            "recovery_contract": {
                "kind": "partial_commit",
                "authoritative_surface": "storage_runtime.recent_mutations",
            },
        })),
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

async fn revalidate_storage_frame_authority(
    page: &Arc<Page>,
    requested_frame_id: Option<&str>,
    before: &FrameContextInfo,
    expected_origin: Option<&str>,
    current_origin: Option<&str>,
    operation_kind: &'static str,
    mutation_committed: bool,
) -> Result<(), RubError> {
    let after = crate::frame_runtime::resolve_frame_context(page, requested_frame_id)
        .await
        .map_err(|error| {
            storage_frame_authority_drift_error(StorageFrameAuthorityDrift {
                reason: "storage_frame_authority_unavailable_after_operation",
                expected_origin,
                current_origin,
                operation_kind,
                tab_target_id: page.target_id().as_ref().to_string(),
                frame_id: before.frame_id.clone(),
                mutation_committed,
                authority_error: Some(error),
            })
        })?;
    if let Some(reason) =
        storage_frame_authority_drift_reason(before, &after.frame, expected_origin, current_origin)
    {
        return Err(storage_frame_authority_drift_error(
            StorageFrameAuthorityDrift {
                reason,
                expected_origin,
                current_origin,
                operation_kind,
                tab_target_id: page.target_id().as_ref().to_string(),
                frame_id: before.frame_id.clone(),
                mutation_committed,
                authority_error: None,
            },
        ));
    }
    Ok(())
}

fn storage_frame_authority_drift_reason(
    before: &FrameContextInfo,
    after: &FrameContextInfo,
    expected_origin: Option<&str>,
    current_origin: Option<&str>,
) -> Option<&'static str> {
    if before.frame_id != after.frame_id {
        return Some("storage_frame_authority_changed_after_operation");
    }
    if let Some(expected_origin) = expected_origin {
        if current_origin.is_some_and(|origin| origin != expected_origin) {
            return Some("storage_frame_origin_changed_after_operation");
        }
        if storage_url_origin(after.url.as_deref()).is_some_and(|origin| origin != expected_origin)
        {
            return Some("storage_frame_origin_changed_after_operation");
        }
    }
    if expected_origin.is_none() && before.url != after.url {
        return Some("storage_frame_url_changed_without_expected_origin");
    }
    None
}

fn storage_url_origin(url: Option<&str>) -> Option<String> {
    let url = url?;
    let (scheme, rest) = url
        .strip_prefix("https://")
        .map(|rest| ("https", rest))
        .or_else(|| url.strip_prefix("http://").map(|rest| ("http", rest)))?;
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}

struct StorageFrameAuthorityDrift<'a> {
    reason: &'static str,
    expected_origin: Option<&'a str>,
    current_origin: Option<&'a str>,
    operation_kind: &'static str,
    tab_target_id: String,
    frame_id: String,
    mutation_committed: bool,
    authority_error: Option<RubError>,
}

fn storage_frame_authority_drift_error(drift: StorageFrameAuthorityDrift<'_>) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        "Storage frame authority changed while evaluating browser storage",
        serde_json::json!({
            "reason": drift.reason,
            "tab_target_id": drift.tab_target_id,
            "frame_id": drift.frame_id,
            "expected_origin": drift.expected_origin,
            "current_origin": drift.current_origin,
            "operation_kind": drift.operation_kind,
            "storage_mutation_committed": drift.mutation_committed,
            "partial_commit": drift.mutation_committed.then(|| serde_json::json!({
                "kind": "storage_mutation",
                "recovery_contract": {
                    "kind": "partial_commit",
                    "authoritative_surface": "storage_runtime.recent_mutations",
                },
            })),
            "authority_error": drift.authority_error.map(|error| error.into_envelope()),
        }),
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
            let mutationCommitted = false;

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
                        mutationCommitted = true;
                        break;
                    case 'remove':
                        storageForArea(operation.area).removeItem(String(operation.key));
                        mutationCommitted = true;
                        break;
                    case 'clear':
                        if (operation.area) {{
                            clearArea(operation.area);
                            mutationCommitted = true;
                        }} else {{
                            clearArea('local');
                            mutationCommitted = true;
                            clearArea('session');
                            mutationCommitted = true;
                        }}
                        break;
                    case 'replace':
                        if (!operation.snapshot || operation.snapshot.origin !== origin) {{
                            throw new Error(`snapshot_origin_mismatch:${{operation.snapshot && operation.snapshot.origin ? operation.snapshot.origin : 'unknown'}}`);
                        }}
                        clearArea('local');
                        mutationCommitted = true;
                        clearArea('session');
                        mutationCommitted = true;
                        for (const [key, value] of Object.entries(operation.snapshot.local_storage || {{}})) {{
                            window.localStorage.setItem(String(key), String(value));
                            mutationCommitted = true;
                        }}
                        for (const [key, value] of Object.entries(operation.snapshot.session_storage || {{}})) {{
                            window.sessionStorage.setItem(String(key), String(value));
                            mutationCommitted = true;
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
                    mutation_committed: mutationCommitted,
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
    use rub_core::model::FrameContextInfo;
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
                    tab_target_id: Some("tab-1".to_string()),
                    frame_id: Some("frame-1".to_string()),
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
        assert!(script.contains("let mutationCommitted = false"));
        assert!(
            script
                .contains("clearArea('local');\n                        mutationCommitted = true")
        );
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

    #[test]
    fn storage_payload_error_marks_committed_mutation_as_partial_commit() {
        let error = super::storage_payload_error(
            super::StoragePayload {
                ok: false,
                origin: Some("https://example.test".to_string()),
                error: Some("snapshot_failed_after_set".to_string()),
                mutation_committed: true,
                local_storage: BTreeMap::new(),
                session_storage: BTreeMap::new(),
            },
            Some("https://example.test"),
            "set",
            "tab-1".to_string(),
            "frame-1".to_string(),
        )
        .into_envelope();

        let context = error.context.expect("partial commit context");
        assert_eq!(context["storage_mutation_committed"], true);
        assert_eq!(context["tab_target_id"], "tab-1");
        assert_eq!(context["frame_id"], "frame-1");
        assert_eq!(
            context["partial_commit"]["recovery_contract"]["authoritative_surface"],
            "storage_runtime.recent_mutations"
        );
    }

    #[test]
    fn storage_frame_authority_revalidation_fails_closed_without_expected_origin() {
        let before = FrameContextInfo {
            frame_id: "frame-1".to_string(),
            name: None,
            parent_frame_id: None,
            target_id: Some("tab-1".to_string()),
            url: Some("https://before.test/app".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        };
        let after_same_frame_new_url = FrameContextInfo {
            url: Some("https://after.test/app".to_string()),
            ..before.clone()
        };
        let after_new_frame = FrameContextInfo {
            frame_id: "frame-2".to_string(),
            ..before.clone()
        };

        assert_eq!(
            super::storage_frame_authority_drift_reason(
                &before,
                &after_same_frame_new_url,
                None,
                Some("https://after.test"),
            ),
            Some("storage_frame_url_changed_without_expected_origin")
        );
        assert_eq!(
            super::storage_frame_authority_drift_reason(
                &before,
                &after_same_frame_new_url,
                Some("https://after.test"),
                Some("https://after.test"),
            ),
            None
        );
        assert_eq!(
            super::storage_frame_authority_drift_reason(
                &before,
                &after_same_frame_new_url,
                Some("https://before.test"),
                Some("https://after.test"),
            ),
            Some("storage_frame_origin_changed_after_operation")
        );
        assert_eq!(
            super::storage_frame_authority_drift_reason(
                &before,
                &after_new_frame,
                None,
                Some("https://before.test"),
            ),
            Some("storage_frame_authority_changed_after_operation")
        );
    }

    #[test]
    fn storage_url_origin_extracts_http_authority_only() {
        assert_eq!(
            super::storage_url_origin(Some("https://example.test/path?q=1#frag")).as_deref(),
            Some("https://example.test")
        );
        assert_eq!(super::storage_url_origin(Some("about:blank")), None);
    }
}
