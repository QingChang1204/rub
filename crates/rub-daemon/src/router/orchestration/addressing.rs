use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationAddressSpec, OrchestrationSessionInfo, TabInfo,
};
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::{IpcRequest, ResponseStatus};

use crate::orchestration_executor::bind_orchestration_daemon_authority;
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
            RubError::domain_with_context(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Unable to reach orchestration {role} session '{}' at {}: {error}",
                    session.session_name, session.socket_path
                ),
                serde_json::json!({
                    "reason": format!("orchestration_{}_session_unreachable", role),
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                    "socket_path": session.socket_path,
                }),
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
            RubError::domain_with_context(
                ErrorCode::IpcProtocolError,
                format!(
                    "Failed to query orchestration {role} session '{}' tabs: {error}",
                    session.session_name
                ),
                serde_json::json!({
                    "reason": format!("orchestration_{}_tabs_query_failed", role),
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }),
            )
        })?;

    match response.status {
        ResponseStatus::Success => response
            .data
            .as_ref()
            .and_then(|data| data.get("tabs").cloned())
            .ok_or_else(|| {
                RubError::domain_with_context(
                    ErrorCode::IpcProtocolError,
                    format!(
                        "Orchestration {role} session '{}' returned tabs without a tabs payload",
                        session.session_name
                    ),
                    serde_json::json!({
                        "reason": format!("orchestration_{}_tabs_payload_missing", role),
                        "session_id": session.session_id,
                        "session_name": session.session_name,
                    }),
                )
            })
            .and_then(|tabs| serde_json::from_value::<Vec<TabInfo>>(tabs).map_err(RubError::from)),
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

#[cfg(test)]
mod tests {
    use super::resolve_orchestration_tab_binding;
    use rub_core::model::{OrchestrationAddressSpec, TabInfo};

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
}
