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
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use crate::orchestration_runtime::projected_orchestration_session;
    use crate::session::SessionState;
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, HumanVerificationHandoffInfo,
        HumanVerificationHandoffStatus, IntegrationMode, IntegrationRuntimeInfo,
        IntegrationRuntimeStatus, IntegrationSurface, OrchestrationAddressInfo, OverlayState,
        ReadinessInfo, ReadinessStatus, RouteStability, SessionAccessibility, TabInfo,
        TakeoverRuntimeInfo, TakeoverRuntimeStatus, TakeoverVisibilityMode,
    };
    use rub_ipc::codec::NdJsonCodec;

    use super::{
        OrchestrationTargetRuntimeSummary, dispatch_action_to_target_session,
        orchestration_target_continuity_failure, orchestration_target_dispatch_command_id,
        orchestration_target_dispatch_request,
    };
    use crate::router::DaemonRouter;
    use rub_ipc::protocol::IpcRequest;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    fn runtime_summary() -> OrchestrationTargetRuntimeSummary {
        OrchestrationTargetRuntimeSummary {
            integration_runtime: IntegrationRuntimeInfo {
                mode: IntegrationMode::Normal,
                status: IntegrationRuntimeStatus::Active,
                request_rule_count: 0,
                request_rules: Vec::new(),
                active_surfaces: vec![
                    IntegrationSurface::Readiness,
                    IntegrationSurface::HumanVerificationHandoff,
                ],
                degraded_surfaces: Vec::new(),
                observatory_ready: true,
                readiness_ready: true,
                state_inspector_ready: true,
                handoff_ready: true,
            },
            frame_runtime: FrameRuntimeInfo {
                status: FrameContextStatus::Top,
                current_frame: Some(FrameContextInfo {
                    frame_id: "main-frame".to_string(),
                    name: Some("main".to_string()),
                    parent_frame_id: None,
                    target_id: Some("tab-target".to_string()),
                    url: Some("https://example.test".to_string()),
                    depth: 0,
                    same_origin_accessible: Some(true),
                }),
                primary_frame: Some(FrameContextInfo {
                    frame_id: "main-frame".to_string(),
                    name: Some("main".to_string()),
                    parent_frame_id: None,
                    target_id: Some("tab-target".to_string()),
                    url: Some("https://example.test".to_string()),
                    depth: 0,
                    same_origin_accessible: Some(true),
                }),
                frame_lineage: vec!["main-frame".to_string()],
                degraded_reason: None,
            },
            readiness_state: ReadinessInfo {
                status: ReadinessStatus::Active,
                route_stability: RouteStability::Stable,
                loading_present: false,
                skeleton_present: false,
                overlay_state: OverlayState::None,
                document_ready_state: Some("complete".to_string()),
                blocking_signals: Vec::new(),
                degraded_reason: None,
            },
            human_verification_handoff: HumanVerificationHandoffInfo {
                status: HumanVerificationHandoffStatus::Unavailable,
                automation_paused: false,
                resume_supported: false,
                unavailable_reason: Some("not_configured".to_string()),
            },
            takeover_runtime: TakeoverRuntimeInfo {
                status: TakeoverRuntimeStatus::Unavailable,
                session_accessibility: SessionAccessibility::AutomationOnly,
                visibility_mode: TakeoverVisibilityMode::Headless,
                elevate_supported: false,
                resume_supported: false,
                automation_paused: false,
                unavailable_reason: Some("not_configured".to_string()),
                last_transition: None,
            },
        }
    }

    fn target_address() -> OrchestrationAddressInfo {
        OrchestrationAddressInfo {
            session_id: "sess-target".to_string(),
            session_name: "target".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("tab-target".to_string()),
            frame_id: None,
        }
    }

    #[test]
    fn target_continuity_ignores_selected_frame_noise_for_explicit_frame_override() {
        let mut summary = runtime_summary();
        summary.frame_runtime.status = FrameContextStatus::Stale;
        summary.frame_runtime.current_frame = None;
        let mut address = target_address();
        address.frame_id = Some("child-frame".to_string());
        let target_tab = TabInfo {
            index: 0,
            target_id: "tab-target".to_string(),
            url: "https://example.test".to_string(),
            title: "Target".to_string(),
            active: true,
        };

        assert!(
            orchestration_target_continuity_failure(&address, &target_tab, &summary).is_none(),
            "explicit request-scoped frame routing should not be blocked by selected-frame noise"
        );
    }

    #[test]
    fn target_continuity_blocks_when_target_runtime_required_surface_degrades() {
        let mut summary = runtime_summary();
        summary.integration_runtime.status = IntegrationRuntimeStatus::Degraded;
        summary.integration_runtime.degraded_surfaces = vec![IntegrationSurface::Readiness];
        let target_tab = TabInfo {
            index: 0,
            target_id: "tab-target".to_string(),
            url: "https://example.test".to_string(),
            title: "Target".to_string(),
            active: true,
        };
        let error =
            orchestration_target_continuity_failure(&target_address(), &target_tab, &summary)
                .expect("required runtime degradation should block orchestration continuity");
        assert_eq!(error.code, ErrorCode::BrowserCrashed);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("continuity_runtime_degraded")
        );
    }

    #[test]
    fn target_dispatch_wrapper_inherits_inner_request_timeout() {
        let address = target_address();
        let inner = IpcRequest::new("wait", serde_json::json!({ "timeout_ms": 42_000 }), 42_000);

        let wrapped = orchestration_target_dispatch_request(&address, inner.clone());

        assert_eq!(wrapped.timeout_ms, 42_000);
        assert_eq!(
            wrapped
                .args
                .get("request")
                .and_then(|value| value.get("timeout_ms"))
                .and_then(|value| value.as_u64()),
            Some(42_000)
        );
    }

    #[test]
    fn target_dispatch_wrapper_uses_dedicated_command_id_for_replay() {
        let address = target_address();
        let inner = IpcRequest::new("click", serde_json::json!({ "selector": "#go" }), 1_000)
            .with_command_id("step-cmd")
            .expect("static command_id must be valid");

        let wrapped = orchestration_target_dispatch_request(&address, inner.clone());

        assert_eq!(
            wrapped.command_id.as_deref(),
            orchestration_target_dispatch_command_id(&address, &inner).as_deref()
        );
        assert_eq!(
            wrapped
                .args
                .get("request")
                .and_then(|value| value.get("command_id"))
                .and_then(|value| value.as_str()),
            Some("step-cmd")
        );
    }

    fn test_router() -> DaemonRouter {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        DaemonRouter::new(adapter)
    }

    #[tokio::test]
    async fn remote_target_dispatch_replays_partial_response_through_wrapper_command_id() {
        let socket_path = PathBuf::from(format!(
            "/tmp/rub-orch-target-{}.sock",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind listener");

        let server = tokio::spawn(async move {
            let expected_outer_command_id = "orchestration_target_dispatch:sess-target:step-cmd";

            let (stream, _) = listener.accept().await.expect("accept first");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read first request")
                .expect("first request");
            assert_eq!(request.command, "_orchestration_target_dispatch");
            assert_eq!(request.daemon_session_id.as_deref(), Some("sess-target"));
            assert_eq!(
                request.command_id.as_deref(),
                Some(expected_outer_command_id)
            );
            let inner_request: IpcRequest = serde_json::from_value(
                request
                    .args
                    .get("request")
                    .cloned()
                    .expect("wrapper request payload"),
            )
            .expect("decode inner request");
            assert_eq!(inner_request.command_id.as_deref(), Some("step-cmd"));
            writer
                .write_all(br#"{"ipc_protocol_version":"1.0","request_id":"req-1""#)
                .await
                .expect("write partial response");
            writer.shutdown().await.expect("shutdown partial writer");

            let (stream, _) = listener.accept().await.expect("accept replay");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let replay_request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read replay request")
                .expect("replay request");
            assert_eq!(replay_request.command, "_orchestration_target_dispatch");
            assert_eq!(
                replay_request.command_id.as_deref(),
                Some(expected_outer_command_id)
            );
            let replay_inner: IpcRequest = serde_json::from_value(
                replay_request
                    .args
                    .get("request")
                    .cloned()
                    .expect("replay wrapper request payload"),
            )
            .expect("decode replay inner request");
            assert_eq!(replay_inner.command_id.as_deref(), Some("step-cmd"));
            let response = rub_ipc::protocol::IpcResponse::success(
                "req-2",
                serde_json::json!({ "result": { "ok": true } }),
            )
            .with_command_id(expected_outer_command_id)
            .expect("static wrapper command_id must be valid");
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write replay response");
        });

        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orch-target-state"),
            None,
        ));
        let router = test_router();
        let session = projected_orchestration_session(
            "sess-target".to_string(),
            "target".to_string(),
            42,
            socket_path.display().to_string(),
            false,
            rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            None,
        );
        let address = OrchestrationAddressInfo {
            session_id: "sess-target".to_string(),
            session_name: "target".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("tab-target".to_string()),
            frame_id: Some("frame-target".to_string()),
        };
        let request = IpcRequest::new("click", serde_json::json!({ "selector": "#go" }), 1_000)
            .with_command_id("step-cmd")
            .expect("static step command_id must be valid");

        let response =
            dispatch_action_to_target_session(&router, &state, &session, &address, request)
                .await
                .expect("wrapper replay should recover committed response");

        assert_eq!(
            response.command_id.as_deref(),
            Some("orchestration_target_dispatch:sess-target:step-cmd")
        );

        server.await.expect("server join");
        let _ = std::fs::remove_file(&socket_path);
    }
}
