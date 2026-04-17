use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    FrameContextStatus, FrameInventoryEntry, FrameRuntimeInfo, HumanVerificationHandoffInfo,
    IntegrationRuntimeInfo, IntegrationRuntimeStatus, IntegrationSurface, OrchestrationAddressInfo,
    OrchestrationRuleInfo, OrchestrationRuntimeInfo, OrchestrationSessionInfo, ReadinessInfo,
    ReadinessStatus, TabInfo, TakeoverRuntimeInfo,
};
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use serde::Deserialize;

use crate::router::DaemonRouter;
use crate::session::SessionState;

use super::protocol::RemoteDispatchContract;
use super::{
    ORCHESTRATION_ACTION_BASE_TIMEOUT_MS, decode_orchestration_success_payload_field,
    decode_orchestration_success_result_items, dispatch_remote_orchestration_request,
    ensure_orchestration_success_response,
};

#[derive(Debug, Clone, Deserialize)]
struct OrchestrationTargetRuntimeSummary {
    integration_runtime: IntegrationRuntimeInfo,
    frame_runtime: FrameRuntimeInfo,
    readiness_state: ReadinessInfo,
    human_verification_handoff: HumanVerificationHandoffInfo,
    takeover_runtime: TakeoverRuntimeInfo,
}

pub(super) fn resolve_target_session<'a>(
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> Result<&'a OrchestrationSessionInfo, ErrorEnvelope> {
    runtime
        .known_sessions
        .iter()
        .find(|session| session.session_id == rule.target.session_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Target session '{}' is not available for orchestration execution",
                    rule.target.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_target_session_missing",
                "target_session_id": rule.target.session_id,
                "target_session_name": rule.target.session_name,
            }))
        })
}

pub(crate) async fn ensure_orchestration_target_continuity(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
    address: &OrchestrationAddressInfo,
) -> Result<TabInfo, ErrorEnvelope> {
    let tabs = list_target_tabs(router, state, session).await?;
    let target_tab = resolve_target_tab(&tabs, address)?;
    if target_tab.active {
        ensure_orchestration_target_frame_continuity(router, state, session, address).await?;
        let runtime_summary =
            fetch_orchestration_target_runtime_summary(router, state, session).await?;
        if let Some(error) =
            orchestration_target_continuity_failure(address, target_tab, &runtime_summary)
        {
            return Err(error);
        }
        return Ok(target_tab.clone());
    }

    let switch_request = IpcRequest::new(
        "switch",
        serde_json::json!({ "index": target_tab.index }),
        ORCHESTRATION_ACTION_BASE_TIMEOUT_MS,
    );
    dispatch_to_target_session(router, state, session, switch_request).await?;

    let tabs = list_target_tabs(router, state, session).await?;
    let target_tab = resolve_target_tab(&tabs, address)?;
    if !target_tab.active {
        return Err(ErrorEnvelope::new(
            ErrorCode::BrowserCrashed,
            "Orchestration target continuity fence failed: target tab is not active after switch",
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_target_not_active",
            "target_session_id": address.session_id,
            "target_tab_index": target_tab.index,
            "target_tab_target_id": target_tab.target_id,
        })));
    }
    ensure_orchestration_target_frame_continuity(router, state, session, address).await?;
    let runtime_summary =
        fetch_orchestration_target_runtime_summary(router, state, session).await?;
    if let Some(error) =
        orchestration_target_continuity_failure(address, target_tab, &runtime_summary)
    {
        return Err(error);
    }

    Ok(target_tab.clone())
}

async fn list_target_tabs(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
) -> Result<Vec<TabInfo>, ErrorEnvelope> {
    let request = IpcRequest::new(
        "tabs",
        serde_json::json!({}),
        ORCHESTRATION_ACTION_BASE_TIMEOUT_MS,
    );
    let response = dispatch_to_target_session(router, state, session, request).await?;
    decode_orchestration_success_result_items(
        response,
        session,
        "orchestration_target_tabs_payload_missing",
        "Orchestration target tabs response did not include a result.items payload",
        "orchestration_target_tabs_payload_invalid",
        "orchestration target tabs payload",
    )
}

async fn list_target_frames(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
) -> Result<Vec<FrameInventoryEntry>, ErrorEnvelope> {
    let request = IpcRequest::new(
        "frames",
        serde_json::json!({}),
        ORCHESTRATION_ACTION_BASE_TIMEOUT_MS,
    );
    let response = dispatch_to_target_session(router, state, session, request).await?;
    decode_orchestration_success_result_items(
        response,
        session,
        "orchestration_target_frames_payload_missing",
        "Orchestration target frames response did not include a result.items payload",
        "orchestration_target_frames_payload_invalid",
        "orchestration target frames payload",
    )
}

