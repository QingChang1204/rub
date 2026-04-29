use super::cookies::CookiesPathArgs;
use super::projection::{
    cookie_artifact, cookie_payload, cookie_subject, cookies_subject, runtime_subject,
    runtime_surface_payload,
};
use super::takeover::{
    takeover_continuity_failure, takeover_degraded_authority_error, takeover_resume_repaused_error,
    takeover_runtime_refresh_unavailable_error,
};
use super::{
    cmd_close, cmd_cookies, cmd_doctor, cmd_handoff, cmd_handshake, cmd_runtime, cmd_upgrade_check,
    intercept_payload, intercept_registry_subject, intercept_rule_id_subject,
    intercept_rule_subject, parse_json_args, project_network_rule,
};
use crate::router::runtime::projection::{
    annotate_doctor_operator_path_states, runtime_projection_state,
};
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    Cookie, FrameContextInfo, FrameContextStatus, HumanVerificationHandoffInfo,
    HumanVerificationHandoffStatus, IntegrationMode, IntegrationRuntimeInfo,
    IntegrationRuntimeStatus, IntegrationSurface, NetworkRule, NetworkRuleSpec, ReadinessInfo,
    ReadinessStatus, TakeoverRuntimeInfo, TakeoverRuntimeStatus,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

fn test_router() -> crate::router::DaemonRouter {
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
    crate::router::DaemonRouter::new(adapter)
}

#[tokio::test]
async fn handoff_start_and_complete_follow_session_state_machine() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));
    state.set_handoff_available(true).await;

    let started = cmd_handoff(&router, &serde_json::json!({ "sub": "start" }), &state)
        .await
        .expect("handoff start should succeed");
    assert_eq!(started["runtime"]["status"], "active");
    assert_eq!(started["runtime"]["automation_paused"], true);
    assert_eq!(started["runtime"]["resume_supported"], true);
    assert!(state.has_active_human_control().await);
    assert!(state.takeover_runtime().await.automation_paused);

    let completed = cmd_handoff(&router, &serde_json::json!({ "sub": "complete" }), &state)
        .await
        .expect("handoff complete should succeed");
    assert_eq!(completed["runtime"]["status"], "completed");
    assert_eq!(completed["runtime"]["automation_paused"], false);
    assert_eq!(completed["runtime"]["resume_supported"], true);
    assert!(!state.has_active_human_control().await);
    assert!(!state.takeover_runtime().await.automation_paused);
}

#[tokio::test]
async fn close_requests_shutdown_after_browser_release_commits() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));

    let payload = cmd_close(&router, &state)
        .await
        .expect("close should succeed");

    assert_eq!(payload["result"]["closed"], true);
    assert_eq!(
        payload["result"]["daemon_exit_policy"],
        "shutdown_requested_by_close"
    );
    assert!(state.is_shutdown_requested());
}

#[tokio::test]
async fn handoff_unavailable_rejects_start_and_complete() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));

    let start = cmd_handoff(&router, &serde_json::json!({ "sub": "start" }), &state).await;
    let complete = cmd_handoff(&router, &serde_json::json!({ "sub": "complete" }), &state).await;

    let start_code = match start {
        Err(RubError::Domain(envelope)) => envelope.code,
        other => panic!("expected invalid-input domain error, got {other:?}"),
    };
    let complete_code = match complete {
        Err(RubError::Domain(envelope)) => envelope.code,
        other => panic!("expected invalid-input domain error, got {other:?}"),
    };

    assert_eq!(start_code, ErrorCode::InvalidInput);
    assert_eq!(complete_code, ErrorCode::InvalidInput);
}

#[test]
fn project_network_rule_uses_canonical_action_and_pattern_fields_only() {
    let rule = NetworkRule {
        id: 7,
        status: rub_core::model::NetworkRuleStatus::Active,
        spec: NetworkRuleSpec::HeaderOverride {
            url_pattern: "https://example.com/*".to_string(),
            headers: std::collections::BTreeMap::from([(
                "x-rub-env".to_string(),
                "dev".to_string(),
            )]),
        },
    };

    let value = project_network_rule(&rule);
    assert_eq!(value["action"], "header_override");
    assert_eq!(value["pattern"], "https://example.com/*");
    assert!(value.get("kind").is_none(), "{value}");
    assert!(value.get("url_pattern").is_none(), "{value}");
}

