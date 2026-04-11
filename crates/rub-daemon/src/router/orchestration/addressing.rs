use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    FrameInventoryEntry, OrchestrationAddressInfo, OrchestrationAddressSpec,
    OrchestrationSessionInfo, TabInfo,
};
use rub_core::port::BrowserPort;
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::{IpcRequest, ResponseStatus};

use crate::orchestration_executor::{
    bind_orchestration_daemon_authority, decode_orchestration_success_result_items,
};
use crate::orchestration_runtime::extend_orchestration_session_path_context;
use crate::router::DaemonRouter;
use crate::session::SessionState;

use super::ORCHESTRATION_ADDRESS_TIMEOUT_MS;

pub(super) async fn resolve_orchestration_address(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    sessions: &[OrchestrationSessionInfo],
    spec: &OrchestrationAddressSpec,
    role: &str,
) -> Result<OrchestrationAddressInfo, RubError> {
    let session = sessions
        .iter()
        .find(|session| session.session_id == spec.session_id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "{role} session '{}' is not present in the current orchestration inventory",
                    spec.session_id
                ),
            )
        })?;
    let (tab_index, tab_target_id) = if spec.tab_index.is_some() || spec.tab_target_id.is_some() {
        let tabs = if session.session_id == state.session_id {
            router.browser.list_tabs().await.map_err(|error| {
                RubError::domain_with_context(
                    ErrorCode::BrowserCrashed,
                    format!("Unable to query local orchestration {role} tab inventory: {error}"),
                    serde_json::json!({
                        "reason": format!("orchestration_{}_local_tabs_query_failed", role),
                        "session_id": session.session_id,
                        "session_name": session.session_name,
                    }),
                )
            })?
        } else {
            list_remote_orchestration_tabs(session, role).await?
        };
        let tab = resolve_orchestration_tab_binding(&tabs, spec, role)?;
        (Some(tab.index), Some(tab.target_id.clone()))
    } else {
        (None, None)
    };
    if let (Some(tab_target_id), Some(frame_id)) =
        (tab_target_id.as_deref(), spec.frame_id.as_deref())
    {
        ensure_orchestration_address_frame_available(
            router,
            state,
            session,
            tab_target_id,
            frame_id,
            role,
        )
        .await?;
    }

    Ok(OrchestrationAddressInfo {
        session_id: session.session_id.clone(),
        session_name: session.session_name.clone(),
        tab_index,
        tab_target_id,
        frame_id: spec.frame_id.clone(),
    })
}

async fn list_remote_orchestration_tabs(
    session: &OrchestrationSessionInfo,
    role: &str,
) -> Result<Vec<TabInfo>, RubError> {
    let mut client = IpcClient::connect(std::path::Path::new(&session.socket_path))
        .await
        .map_err(|error| {
            let mut context = serde_json::json!({
                "reason": format!("orchestration_{}_session_unreachable", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
            });
            extend_orchestration_session_path_context(&mut context, session);
            RubError::domain_with_context(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Unable to reach orchestration {role} session '{}' at {}: {error}",
                    session.session_name, session.socket_path
                ),
                context,
            )
        })?;
    let response = client
        .send(
            &bind_orchestration_daemon_authority(
                IpcRequest::new(
                    "tabs",
                    serde_json::json!({}),
                    ORCHESTRATION_ADDRESS_TIMEOUT_MS,
                ),
                session,
                role,
            )
            .map_err(RubError::Domain)?,
        )
        .await
        .map_err(|error| {
            let mut context = serde_json::json!({
                "reason": format!("orchestration_{}_tabs_query_failed", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
            });
            extend_orchestration_session_path_context(&mut context, session);
            RubError::domain_with_context(
                ErrorCode::IpcProtocolError,
                format!(
                    "Failed to query orchestration {role} session '{}' tabs: {error}",
                    session.session_name
                ),
                context,
            )
        })?;

    match response.status {
        ResponseStatus::Success => {
            let missing_reason = format!("orchestration_{}_tabs_payload_missing", role);
            let invalid_reason = format!("orchestration_{}_tabs_payload_invalid", role);
            let missing_message = format!(
                "Orchestration {role} session '{}' returned tabs without a result.items payload",
                session.session_name
            );
            decode_orchestration_success_result_items::<TabInfo>(
                response,
                session,
                &missing_reason,
                &missing_message,
                &invalid_reason,
                "orchestration tab inventory payload",
            )
            .map_err(RubError::Domain)
        }
        ResponseStatus::Error => {
            let envelope = response.error.unwrap_or_else(|| {
                rub_core::error::ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "remote tabs request returned an error without an envelope",
                )
            });
            Err(RubError::domain_with_context(
                envelope.code,
                format!(
                    "Orchestration {role} session '{}' rejected tab inventory query: {}",
                    session.session_name, envelope.message
                ),
                serde_json::json!({
                    "reason": format!("orchestration_{}_tabs_query_rejected", role),
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                    "remote_error_code": envelope.code,
                }),
            ))
        }
    }
}