async fn ensure_orchestration_target_frame_continuity(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
    address: &OrchestrationAddressInfo,
) -> Result<(), ErrorEnvelope> {
    let Some(frame_id) = address.frame_id.as_deref() else {
        return Ok(());
    };
    let frames = list_target_frames(router, state, session).await?;
    let frame = frames
        .iter()
        .find(|entry| entry.frame.frame_id == frame_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                format!(
                    "Orchestration target frame '{frame_id}' is not present in session '{}'",
                    address.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "continuity_frame_unavailable",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_target_id": address.tab_target_id,
                "target_frame_id": frame_id,
            }))
        })?;
    if frame.is_primary || matches!(frame.frame.same_origin_accessible, Some(true)) {
        return Ok(());
    }
    Err(ErrorEnvelope::new(
        ErrorCode::BrowserCrashed,
        format!(
            "Orchestration target frame '{frame_id}' is not same-origin accessible for frame-scoped execution"
        ),
    )
    .with_context(serde_json::json!({
        "reason": "continuity_frame_unavailable",
        "target_session_id": address.session_id,
        "target_session_name": address.session_name,
        "target_tab_target_id": address.tab_target_id,
        "target_frame_id": frame_id,
        "same_origin_accessible": frame.frame.same_origin_accessible,
    })))
}

async fn fetch_orchestration_target_runtime_summary(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
) -> Result<OrchestrationTargetRuntimeSummary, ErrorEnvelope> {
    let request = IpcRequest::new(
        "runtime",
        serde_json::json!({ "sub": "summary" }),
        ORCHESTRATION_ACTION_BASE_TIMEOUT_MS,
    );
    let response = dispatch_to_target_session(router, state, session, request).await?;
    decode_orchestration_success_payload_field(
        response,
        session,
        "runtime",
        "orchestration_target_runtime_summary_payload_missing",
        "Orchestration target runtime summary returned success without a runtime payload",
        "orchestration_target_runtime_summary_payload_invalid",
        "orchestration target runtime summary",
    )
}

fn orchestration_target_continuity_failure(
    address: &OrchestrationAddressInfo,
    target_tab: &TabInfo,
    summary: &OrchestrationTargetRuntimeSummary,
) -> Option<ErrorEnvelope> {
    if summary.human_verification_handoff.automation_paused
        || summary.takeover_runtime.automation_paused
    {
        return Some(
            ErrorEnvelope::new(
                ErrorCode::AutomationPaused,
                "Orchestration target automation is paused by active handoff/takeover",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_target_automation_paused",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_target_id": address.tab_target_id,
                "target_frame_id": address.frame_id,
                "human_verification_handoff": summary.human_verification_handoff,
                "takeover_runtime": summary.takeover_runtime,
            })),
        );
    }

    if address.frame_id.is_none()
        && (matches!(
            summary.frame_runtime.status,
            FrameContextStatus::Unknown | FrameContextStatus::Stale | FrameContextStatus::Degraded
        ) || summary.frame_runtime.current_frame.is_none())
    {
        return Some(
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                "Orchestration target continuity fence failed: frame context became unavailable",
            )
            .with_context(serde_json::json!({
                "reason": "continuity_frame_unavailable",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_target_id": address.tab_target_id,
                "target_frame_id": address.frame_id,
                "frame_runtime": summary.frame_runtime,
                "readiness_state": summary.readiness_state,
            })),
        );
    }
    if address.frame_id.is_none()
        && summary
            .frame_runtime
            .current_frame
            .as_ref()
            .and_then(|frame| frame.target_id.as_deref())
            != Some(target_tab.target_id.as_str())
    {
        return Some(
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                "Orchestration target continuity fence failed: frame context no longer matches the target tab authority",
            )
            .with_context(serde_json::json!({
                "reason": "continuity_frame_target_mismatch",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_target_id": address.tab_target_id,
                "target_frame_id": address.frame_id,
                "frame_runtime": summary.frame_runtime,
            })),
        );
    }

    if matches!(summary.readiness_state.status, ReadinessStatus::Degraded) {
        return Some(
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                "Orchestration target continuity fence failed: readiness surface degraded",
            )
            .with_context(serde_json::json!({
                "reason": "continuity_readiness_degraded",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_target_id": address.tab_target_id,
                "target_frame_id": address.frame_id,
                "readiness_state": summary.readiness_state,
                "integration_runtime": summary.integration_runtime,
            })),
        );
    }

    let runtime_required_surface_degraded = summary
        .integration_runtime
        .degraded_surfaces
        .iter()
        .any(|surface| {
            matches!(
                surface,
                IntegrationSurface::Readiness | IntegrationSurface::HumanVerificationHandoff
            )
        });
    if matches!(
        summary.integration_runtime.status,
        IntegrationRuntimeStatus::Degraded
    ) && runtime_required_surface_degraded
    {
        return Some(
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                "Orchestration target continuity fence failed: integration runtime degraded",
            )
            .with_context(serde_json::json!({
                "reason": "continuity_runtime_degraded",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_target_id": address.tab_target_id,
                "target_frame_id": address.frame_id,
                "integration_runtime": summary.integration_runtime,
                "human_verification_handoff": summary.human_verification_handoff,
            })),
        );
    }

    None
}