#[test]
fn intercept_payload_uses_subject_result_runtime_envelope() {
    let payload = intercept_payload(
        intercept_registry_subject(),
        serde_json::json!({ "rules": [] }),
        serde_json::json!({ "status": "active" }),
    );
    assert_eq!(payload["subject"]["kind"], "intercept_rule_registry");
    assert_eq!(payload["result"]["rules"], serde_json::json!([]));
    assert_eq!(payload["runtime"]["status"], "active");
}

#[test]
fn runtime_surface_payload_marks_runtime_as_display_only_operator_projection() {
    let payload = runtime_surface_payload(
        runtime_subject("integration"),
        runtime_projection_state("integration", "session.integration_runtime"),
        serde_json::json!({ "status": "active" }),
    );
    assert_eq!(payload["subject"]["kind"], "runtime_surface");
    assert_eq!(payload["subject"]["surface"], "integration");
    assert_eq!(
        payload["runtime_projection_state"]["projection_kind"],
        "live_runtime_projection"
    );
    assert_eq!(
        payload["runtime_projection_state"]["projection_authority"],
        "session.integration_runtime"
    );
    assert_eq!(
        payload["runtime_projection_state"]["truth_level"],
        "operator_projection"
    );
    assert_eq!(
        payload["runtime_projection_state"]["control_role"],
        "display_only"
    );
    assert_eq!(payload["runtime"]["status"], "active");
}

#[tokio::test]
async fn doctor_marks_runtime_summary_as_operator_projection() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));

    let payload = cmd_doctor(&router, &state)
        .await
        .expect("doctor should succeed");
    assert_eq!(
        payload["runtime_projection_state"]["projection_kind"],
        "live_runtime_projection"
    );
    assert_eq!(
        payload["runtime_projection_state"]["projection_authority"],
        "session.runtime_summary"
    );
    assert_eq!(
        payload["runtime_projection_state"]["upstream_truth"],
        "session_live_runtime_state"
    );
    assert_eq!(
        payload["runtime_projection_state"]["truth_level"],
        "operator_projection"
    );
    assert!(payload["result"]["refresh_outcomes"].is_array());
    assert!(
        payload["result"]["refresh_outcomes"]
            .as_array()
            .is_some_and(|outcomes| outcomes
                .iter()
                .any(|outcome| outcome["surface"] == "runtime_state"))
    );
}

#[tokio::test]
async fn runtime_and_doctor_surface_post_commit_journal_degradation() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test-journal-surface"),
        None,
    ));
    state.force_post_commit_journal_failure_once();
    let request = rub_ipc::protocol::IpcRequest::new("state", serde_json::json!({}), 1_000);
    let response = rub_ipc::protocol::IpcResponse::success(
        "req-journal-surface",
        serde_json::json!({"ok": true}),
    );

    state
        .record_post_commit_journal(
            &request,
            &response,
            crate::workflow_capture::WorkflowCaptureDeliveryState::Delivered,
        )
        .await
        .expect_err("forced post-commit journal append should fail");

    let runtime = cmd_runtime(&router, &state, &serde_json::json!({ "sub": "summary" }))
        .await
        .expect("runtime summary should succeed");
    assert_eq!(
        runtime["runtime"]["post_commit_journal"]["status"],
        "degraded"
    );
    assert_eq!(
        runtime["runtime"]["post_commit_journal"]["failure_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        runtime["runtime"]["post_commit_journal"]["recovery_contract"]["kind"],
        "post_commit_journal_recovery"
    );

    let doctor = cmd_doctor(&router, &state)
        .await
        .expect("doctor should succeed");
    assert_eq!(
        doctor["result"]["post_commit_journal"]["status"],
        "degraded"
    );
    assert_eq!(
        doctor["result"]["post_commit_journal"]["failure_count"],
        serde_json::json!(1)
    );
}