pub(super) fn resolve_orchestration_tab_binding<'a>(
    tabs: &'a [TabInfo],
    spec: &OrchestrationAddressSpec,
    role: &str,
) -> Result<&'a TabInfo, RubError> {
    if let Some(target_id) = spec.tab_target_id.as_deref() {
        return tabs
            .iter()
            .find(|tab| tab.target_id == target_id)
            .ok_or_else(|| {
                RubError::domain_with_context(
                    ErrorCode::TabNotFound,
                    format!(
                        "Orchestration {role} tab target '{}' is not present in the remote session",
                        target_id
                    ),
                    serde_json::json!({
                        "reason": format!("orchestration_{}_tab_target_missing", role),
                        "tab_target_id": target_id,
                    }),
                )
            });
    }

    if let Some(index) = spec.tab_index {
        return tabs.iter().find(|tab| tab.index == index).ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::TabNotFound,
                format!(
                    "Orchestration {role} tab index {} is not present in the remote session",
                    index
                ),
                serde_json::json!({
                    "reason": format!("orchestration_{}_tab_index_missing", role),
                    "tab_index": index,
                }),
            )
        });
    }

    tabs.iter().find(|tab| tab.active).ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::TabNotFound,
            format!(
                "Orchestration {role} address did not specify a tab and the remote session has no active tab"
            ),
            serde_json::json!({
                "reason": format!("orchestration_{}_active_tab_missing", role),
            }),
        )
    })
}

async fn ensure_orchestration_address_frame_available(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: &str,
    role: &str,
) -> Result<(), RubError> {
    let frames = if session.session_id == state.session_id {
        let browser = router.browser_port();
        list_local_orchestration_frames_for_tab(&browser, tab_target_id, frame_id, role).await?
    } else {
        list_remote_orchestration_frames_for_tab(session, tab_target_id, frame_id, role).await?
    };
    validate_orchestration_frame_inventory(&frames, tab_target_id, frame_id, role)
}

async fn list_local_orchestration_frames_for_tab(
    browser: &Arc<dyn BrowserPort>,
    tab_target_id: &str,
    frame_id: &str,
    role: &str,
) -> Result<Vec<FrameInventoryEntry>, RubError> {
    browser
        .list_frames_for_tab(tab_target_id)
        .await
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::BrowserCrashed,
                format!("Unable to inspect orchestration {role} frame inventory: {error}"),
                serde_json::json!({
                    "reason": format!("orchestration_{}_frame_inventory_unavailable", role),
                    "tab_target_id": tab_target_id,
                    "frame_id": frame_id,
                }),
            )
        })
}

async fn list_remote_orchestration_frames_for_tab(
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: &str,
    role: &str,
) -> Result<Vec<FrameInventoryEntry>, RubError> {
    let mut client = IpcClient::connect(std::path::Path::new(&session.socket_path))
        .await
        .map_err(|error| {
            let mut context = serde_json::json!({
                "reason": format!("orchestration_{}_session_unreachable", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
            });
            extend_orchestration_session_path_context(&mut context, session);
            RubError::domain_with_context(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Unable to reach orchestration {role} session '{}' at {}: {error}",
                    session.session_name, session.socket_path
                ),
                context,
            )
        })?;
    let response = client
        .send(
            &bind_orchestration_daemon_authority(
                IpcRequest::new(
                    "_orchestration_tab_frames",
                    serde_json::json!({ "tab_target_id": tab_target_id }),
                    ORCHESTRATION_ADDRESS_TIMEOUT_MS,
                ),
                session,
                role,
            )
            .map_err(RubError::Domain)?,
        )
        .await
        .map_err(|error| {
            let mut context = serde_json::json!({
                "reason": format!("orchestration_{}_frame_inventory_query_failed", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
                "tab_target_id": tab_target_id,
                "frame_id": frame_id,
            });
            extend_orchestration_session_path_context(&mut context, session);
            RubError::domain_with_context(
                ErrorCode::IpcProtocolError,
                format!(
                    "Failed to query orchestration {role} session '{}' frame inventory: {error}",
                    session.session_name
                ),
                context,
            )
        })?;

    match response.status {
        ResponseStatus::Success => {
            let missing_reason = format!("orchestration_{}_frames_payload_missing", role);
            let invalid_reason = format!("orchestration_{}_frames_payload_invalid", role);
            let missing_message = format!(
                "Orchestration {role} session '{}' returned frames without a result.items payload",
                session.session_name
            );
            decode_orchestration_success_result_items::<FrameInventoryEntry>(
                response,
                session,
                &missing_reason,
                &missing_message,
                &invalid_reason,
                "orchestration frame inventory payload",
            )
            .map_err(RubError::Domain)
        }
        ResponseStatus::Error => {
            let envelope = response.error.unwrap_or_else(|| {
                rub_core::error::ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "remote frames request returned an error without an envelope",
                )
            });
            Err(RubError::domain_with_context(
                envelope.code,
                format!(
                    "Orchestration {role} session '{}' rejected frame inventory query: {}",
                    session.session_name, envelope.message
                ),
                serde_json::json!({
                    "reason": format!("orchestration_{}_frame_inventory_query_rejected", role),
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                    "tab_target_id": tab_target_id,
                    "frame_id": frame_id,
                    "remote_error_code": envelope.code,
                }),
            ))
        }
    }
}

