use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    FrameInventoryEntry, OrchestrationAddressInfo, OrchestrationAddressSpec,
    OrchestrationSessionInfo, TabInfo,
};
use rub_core::port::BrowserPort;
use rub_ipc::protocol::IpcRequest;

use crate::orchestration_executor::{
    RemoteDispatchContract, bind_live_orchestration_phase_command_id,
    decode_orchestration_success_result_items, dispatch_remote_orchestration_request,
    run_orchestration_future_with_outer_deadline,
};
use crate::orchestration_runtime::{
    extend_orchestration_session_path_context, orchestration_session_addressability_reason,
};
use crate::router::DaemonRouter;
use crate::router::TransactionDeadline;
use crate::session::SessionState;

use super::ORCHESTRATION_ADDRESS_TIMEOUT_MS;

pub(super) async fn resolve_orchestration_address(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    sessions: &[OrchestrationSessionInfo],
    spec: &OrchestrationAddressSpec,
    role: &'static str,
    deadline: Option<TransactionDeadline>,
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
    ensure_orchestration_session_addressable(session, role)?;
    let tabs = if session.session_id == state.session_id {
        list_local_orchestration_tabs(router, session, role, deadline).await?
    } else {
        list_remote_orchestration_tabs(session, role, deadline).await?
    };
    let tab = resolve_orchestration_tab_binding(&tabs, spec, role)?;
    let tab_index = Some(tab.index);
    let tab_target_id = Some(tab.target_id.clone());
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
            deadline,
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

fn ensure_orchestration_session_addressable(
    session: &OrchestrationSessionInfo,
    role: &str,
) -> Result<(), RubError> {
    let Some(reason) = orchestration_session_addressability_reason(session) else {
        return Ok(());
    };
    let mut context = serde_json::json!({
        "reason": format!("orchestration_{}_session_not_addressable", role),
        "session_id": session.session_id,
        "session_name": session.session_name,
        "availability": session.availability,
        "addressing_supported": session.addressing_supported,
        "addressability_reason": reason,
    });
    extend_orchestration_session_path_context(&mut context, session);
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        format!(
            "Orchestration {role} session '{}' is still present but not currently addressable",
            session.session_name
        ),
        context,
    ))
}

fn local_orchestration_inventory_unavailable_error(
    session: &OrchestrationSessionInfo,
    reason: String,
    message: String,
    extra_context: serde_json::Value,
) -> RubError {
    let mut context = serde_json::json!({
        "reason": reason,
        "session_id": session.session_id,
        "session_name": session.session_name,
    });
    extend_orchestration_session_path_context(&mut context, session);
    if let (Some(context_object), Some(extra_object)) =
        (context.as_object_mut(), extra_context.as_object())
    {
        for (key, value) in extra_object {
            context_object.insert(key.clone(), value.clone());
        }
    }
    RubError::domain_with_context(ErrorCode::SessionBusy, message, context)
}

async fn list_remote_orchestration_tabs(
    session: &OrchestrationSessionInfo,
    role: &'static str,
    deadline: Option<TransactionDeadline>,
) -> Result<Vec<TabInfo>, RubError> {
    let timeout_ms = bounded_orchestration_address_timeout_ms(deadline).ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::IpcTimeout,
            format!(
                "Orchestration {role} tab inventory query exhausted the caller-owned timeout budget before dispatch"
            ),
            serde_json::json!({
                "reason": format!("orchestration_{}_address_timeout_budget_exhausted", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
            }),
        )
    })?;
    let request = bind_remote_orchestration_inventory_request(
        IpcRequest::new("tabs", serde_json::json!({}), timeout_ms),
        role,
        RemoteInventorySubject::Tabs,
    )?;
    let response = dispatch_remote_orchestration_request(
        session,
        role,
        request,
        remote_orchestration_inventory_contract(role, RemoteInventorySubject::Tabs),
    )
    .await
    .map_err(RubError::Domain)?;

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
    role: &'static str,
    deadline: Option<TransactionDeadline>,
) -> Result<(), RubError> {
    let frames = if session.session_id == state.session_id {
        let browser = router.browser_port();
        list_local_orchestration_frames_for_tab(
            &browser,
            session,
            tab_target_id,
            frame_id,
            role,
            deadline,
        )
        .await?
    } else {
        list_remote_orchestration_frames_for_tab(session, tab_target_id, frame_id, role, deadline)
            .await?
    };
    validate_orchestration_frame_inventory(&frames, tab_target_id, frame_id, role)
}