#[tokio::test]
async fn handshake_and_upgrade_check_expose_automation_scheduler_inventory() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));

    let handshake = cmd_handshake(&router, &state)
        .await
        .expect("handshake should succeed");
    let upgrade = cmd_upgrade_check(&state)
        .await
        .expect("upgrade check should succeed");

    assert_eq!(
        handshake["automation_scheduler"]["slice"],
        "shared_fifo_scheduler_policy"
    );
    assert_eq!(
        handshake["automation_scheduler"]["authority_inventory"]["queue_owner"],
        "router.exec_semaphore"
    );
    assert_eq!(
        handshake["automation_scheduler"]["reservation_wait_policy"]["worker_cycle"]["mode"],
        "bounded_worker_cycle_budget"
    );
    assert_eq!(
        handshake["automation_scheduler"]["reservation_wait_policy"]["worker_cycle"]["wait_budget_ms"],
        500
    );
    assert_eq!(
        handshake["automation_scheduler"]["reservation_wait_policy"]["active_orchestration_step"]["mode"],
        "action_timeout_budget"
    );
    assert_eq!(
        handshake["automation_scheduler"]["reservation_wait_policy"]["active_orchestration_step"]["timeout_authority"],
        "orchestration_action_request.timeout_ms"
    );
    assert_eq!(
        upgrade["automation_scheduler"]["authority_inventory"]["shutdown_drain_fence"],
        "daemon.shutdown.wait_for_transaction_drain"
    );
    assert_eq!(
        upgrade["semantic_command_protocol"]["compatible"],
        serde_json::json!(true)
    );
    assert_eq!(
        upgrade["semantic_command_protocol"]["daemon_protocol_version"],
        rub_ipc::protocol::IPC_PROTOCOL_VERSION
    );
    assert_eq!(
        handshake["browser_event_ingress"]["critical"]["mode"],
        "lossless_metered_unbounded"
    );
    assert_eq!(
        upgrade["browser_event_ingress"]["progress"]["mode"],
        "bounded_drop_with_degraded_marker"
    );
}

#[tokio::test]
async fn doctor_includes_automation_scheduler_metrics() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));

    let payload = cmd_doctor(&router, &state)
        .await
        .expect("doctor should succeed");

    assert_eq!(
        payload["result"]["automation_scheduler"]["slice"],
        "shared_fifo_scheduler_policy"
    );
    assert_eq!(
        payload["result"]["automation_scheduler"]["authority_inventory"]["automation_reservation_fence"],
        "router.begin_automation_reservation_transaction_owned"
    );
    assert_eq!(
        payload["result"]["browser_event_ingress"]["critical"]["mode"],
        "lossless_metered_unbounded"
    );
}

#[test]
fn annotate_doctor_operator_path_states_marks_display_only_path_references() {
    let mut result = serde_json::json!({
        "browser": {
            "path": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        },
        "socket": {
            "path": "/tmp/rub.sock",
        },
        "disk": {
            "rub_home": "/tmp/rub-home",
        },
        "launch_policy": {
            "user_data_dir": "/tmp/rub-home/profile-root",
            "connection_target": {
                "source": "profile",
                "name": "Default",
                "resolved_path": "/tmp/rub-home/profile-root/Default",
            }
        },
    });

    annotate_doctor_operator_path_states(&mut result);

    assert_eq!(
        result["browser"]["path_state"]["truth_level"],
        "operator_path_reference"
    );
    assert_eq!(
        result["browser"]["path_state"]["path_authority"],
        "router.doctor.browser_path"
    );
    assert_eq!(
        result["browser"]["path_state"]["path_kind"],
        "browser_binary_reference"
    );
    assert_eq!(
        result["socket"]["path_state"]["path_authority"],
        "router.doctor.socket_path"
    );
    assert_eq!(
        result["socket"]["path_state"]["path_kind"],
        "daemon_socket_reference"
    );
    assert_eq!(
        result["disk"]["rub_home_state"]["path_authority"],
        "router.doctor.rub_home"
    );
    assert_eq!(
        result["disk"]["rub_home_state"]["path_kind"],
        "daemon_home_directory"
    );
    assert_eq!(
        result["disk"]["rub_home_state"]["control_role"],
        "display_only"
    );
    assert_eq!(
        result["launch_policy"]["user_data_dir_state"]["path_authority"],
        "router.doctor.launch_policy.user_data_dir"
    );
    assert_eq!(
        result["launch_policy"]["user_data_dir_state"]["path_kind"],
        "managed_user_data_directory"
    );
    assert_eq!(
        result["launch_policy"]["connection_target"]["resolved_path_state"]["path_authority"],
        "router.doctor.launch_policy.connection_target.resolved_path"
    );
    assert_eq!(
        result["launch_policy"]["connection_target"]["resolved_path_state"]["path_kind"],
        "profile_directory_reference"
    );
}

