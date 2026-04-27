use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::orchestration_runtime::projected_orchestration_session;
use crate::session::SessionState;
use rub_core::error::ErrorCode;
use rub_core::model::{
    FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, HumanVerificationHandoffInfo,
    HumanVerificationHandoffStatus, IntegrationMode, IntegrationRuntimeInfo,
    IntegrationRuntimeStatus, IntegrationSurface, OrchestrationAddressInfo,
    OrchestrationSessionAvailability, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
    SessionAccessibility, TabInfo, TakeoverRuntimeInfo, TakeoverRuntimeStatus,
    TakeoverVisibilityMode,
};
use rub_ipc::codec::NdJsonCodec;

use super::{
    OrchestrationTargetRuntimeSummary, dispatch_action_to_target_session,
    orchestration_target_continuity_failure, orchestration_target_dispatch_command_id,
    orchestration_target_dispatch_request, resolve_target_session, target_continuity_phase_request,
};
use crate::router::DaemonRouter;
use rub_ipc::protocol::IpcRequest;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn queue_test_connection(socket_path: &std::path::Path) -> UnixStream {
    let (client, server) = UnixStream::pair().expect("create in-memory unix stream pair");
    crate::orchestration_executor::protocol::queue_remote_orchestration_connection_for_test(
        socket_path,
        client,
    );
    server
}

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

fn target_rule() -> rub_core::model::OrchestrationRuleInfo {
    rub_core::model::OrchestrationRuleInfo {
        id: 1,
        status: rub_core::model::OrchestrationRuleStatus::Armed,
        lifecycle_generation: 1,
        source: OrchestrationAddressInfo {
            session_id: "sess-source".to_string(),
            session_name: "source".to_string(),
            tab_index: Some(0),
            tab_target_id: Some("source-tab".to_string()),
            frame_id: None,
        },
        target: target_address(),
        mode: rub_core::model::OrchestrationMode::Once,
        execution_policy: rub_core::model::OrchestrationExecutionPolicyInfo::default(),
        condition: rub_core::model::TriggerConditionSpec {
            kind: rub_core::model::TriggerConditionKind::TextPresent,
            locator: None,
            text: Some("ready".to_string()),
            url_pattern: None,
            readiness_state: None,
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        },
        actions: Vec::new(),
        correlation_key: "corr-target".to_string(),
        idempotency_key: "idem-target".to_string(),
        unavailable_reason: None,
        last_condition_evidence: None,
        last_result: None,
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
        active_authority: None,
        degraded_reason: None,
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
        active_authority: None,
        degraded_reason: None,
    };
    let error = orchestration_target_continuity_failure(&target_address(), &target_tab, &summary)
        .expect("required runtime degradation should block orchestration continuity");
    assert_eq!(error.code, ErrorCode::SessionBusy);
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

#[test]
fn target_dispatch_wrapper_uses_auto_generated_command_id_for_default_requests() {
    let address = target_address();
    let inner = IpcRequest::new("click", serde_json::json!({ "selector": "#go" }), 1_000);

    let wrapped = orchestration_target_dispatch_request(&address, inner.clone());

    assert!(inner.command_id.is_some());
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
        inner.command_id.as_deref()
    );
}

#[test]
fn target_continuity_phase_request_uses_live_request_scoped_phase_command_id() {
    let request_a = target_continuity_phase_request(
        "tabs",
        serde_json::json!({}),
        None,
        "orchestration_target_continuity_tabs",
    )
    .expect("phase request should bind live command_id");
    let request_b = target_continuity_phase_request(
        "tabs",
        serde_json::json!({}),
        None,
        "orchestration_target_continuity_tabs",
    )
    .expect("phase request should bind live command_id");

    assert!(request_a.command_id.is_some());
    assert_ne!(request_a.command_id, request_b.command_id);
    assert_eq!(request_a.command, "tabs");
}

#[test]
fn resolve_target_session_rejects_visible_but_non_addressable_session() {
    let session = projected_orchestration_session(
        "sess-target".to_string(),
        "target".to_string(),
        42,
        "/tmp/rub-target.sock".to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::ProtocolIncompatible,
        Some("/tmp/rub-target-profile".to_string()),
    );
    let runtime = rub_core::model::OrchestrationRuntimeInfo {
        status: rub_core::model::OrchestrationRuntimeStatus::Degraded,
        known_sessions: vec![session],
        session_count: 1,
        addressing_supported: true,
        execution_supported: true,
        current_session_id: Some("sess-local".to_string()),
        current_session_name: Some("default".to_string()),
        degraded_reason: Some("registry_contains_non_addressable_sessions".to_string()),
        ..Default::default()
    };

    let error =
        resolve_target_session(&runtime, &target_rule()).expect_err("non-addressable session");

    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|value| value.get("reason"))
            .and_then(|value| value.as_str()),
        Some("orchestration_target_session_not_addressable")
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|value| value.get("availability")),
        Some(&serde_json::json!("protocol_incompatible"))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|value| value.get("user_data_dir"))
            .and_then(|value| value.as_str()),
        Some("/tmp/rub-target-profile")
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|value| value.get("user_data_dir_state"))
            .and_then(|value| value.get("path_kind"))
            .and_then(|value| value.as_str()),
        Some("managed_user_data_directory")
    );
}

fn test_router() -> DaemonRouter {
    let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
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
    let first_stream = queue_test_connection(&socket_path);
    let replay_stream = queue_test_connection(&socket_path);

    let server = tokio::spawn(async move {
        let expected_outer_command_id = "orchestration_target_dispatch:sess-target:step-cmd";

        let stream = first_stream;
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

        let stream = replay_stream;
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
        .with_daemon_session_id("sess-target")
        .expect("daemon_session_id must be valid")
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
        OrchestrationSessionAvailability::Addressable,
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
        dispatch_action_to_target_session(&router, &state, &session, &address, request, None)
            .await
            .expect("wrapper replay should recover committed response");

    assert_eq!(
        response.command_id.as_deref(),
        Some("orchestration_target_dispatch:sess-target:step-cmd")
    );

    server.await.expect("server join");
}