async fn list_local_orchestration_frames_for_tab(
    browser: &Arc<dyn BrowserPort>,
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: &str,
    role: &str,
    deadline: Option<TransactionDeadline>,
) -> Result<Vec<FrameInventoryEntry>, RubError> {
    run_orchestration_future_with_outer_deadline(
        deadline,
        || {
            let mut context = serde_json::json!({
                "reason": format!("orchestration_{}_address_timeout_budget_exhausted", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
                "tab_target_id": tab_target_id,
                "frame_id": frame_id,
            });
            extend_orchestration_session_path_context(&mut context, session);
            RubError::domain_with_context(
                ErrorCode::IpcTimeout,
                format!(
                    "Orchestration {role} frame inventory query exhausted the caller-owned timeout budget before authoritative local inspection completed"
                ),
                context,
            )
        },
        async {
            browser
                .list_frames_for_tab(tab_target_id)
                .await
                .map_err(|error| {
                    local_orchestration_inventory_unavailable_error(
                        session,
                        format!("orchestration_{}_frame_inventory_unavailable", role),
                        format!("Unable to inspect orchestration {role} frame inventory: {error}"),
                        serde_json::json!({
                            "tab_target_id": tab_target_id,
                            "frame_id": frame_id,
                        }),
                    )
                })
        },
    )
    .await
}

async fn list_local_orchestration_tabs(
    router: &DaemonRouter,
    session: &OrchestrationSessionInfo,
    role: &'static str,
    deadline: Option<TransactionDeadline>,
) -> Result<Vec<TabInfo>, RubError> {
    run_orchestration_future_with_outer_deadline(
        deadline,
        || {
            let mut context = serde_json::json!({
                "reason": format!("orchestration_{}_address_timeout_budget_exhausted", role),
                "session_id": session.session_id,
                "session_name": session.session_name,
            });
            extend_orchestration_session_path_context(&mut context, session);
            RubError::domain_with_context(
                ErrorCode::IpcTimeout,
                format!(
                    "Orchestration {role} tab inventory query exhausted the caller-owned timeout budget before authoritative local inspection completed"
                ),
                context,
            )
        },
        async {
            router.browser.list_tabs().await.map_err(|error| {
                local_orchestration_inventory_unavailable_error(
                    session,
                    format!("orchestration_{}_local_tabs_query_failed", role),
                    format!("Unable to query local orchestration {role} tab inventory: {error}"),
                    serde_json::json!({
                    }),
                )
            })
        },
    )
    .await
}

async fn list_remote_orchestration_frames_for_tab(
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: &str,
    role: &'static str,
    deadline: Option<TransactionDeadline>,
) -> Result<Vec<FrameInventoryEntry>, RubError> {
    let timeout_ms = bounded_orchestration_address_timeout_ms(deadline).ok_or_else(|| {
        let mut context = serde_json::json!({
            "reason": format!("orchestration_{}_address_timeout_budget_exhausted", role),
            "session_id": session.session_id,
            "session_name": session.session_name,
            "tab_target_id": tab_target_id,
            "frame_id": frame_id,
        });
        extend_orchestration_session_path_context(&mut context, session);
        RubError::domain_with_context(
            ErrorCode::IpcTimeout,
            format!(
                "Orchestration {role} frame inventory query exhausted the caller-owned timeout budget before dispatch"
            ),
            context,
        )
    })?;
    let request = bind_remote_orchestration_inventory_request(
        IpcRequest::new(
            "_orchestration_tab_frames",
            serde_json::json!({ "tab_target_id": tab_target_id }),
            timeout_ms,
        ),
        role,
        RemoteInventorySubject::Frames,
    )?;
    let response = dispatch_remote_orchestration_request(
        session,
        role,
        request,
        remote_orchestration_inventory_contract(role, RemoteInventorySubject::Frames),
    )
    .await
    .map_err(RubError::Domain)?;

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

#[derive(Clone, Copy)]
enum RemoteInventorySubject {
    Tabs,
    Frames,
}

fn remote_orchestration_inventory_contract(
    role: &'static str,
    subject: RemoteInventorySubject,
) -> RemoteDispatchContract {
    match (role, subject) {
        ("source", RemoteInventorySubject::Tabs) => RemoteDispatchContract {
            dispatch_subject: "tab inventory query",
            unreachable_reason: "orchestration_source_session_unreachable",
            transport_failure_reason: "orchestration_source_tabs_query_failed",
            protocol_failure_reason: "orchestration_source_tabs_query_rejected",
            missing_error_message: "remote orchestration source tab inventory returned an error without an envelope",
        },
        ("target", RemoteInventorySubject::Tabs) => RemoteDispatchContract {
            dispatch_subject: "tab inventory query",
            unreachable_reason: "orchestration_target_session_unreachable",
            transport_failure_reason: "orchestration_target_tabs_query_failed",
            protocol_failure_reason: "orchestration_target_tabs_query_rejected",
            missing_error_message: "remote orchestration target tab inventory returned an error without an envelope",
        },
        ("source", RemoteInventorySubject::Frames) => RemoteDispatchContract {
            dispatch_subject: "frame inventory query",
            unreachable_reason: "orchestration_source_session_unreachable",
            transport_failure_reason: "orchestration_source_frame_inventory_query_failed",
            protocol_failure_reason: "orchestration_source_frame_inventory_query_rejected",
            missing_error_message: "remote orchestration source frame inventory returned an error without an envelope",
        },
        ("target", RemoteInventorySubject::Frames) => RemoteDispatchContract {
            dispatch_subject: "frame inventory query",
            unreachable_reason: "orchestration_target_session_unreachable",
            transport_failure_reason: "orchestration_target_frame_inventory_query_failed",
            protocol_failure_reason: "orchestration_target_frame_inventory_query_rejected",
            missing_error_message: "remote orchestration target frame inventory returned an error without an envelope",
        },
        _ => unreachable!("orchestration inventory role must remain source or target"),
    }
}

fn remote_orchestration_inventory_phase(
    role: &'static str,
    subject: RemoteInventorySubject,
) -> &'static str {
    match (role, subject) {
        ("source", RemoteInventorySubject::Tabs) => "orchestration_source_tab_inventory",
        ("target", RemoteInventorySubject::Tabs) => "orchestration_target_tab_inventory",
        ("source", RemoteInventorySubject::Frames) => "orchestration_source_frame_inventory",
        ("target", RemoteInventorySubject::Frames) => "orchestration_target_frame_inventory",
        _ => unreachable!("orchestration inventory role must remain source or target"),
    }
}

fn bind_remote_orchestration_inventory_request(
    request: IpcRequest,
    role: &'static str,
    subject: RemoteInventorySubject,
) -> Result<IpcRequest, RubError> {
    bind_live_orchestration_phase_command_id(
        request,
        remote_orchestration_inventory_phase(role, subject),
    )
    .map_err(RubError::Domain)
}

fn bounded_orchestration_address_timeout_ms(deadline: Option<TransactionDeadline>) -> Option<u64> {
    deadline
        .map(|deadline| ORCHESTRATION_ADDRESS_TIMEOUT_MS.min(deadline.remaining_ms()))
        .or(Some(ORCHESTRATION_ADDRESS_TIMEOUT_MS))
        .filter(|timeout_ms| *timeout_ms > 0)
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
    use super::{
        RemoteInventorySubject, bind_remote_orchestration_inventory_request,
        local_orchestration_inventory_unavailable_error, resolve_orchestration_tab_binding,
        validate_orchestration_frame_inventory,
    };
    use crate::orchestration_runtime::projected_orchestration_session;
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        FrameContextInfo, FrameInventoryEntry, OrchestrationAddressSpec,
        OrchestrationSessionAvailability, TabInfo,
    };
    use rub_ipc::protocol::IpcRequest;

    #[test]
    fn resolve_orchestration_tab_binding_prefers_target_id_when_present() {
        let tabs = vec![
            TabInfo {
                index: 0,
                target_id: "tab-a".to_string(),
                url: "https://example.com/a".to_string(),
                title: "A".to_string(),
                active: false,
                active_authority: None,
                degraded_reason: None,
            },
            TabInfo {
                index: 1,
                target_id: "tab-b".to_string(),
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
                active: true,
                active_authority: None,
                degraded_reason: None,
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
                active_authority: None,
                degraded_reason: None,
            },
            TabInfo {
                index: 3,
                target_id: "tab-b".to_string(),
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
                active: true,
                active_authority: None,
                degraded_reason: None,
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

    #[test]
    fn local_orchestration_inventory_unavailable_uses_session_busy_family() {
        let session = projected_orchestration_session(
            "sess-source".to_string(),
            "source".to_string(),
            42,
            "/tmp/rub-source.sock".to_string(),
            true,
            "1.0".to_string(),
            OrchestrationSessionAvailability::Addressable,
            Some("/tmp/rub-source-profile".to_string()),
        );

        let envelope = local_orchestration_inventory_unavailable_error(
            &session,
            "orchestration_source_frame_inventory_unavailable".to_string(),
            "Unable to inspect orchestration source frame inventory".to_string(),
            serde_json::json!({
                "tab_target_id": "tab-a",
                "frame_id": "frame-a",
            }),
        )
        .into_envelope();

        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_source_frame_inventory_unavailable")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("socket_path"))
                .and_then(|value| value.as_str()),
            Some("/tmp/rub-source.sock")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("frame_id"))
                .and_then(|value| value.as_str()),
            Some("frame-a")
        );
    }

    #[test]
    fn remote_orchestration_inventory_request_binds_live_request_scoped_phase_command_id() {
        let request_a = bind_remote_orchestration_inventory_request(
            IpcRequest::new("tabs", serde_json::json!({}), 5000),
            "source",
            RemoteInventorySubject::Tabs,
        )
        .expect("source tab inventory request should bind a live phase command_id");
        let request_b = bind_remote_orchestration_inventory_request(
            IpcRequest::new("tabs", serde_json::json!({}), 5000),
            "source",
            RemoteInventorySubject::Tabs,
        )
        .expect("live phase command_id should be request scoped");
        let request_c = bind_remote_orchestration_inventory_request(
            IpcRequest::new("tabs", serde_json::json!({}), 5000),
            "target",
            RemoteInventorySubject::Tabs,
        )
        .expect("target tab inventory should use a distinct phase command_id");

        assert_ne!(request_a.command_id, request_b.command_id);
        assert_ne!(request_a.command_id, request_c.command_id);
        assert!(request_a.command_id.as_deref().is_some_and(|command_id| {
            command_id.starts_with("orchestration_source_tab_inventory:")
        }));
    }
}