#[tokio::test]
async fn runtime_command_marks_surface_as_operator_projection() {
    let router = test_router();
    let state = Arc::new(SessionState::new(
        "default",
        PathBuf::from("/tmp/rub-test"),
        None,
    ));

    let payload = cmd_runtime(
        &router,
        &state,
        &serde_json::json!({ "sub": "integration" }),
    )
    .await
    .expect("runtime command should succeed");
    assert_eq!(
        payload["runtime_projection_state"]["projection_kind"],
        "live_runtime_projection"
    );
    assert_eq!(
        payload["runtime_projection_state"]["projection_authority"],
        "session.integration_runtime"
    );
    assert_eq!(
        payload["runtime_projection_state"]["durability"],
        "best_effort"
    );
    assert_eq!(
        payload["runtime_projection_state"]["control_role"],
        "display_only"
    );
    assert_eq!(payload["subject"]["surface"], "integration");
}

#[tokio::test]
async fn cookies_set_rejects_unknown_or_mistyped_fields() {
    let router = test_router();
    let error = cmd_cookies(
        &router,
        &serde_json::json!({
            "sub": "set",
            "name": "session",
            "value": "abc",
            "domain": "example.com",
            "secure": "true",
            "same_sitee": "Lax"
        }),
    )
    .await
    .expect_err("invalid cookie set payload must fail closed");

    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::InvalidInput);
    assert!(envelope.message.contains("Invalid cookies set payload"));
}

#[tokio::test]
async fn cookies_set_accepts_missing_optional_domain() {
    let router = test_router();
    let error = cmd_cookies(
        &router,
        &serde_json::json!({
            "sub": "set",
            "name": "session",
            "value": "abc",
            "path": "/",
            "same_site": "Lax"
        }),
    )
    .await
    .expect_err("test router still fails later without a real page, but parsing should succeed");

    let envelope = error.into_envelope();
    assert_ne!(envelope.code, ErrorCode::InvalidInput);
    assert!(
        !envelope.message.contains("Invalid cookies set payload"),
        "{envelope:?}"
    );
}

#[test]
fn intercept_subject_helpers_are_machine_facing() {
    let rule = NetworkRule {
        id: 3,
        status: rub_core::model::NetworkRuleStatus::Active,
        spec: NetworkRuleSpec::Rewrite {
            url_pattern: "https://example.com/*".to_string(),
            target_base: "http://localhost:3000/mock".to_string(),
        },
    };
    let subject = intercept_rule_subject(&rule);
    assert_eq!(subject["kind"], "intercept_rule");
    assert_eq!(subject["action"], "rewrite");
    assert_eq!(subject["pattern"], "https://example.com/*");

    let by_id = intercept_rule_id_subject(3);
    assert_eq!(by_id["kind"], "intercept_rule");
    assert_eq!(by_id["id"], 3);
}

#[test]
fn cookie_payload_uses_subject_result_and_artifact() {
    let cookie = Cookie {
        name: "sid".to_string(),
        value: "abc".to_string(),
        domain: "example.test".to_string(),
        path: "/".to_string(),
        secure: false,
        http_only: false,
        same_site: "Lax".to_string(),
        expires: None,
    };
    let payload = cookie_payload(
        cookie_subject(&cookie),
        serde_json::json!({ "cookie": cookie }),
        Some(cookie_artifact("/tmp/cookies.json", "output", "durable")),
    );
    assert_eq!(payload["subject"]["kind"], "cookie");
    assert_eq!(payload["result"]["cookie"]["name"], "sid");
    assert_eq!(payload["artifact"]["kind"], "cookies_archive");
    assert_eq!(payload["artifact"]["direction"], "output");
    assert_eq!(
        payload["artifact"]["artifact_state"]["truth_level"],
        "command_artifact"
    );
    assert_eq!(
        payload["artifact"]["artifact_state"]["artifact_authority"],
        "router.cookies_export_artifact"
    );
    assert_eq!(
        payload["artifact"]["artifact_state"]["upstream_truth"],
        "cookies_export_result"
    );
    assert_eq!(
        payload["artifact"]["artifact_state"]["durability"],
        "durable"
    );
}

#[test]
fn cookies_subject_is_url_scoped_collection_identity() {
    let subject = cookies_subject(Some("https://example.test"));
    assert_eq!(subject["kind"], "cookies");
    assert_eq!(subject["url"], "https://example.test");
}

#[test]
fn continuity_failure_requires_active_tab_and_live_frame_context() {
    let frame_runtime = rub_core::model::FrameRuntimeInfo::default();
    let readiness = ReadinessInfo::default();
    let integration = IntegrationRuntimeInfo::default();

    let no_tab = takeover_continuity_failure(false, &frame_runtime, &readiness, &integration);
    assert_eq!(
        no_tab,
        Some((
            "continuity_no_active_tab",
            "No active tab remained after takeover transition",
        ))
    );

    let with_tab = takeover_continuity_failure(true, &frame_runtime, &readiness, &integration);
    assert_eq!(
        with_tab,
        Some((
            "continuity_frame_unavailable",
            "Frame context became unavailable after takeover transition",
        ))
    );
}