fn resolve_target_tab<'a>(
    tabs: &'a [TabInfo],
    address: &OrchestrationAddressInfo,
) -> Result<&'a TabInfo, ErrorEnvelope> {
    if let Some(target_id) = address.tab_target_id.as_deref() {
        return tabs
            .iter()
            .find(|tab| tab.target_id == target_id)
            .ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::TabNotFound,
                    format!(
                        "Target tab '{}' is not present in session '{}'",
                        target_id, address.session_name
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "orchestration_target_tab_missing",
                    "target_session_id": address.session_id,
                    "target_session_name": address.session_name,
                    "target_tab_target_id": target_id,
                }))
            });
    }
    if let Some(index) = address.tab_index {
        return tabs.iter().find(|tab| tab.index == index).ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::TabNotFound,
                format!(
                    "Target tab index {} is not present in session '{}'",
                    index, address.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_target_tab_index_missing",
                "target_session_id": address.session_id,
                "target_session_name": address.session_name,
                "target_tab_index": index,
            }))
        });
    }
    tabs.iter().find(|tab| tab.active).ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::TabNotFound,
            format!(
                "Target session '{}' does not expose an active tab",
                address.session_name
            ),
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_target_active_tab_missing",
            "target_session_id": address.session_id,
            "target_session_name": address.session_name,
        }))
    })
}

pub(crate) async fn dispatch_to_target_session(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
    request: IpcRequest,
) -> Result<IpcResponse, ErrorEnvelope> {
    dispatch_target_request(router, state, session, request).await
}

pub(crate) async fn dispatch_action_to_target_session(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
    address: &OrchestrationAddressInfo,
    request: IpcRequest,
) -> Result<IpcResponse, ErrorEnvelope> {
    dispatch_target_request(
        router,
        state,
        session,
        orchestration_target_dispatch_request(address, request),
    )
    .await
}

async fn dispatch_target_request(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    session: &OrchestrationSessionInfo,
    request: IpcRequest,
) -> Result<IpcResponse, ErrorEnvelope> {
    if session.session_id == state.session_id {
        return ensure_orchestration_success_response(
            router
                .dispatch_within_active_transaction_preserving_replay(request, state)
                .await,
            "local orchestration dispatch returned an error without an envelope",
        );
    }

    dispatch_remote_orchestration_request(
        session,
        "target",
        request,
        RemoteDispatchContract {
            dispatch_subject: "request",
            unreachable_reason: "orchestration_target_session_unreachable",
            transport_failure_reason: "orchestration_target_dispatch_transport_failed",
            protocol_failure_reason: "orchestration_target_dispatch_protocol_failed",
            missing_error_message:
                "remote orchestration dispatch returned an error without an envelope",
        },
    )
    .await
}

fn orchestration_target_dispatch_request(
    address: &OrchestrationAddressInfo,
    request: IpcRequest,
) -> IpcRequest {
    let timeout_ms = request.timeout_ms;
    // The outer wrapper owns transport replay authority for the cross-session
    // dispatch itself. It derives a dedicated command_id from the inner step
    // command_id so the remote daemon can replay the wrapper request without
    // conflating it with the nested browser/action command.
    let command_id = orchestration_target_dispatch_command_id(address, &request);
    let request = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({
            "target": address,
            "request": request,
        }),
        timeout_ms,
    );
    if let Some(command_id) = command_id {
        request
            .with_command_id(command_id)
            .expect("derived orchestration target dispatch command_id must remain protocol-valid")
    } else {
        request
    }
}

fn orchestration_target_dispatch_command_id(
    address: &OrchestrationAddressInfo,
    request: &IpcRequest,
) -> Option<String> {
    request.command_id.as_ref().map(|command_id| {
        format!(
            "orchestration_target_dispatch:{}:{command_id}",
            address.session_id
        )
    })
}

#[cfg(test)]
mod tests;