fn validate_orchestration_frame_inventory(
    frames: &[FrameInventoryEntry],
    tab_target_id: &str,
    frame_id: &str,
    role: &str,
) -> Result<(), RubError> {
    let entry = frames
        .iter()
        .find(|entry| entry.frame.frame_id == frame_id)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Orchestration {role} frame '{frame_id}' is not present in tab '{tab_target_id}'"
                ),
                serde_json::json!({
                    "reason": format!("orchestration_{}_frame_missing", role),
                    "tab_target_id": tab_target_id,
                    "frame_id": frame_id,
                }),
            )
        })?;
    if entry.is_primary || matches!(entry.frame.same_origin_accessible, Some(true)) {
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!(
            "Orchestration {role} frame '{frame_id}' is not same-origin accessible for frame-scoped execution"
        ),
        serde_json::json!({
            "reason": format!("orchestration_{}_frame_unavailable", role),
            "tab_target_id": tab_target_id,
            "frame_id": frame_id,
            "same_origin_accessible": entry.frame.same_origin_accessible,
            "index": entry.index,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::{resolve_orchestration_tab_binding, validate_orchestration_frame_inventory};
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        FrameContextInfo, FrameInventoryEntry, OrchestrationAddressSpec, TabInfo,
    };

    #[test]
    fn resolve_orchestration_tab_binding_prefers_target_id_when_present() {
        let tabs = vec![
            TabInfo {
                index: 0,
                target_id: "tab-a".to_string(),
                url: "https://example.com/a".to_string(),
                title: "A".to_string(),
                active: false,
            },
            TabInfo {
                index: 1,
                target_id: "tab-b".to_string(),
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
                active: true,
            },
        ];
        let valid = OrchestrationAddressSpec {
            session_id: "sess-source".to_string(),
            tab_index: None,
            tab_target_id: Some("tab-b".to_string()),
            frame_id: None,
        };
        let resolved = resolve_orchestration_tab_binding(&tabs, &valid, "source")
            .expect("target id binding should resolve");
        assert_eq!(resolved.index, 1);
        assert_eq!(resolved.target_id, "tab-b");
    }

    #[test]
    fn resolve_orchestration_tab_binding_uses_active_tab_when_unspecified() {
        let tabs = vec![
            TabInfo {
                index: 0,
                target_id: "tab-a".to_string(),
                url: "https://example.com/a".to_string(),
                title: "A".to_string(),
                active: false,
            },
            TabInfo {
                index: 3,
                target_id: "tab-b".to_string(),
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
                active: true,
            },
        ];
        let valid = OrchestrationAddressSpec {
            session_id: "sess-source".to_string(),
            tab_index: None,
            tab_target_id: None,
            frame_id: None,
        };
        let resolved = resolve_orchestration_tab_binding(&tabs, &valid, "target")
            .expect("active tab fallback should resolve");
        assert_eq!(resolved.index, 3);
        assert_eq!(resolved.target_id, "tab-b");
    }

    fn frame_inventory_entry(
        frame_id: &str,
        index: u32,
        same_origin_accessible: Option<bool>,
        is_primary: bool,
    ) -> FrameInventoryEntry {
        FrameInventoryEntry {
            index,
            is_current: false,
            is_primary,
            frame: FrameContextInfo {
                frame_id: frame_id.to_string(),
                name: Some(frame_id.to_string()),
                parent_frame_id: None,
                target_id: Some("tab-a".to_string()),
                url: Some(format!("https://example.test/{frame_id}")),
                depth: if is_primary { 0 } else { 1 },
                same_origin_accessible,
            },
        }
    }

    #[test]
    fn orchestration_frame_inventory_rejects_missing_frame() {
        let error = validate_orchestration_frame_inventory(
            &[frame_inventory_entry("frame-a", 0, Some(true), true)],
            "tab-a",
            "frame-b",
            "target",
        )
        .expect_err("missing frame should fail");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_target_frame_missing")
        );
    }

    #[test]
    fn orchestration_frame_inventory_rejects_cross_origin_child_frames() {
        let error = validate_orchestration_frame_inventory(
            &[frame_inventory_entry("frame-a", 2, Some(false), false)],
            "tab-a",
            "frame-a",
            "source",
        )
        .expect_err("non-switchable frame should fail");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_source_frame_unavailable")
        );
    }
}