#[test]
fn continuity_failure_degrades_readiness_and_integration_surfaces() {
    let frame_runtime = rub_core::model::FrameRuntimeInfo {
        status: FrameContextStatus::Top,
        current_frame: Some(FrameContextInfo {
            frame_id: "root".to_string(),
            name: None,
            parent_frame_id: None,
            target_id: None,
            url: Some("https://example.com".to_string()),
            depth: 0,
            same_origin_accessible: Some(true),
        }),
        primary_frame: None,
        frame_lineage: vec!["root".to_string()],
        degraded_reason: None,
    };
    let degraded_readiness = ReadinessInfo {
        status: ReadinessStatus::Degraded,
        ..ReadinessInfo::default()
    };
    let degraded_integration = IntegrationRuntimeInfo {
        mode: IntegrationMode::Normal,
        status: IntegrationRuntimeStatus::Degraded,
        degraded_surfaces: vec![IntegrationSurface::RuntimeObservatory],
        ..IntegrationRuntimeInfo::default()
    };
    let degraded_optional_integration = IntegrationRuntimeInfo {
        mode: IntegrationMode::Normal,
        status: IntegrationRuntimeStatus::Degraded,
        degraded_surfaces: vec![IntegrationSurface::StateInspector],
        ..IntegrationRuntimeInfo::default()
    };

    assert_eq!(
        takeover_continuity_failure(
            true,
            &frame_runtime,
            &degraded_readiness,
            &IntegrationRuntimeInfo::default()
        ),
        Some((
            "continuity_readiness_degraded",
            "Readiness surface degraded after takeover transition",
        ))
    );
    assert_eq!(
        takeover_continuity_failure(
            true,
            &frame_runtime,
            &ReadinessInfo::default(),
            &degraded_integration
        ),
        Some((
            "continuity_runtime_degraded",
            "Integration runtime degraded after takeover transition",
        ))
    );
    assert_eq!(
        takeover_continuity_failure(
            true,
            &frame_runtime,
            &ReadinessInfo::default(),
            &degraded_optional_integration
        ),
        None
    );
    assert_eq!(
        takeover_continuity_failure(
            true,
            &frame_runtime,
            &ReadinessInfo::default(),
            &IntegrationRuntimeInfo::default()
        ),
        None
    );
}

#[test]
fn takeover_degraded_authority_error_uses_shared_session_busy_family() {
    let error = takeover_degraded_authority_error(
        "Takeover continuity degraded",
        "continuity_frame_unavailable",
        serde_json::json!({
            "frame_id": "child",
        }),
    )
    .into_envelope();

    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("continuity_frame_unavailable")
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("frame_id"))
            .and_then(|value| value.as_str()),
        Some("child")
    );
}

#[test]
fn takeover_runtime_refresh_unavailable_error_uses_specific_reason() {
    let error = takeover_runtime_refresh_unavailable_error("cdp socket closed").into_envelope();

    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("continuity_runtime_refresh_unavailable")
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("phase"))
            .and_then(|value| value.as_str()),
        Some("runtime_refresh")
    );
}

#[test]
fn takeover_resume_repaused_error_rejects_reactivated_handoff() {
    let error = takeover_resume_repaused_error(
        &TakeoverRuntimeInfo {
            status: TakeoverRuntimeStatus::Active,
            automation_paused: true,
            ..TakeoverRuntimeInfo::default()
        },
        &HumanVerificationHandoffInfo {
            status: HumanVerificationHandoffStatus::Active,
            automation_paused: true,
            ..HumanVerificationHandoffInfo::default()
        },
    )
    .expect("repaused runtime should reject resume");

    assert_eq!(error.into_envelope().code, ErrorCode::AutomationPaused);
}

#[test]
fn cookies_path_payload_accepts_path_state_metadata() {
    let parsed = parse_json_args::<CookiesPathArgs>(
        &serde_json::json!({
            "sub": "export",
            "path": "/tmp/cookies.json",
            "path_state": {
                "path_authority": "cli.cookies.export.path"
            }
        }),
        "cookies export",
    )
    .expect("cookies path payload should accept display-only path metadata");
    assert_eq!(parsed.path, "/tmp/cookies.json");
}
