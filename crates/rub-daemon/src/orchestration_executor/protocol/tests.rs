use super::{
    RemoteDispatchContract, RemoteDispatchFailureInfo, align_orchestration_timeout_authority,
    bind_live_orchestration_phase_command_id, bind_stable_orchestration_phase_command_id,
    dispatch_remote_orchestration_request, orchestration_timeout_budget_exhausted_error,
    project_orchestration_request_onto_deadline,
};
use crate::orchestration_probe::OrchestrationProbeResult;
use crate::orchestration_probe::dispatch_remote_orchestration_probe;
use crate::orchestration_runtime::projected_orchestration_session;
use rub_core::error::ErrorCode;
use rub_core::error::ErrorEnvelope;
use rub_core::model::OrchestrationSessionAvailability;
use rub_core::model::TriggerConditionSpec;
use rub_ipc::client::IpcClientError;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IpcRequest, IpcResponse, ResponseStatus};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn queue_test_connection(socket_path: &std::path::Path) -> UnixStream {
    let (client, server) = UnixStream::pair().expect("create in-memory unix stream pair");
    super::queue_remote_orchestration_connection_for_test(socket_path, client);
    server
}

#[tokio::test]
async fn remote_dispatch_replays_partial_response_frame_for_replayable_requests() {
    let socket_path =
        std::path::PathBuf::from(format!("/tmp/rub-orch-{}.sock", uuid::Uuid::now_v7()));
    let first_stream = queue_test_connection(&socket_path);
    let replay_stream = queue_test_connection(&socket_path);

    let server = tokio::spawn(async move {
        let stream = first_stream;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.daemon_session_id.as_deref(), Some("daemon-b"));
        assert_eq!(request.command_id.as_deref(), Some("cmd-1"));
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
        assert_eq!(
            replay_request.daemon_session_id.as_deref(),
            Some("daemon-b")
        );
        assert_eq!(replay_request.command_id.as_deref(), Some("cmd-1"));
        let response = IpcResponse::success("req-2", serde_json::json!({ "ok": true }))
            .with_daemon_session_id("daemon-b")
            .expect("daemon_session_id must be valid")
            .with_command_id("cmd-1")
            .expect("static command id should be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write replay response");
    });

    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        socket_path.display().to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::Addressable,
        None,
    );
    let request = IpcRequest::new("tabs", serde_json::json!({}), 1_000)
        .with_command_id("cmd-1")
        .expect("static command id should be valid");

    let response = dispatch_remote_orchestration_request(
        &session,
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
    .expect("partial response should recover through replay");

    assert_eq!(response.status, ResponseStatus::Success);
    assert_eq!(response.command_id.as_deref(), Some("cmd-1"));

    server.await.expect("server join");
}

#[test]
fn stable_orchestration_phase_command_id_is_semantically_stable() {
    let request_a = bind_stable_orchestration_phase_command_id(
        IpcRequest::new(
            "_orchestration_target_dispatch",
            serde_json::json!({
                "target": { "tab_target_id": "tab-1" },
                "metadata": {
                    "phase": "frozen_dispatch",
                    "attempt": 1,
                },
            }),
            1_000,
        ),
        "orchestration_frozen_target_dispatch",
    )
    .expect("stable command id binding should succeed");
    let request_b = bind_stable_orchestration_phase_command_id(
        IpcRequest::new(
            "_orchestration_target_dispatch",
            serde_json::json!({
                "target": { "tab_target_id": "tab-1" },
                "metadata": {
                    "attempt": 1,
                    "phase": "frozen_dispatch",
                },
            }),
            2_000,
        ),
        "orchestration_frozen_target_dispatch",
    )
    .expect("stable command id binding should succeed");

    assert_eq!(request_a.command_id, request_b.command_id);
}

#[test]
fn live_orchestration_phase_command_id_is_request_scoped_for_identical_payloads() {
    let request_a = bind_live_orchestration_phase_command_id(
        IpcRequest::new("tabs", serde_json::json!({}), 1_000),
        "orchestration_source_tab_inventory",
    )
    .expect("live command id binding should succeed");
    let request_b = bind_live_orchestration_phase_command_id(
        IpcRequest::new("tabs", serde_json::json!({}), 1_000),
        "orchestration_source_tab_inventory",
    )
    .expect("live command id binding should succeed");

    assert_ne!(request_a.command_id, request_b.command_id);
    assert!(
        request_a
            .command_id
            .as_deref()
            .is_some_and(|command_id| command_id.starts_with("orchestration_source_tab_inventory:"))
    );
    assert!(
        request_b
            .command_id
            .as_deref()
            .is_some_and(|command_id| command_id.starts_with("orchestration_source_tab_inventory:"))
    );
}

#[tokio::test]
async fn source_probe_replays_partial_response_frame_with_request_scoped_command_id() {
    let socket_path =
        std::path::PathBuf::from(format!("/tmp/rub-orch-probe-{}.sock", uuid::Uuid::now_v7()));
    let first_stream = queue_test_connection(&socket_path);
    let replay_stream = queue_test_connection(&socket_path);

    let server = tokio::spawn(async move {
        let stream = first_stream;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let first_command_id = request
            .command_id
            .clone()
            .expect("probe request should keep a replayable command_id");
        assert_eq!(request.daemon_session_id.as_deref(), Some("daemon-b"));
        writer
            .write_all(br#"{"ipc_protocol_version":"1.1","request_id":"req-1""#)
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
        assert_eq!(
            replay_request.command_id.as_deref(),
            Some(first_command_id.as_str())
        );
        let response = IpcResponse::success(
            "req-2",
            serde_json::to_value(OrchestrationProbeResult {
                matched: true,
                evidence: None,
                next_network_cursor: 7,
                observed_drop_count: 0,
                degraded_reason: None,
            })
            .expect("probe result should serialize"),
        )
        .with_daemon_session_id("daemon-b")
        .expect("daemon_session_id must be valid")
        .with_command_id(first_command_id)
        .expect("stable command id should be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write replay response");
    });

    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        socket_path.display().to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::Addressable,
        None,
    );

    let result = dispatch_remote_orchestration_probe(
        &session,
        "tab-target",
        Some("frame-main"),
        &TriggerConditionSpec {
            kind: rub_core::model::TriggerConditionKind::Readiness,
            locator: None,
            text: None,
            url_pattern: None,
            readiness_state: Some("interactive".to_string()),
            method: None,
            status_code: None,
            storage_area: None,
            key: None,
            value: None,
        },
        0,
        0,
        None,
    )
    .await
    .expect("source probe should recover through replay");

    assert!(result.matched);
    assert_eq!(result.next_network_cursor, 7);

    server.await.expect("server join");
}

#[tokio::test]
async fn source_probe_uses_fresh_command_id_across_distinct_live_reads() {
    let socket_path = std::path::PathBuf::from(format!(
        "/tmp/rub-orch-probe-fresh-{}.sock",
        uuid::Uuid::now_v7()
    ));
    let first_stream = queue_test_connection(&socket_path);
    let second_stream = queue_test_connection(&socket_path);

    let server = tokio::spawn(async move {
        let mut command_ids = Vec::new();
        for stream in [first_stream, second_stream] {
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            command_ids.push(
                request
                    .command_id
                    .clone()
                    .expect("probe request should keep a replayable command_id"),
            );
            let response = IpcResponse::success(
                "req",
                serde_json::to_value(OrchestrationProbeResult {
                    matched: false,
                    evidence: None,
                    next_network_cursor: 0,
                    observed_drop_count: 0,
                    degraded_reason: None,
                })
                .expect("probe result should serialize"),
            )
            .with_daemon_session_id("daemon-b")
            .expect("daemon_session_id must be valid")
            .with_command_id(command_ids.last().unwrap().clone())
            .expect("request-scoped command id should be valid");
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write response");
        }
        command_ids
    });

    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        socket_path.display().to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::Addressable,
        None,
    );
    let condition = TriggerConditionSpec {
        kind: rub_core::model::TriggerConditionKind::TextPresent,
        locator: None,
        text: Some("Ready".to_string()),
        url_pattern: None,
        readiness_state: None,
        method: None,
        status_code: None,
        storage_area: None,
        key: None,
        value: None,
    };

    let first =
        dispatch_remote_orchestration_probe(&session, "tab-target", None, &condition, 0, 0, None)
            .await
            .expect("first live probe should succeed");
    assert!(!first.matched);

    let second =
        dispatch_remote_orchestration_probe(&session, "tab-target", None, &condition, 0, 0, None)
            .await
            .expect("second live probe should succeed");
    assert!(!second.matched);

    let command_ids = server.await.expect("server join");
    assert_eq!(command_ids.len(), 2);
    assert_ne!(command_ids[0], command_ids[1]);
}

#[test]
fn remote_dispatch_replay_classifies_post_commit_timeout_protocol_failures() {
    let error = IpcClientError::Protocol(
        ErrorEnvelope::new(
            ErrorCode::IpcTimeout,
            "response timed out after request commit",
        )
        .with_context(serde_json::json!({
            "reason": "ipc_response_timeout_after_request_commit",
        })),
    );
    assert_eq!(
        super::orchestration_recoverable_transport_reason(&error),
        Some("response_timeout_after_request_commit")
    );
}

#[test]
fn remote_dispatch_replay_classifies_post_commit_response_transport_protocol_failures() {
    let error = IpcClientError::Protocol(
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            "response transport failed after request commit",
        )
        .with_context(serde_json::json!({
            "reason": "ipc_response_transport_failure_after_request_commit",
        })),
    );
    assert_eq!(
        super::orchestration_recoverable_transport_reason(&error),
        Some("response_transport_failure_after_request_commit")
    );
}

#[tokio::test]
async fn remote_dispatch_fails_closed_after_partial_response_for_compat_request_without_command_id()
{
    let socket_path =
        std::path::PathBuf::from(format!("/tmp/rub-orch-{}.sock", uuid::Uuid::now_v7()));
    let first_stream = queue_test_connection(&socket_path);

    let server = tokio::spawn(async move {
        let stream = first_stream;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.daemon_session_id.as_deref(), Some("daemon-b"));
        assert_eq!(request.command_id, None);
        writer
            .write_all(br#"{"ipc_protocol_version":"1.0","request_id":"req-1""#)
            .await
            .expect("write partial response");
        writer.shutdown().await.expect("shutdown partial writer");

        tokio::time::sleep(Duration::from_millis(20)).await;
    });

    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        socket_path.display().to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::Addressable,
        None,
    );
    let request = IpcRequest::new("_handshake", serde_json::json!({}), 1_000);

    let error = dispatch_remote_orchestration_request(
        &session,
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
    .expect_err("partial compatibility response without command_id must fail closed");

    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason")),
        Some(&serde_json::json!(
            "orchestration_target_dispatch_protocol_failed"
        ))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("ipc_protocol_error"))
            .and_then(|context| context.get("context"))
            .and_then(|context| context.get("reason")),
        Some(&serde_json::json!(
            "ipc_response_transport_failure_after_request_commit"
        ))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("retry_reason")),
        None
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("replay_phase")),
        None
    );

    server.await.expect("server join");
}

#[tokio::test]
async fn remote_error_response_namespaces_reason_and_adds_session_path_context() {
    let socket_path =
        std::path::PathBuf::from(format!("/tmp/rub-orch-error-{}.sock", uuid::Uuid::now_v7()));
    let request_stream = queue_test_connection(&socket_path);

    let server = tokio::spawn(async move {
        let stream = request_stream;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let response = IpcResponse::error(
            "req-1",
            ErrorEnvelope::new(ErrorCode::SessionBusy, "remote daemon refused the request")
                .with_context(serde_json::json!({
                    "reason": "remote_rule_blocked",
                    "remote_detail": "still_cooling_down",
                    "retry_reason": "remote_retry_shadow",
                    "socket_path": "remote-socket-shadow",
                })),
        )
        .with_daemon_session_id("daemon-b")
        .expect("daemon_session_id must be valid")
        .with_command_id(
            request
                .command_id
                .clone()
                .expect("request should carry a command_id"),
        )
        .expect("command_id should remain valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write error response");
    });

    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        socket_path.display().to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::Addressable,
        Some("/tmp/rub-profile".to_string()),
    );
    let request = IpcRequest::new("tabs", serde_json::json!({}), 1_000)
        .with_command_id("cmd-1")
        .expect("static command id should be valid");

    let error = dispatch_remote_orchestration_request(
        &session,
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
    .expect_err("remote error response should surface as envelope");

    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason")),
        Some(&serde_json::json!("orchestration_remote_error_response"))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("remote_reason")),
        Some(&serde_json::json!("remote_rule_blocked"))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("local_dispatch_reason")),
        Some(&serde_json::json!(
            "orchestration_target_dispatch_protocol_failed"
        ))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("session_id")),
        Some(&serde_json::json!("daemon-b"))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("socket_path")),
        Some(&serde_json::json!(socket_path.display().to_string()))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("user_data_dir")),
        Some(&serde_json::json!("/tmp/rub-profile"))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("remote_context"))
            .and_then(|context| context.get("remote_detail")),
        Some(&serde_json::json!("still_cooling_down"))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("socket_path")),
        Some(&serde_json::json!(socket_path.display().to_string()))
    );
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("remote_context"))
            .and_then(|context| context.get("socket_path")),
        Some(&serde_json::json!("remote-socket-shadow"))
    );

    server.await.expect("server join");
}

#[test]
fn project_remote_request_onto_deadline_shrinks_nested_wrapper_timeout_authority() {
    let inner = IpcRequest::new("wait", serde_json::json!({ "timeout_ms": 5_000 }), 5_000)
        .with_command_id("cmd-inner")
        .expect("static command id should be valid");
    let wrapper = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({
            "target": {
                "session_id": "daemon-b",
                "session_name": "remote",
                "tab_target_id": "tab-target",
                "frame_id": "frame-main",
            },
            "request": inner,
        }),
        5_000,
    )
    .with_command_id("cmd-wrapper")
    .expect("static command id should be valid");

    let projected = project_orchestration_request_onto_deadline(
        &wrapper,
        Instant::now() + Duration::from_millis(200),
    )
    .expect("timeout projection should succeed")
    .expect("remaining budget should still be available");

    assert!(projected.timeout_ms <= 200);
    let nested_timeout = projected
        .args
        .get("request")
        .and_then(|value| value.get("timeout_ms"))
        .and_then(|value| value.as_u64())
        .expect("nested request timeout should remain present");
    assert_eq!(nested_timeout, projected.timeout_ms);
    let nested_wait_timeout = projected
        .args
        .get("request")
        .and_then(|value| value.get("args"))
        .and_then(|value| value.get("timeout_ms"))
        .and_then(|value| value.as_u64())
        .expect("nested wait timeout should remain present");
    assert_eq!(
        nested_wait_timeout,
        projected
            .timeout_ms
            .saturating_sub(super::IPC_REPLAY_TIMEOUT_BUFFER_MS)
    );
}

#[test]
fn align_orchestration_timeout_authority_shrinks_nested_wait_budget() {
    let mut request = IpcRequest::new("wait", serde_json::json!({ "timeout_ms": 7_000 }), 500);
    align_orchestration_timeout_authority(&mut request)
        .expect("wait timeout alignment should succeed");
    assert_eq!(request.timeout_ms, 500);
    assert_eq!(
        request
            .args
            .get("timeout_ms")
            .and_then(|value| value.as_u64()),
        Some(500u64.saturating_sub(super::IPC_REPLAY_TIMEOUT_BUFFER_MS))
    );
}

#[test]
fn align_orchestration_timeout_authority_shrinks_inspect_list_wait_budget() {
    let mut request = IpcRequest::new(
        "inspect",
        serde_json::json!({
            "sub": "list",
            "wait_field": "status",
            "wait_contains": "ready",
            "wait_timeout_ms": 7_000,
        }),
        500,
    );
    align_orchestration_timeout_authority(&mut request)
        .expect("inspect list wait timeout alignment should succeed");
    assert_eq!(request.timeout_ms, 500);
    assert_eq!(
        request
            .args
            .get("wait_timeout_ms")
            .and_then(|value| value.as_u64()),
        Some(500u64.saturating_sub(super::IPC_REPLAY_TIMEOUT_BUFFER_MS))
    );
}

#[test]
fn project_remote_request_onto_deadline_shrinks_nested_inspect_list_wait_budget() {
    let inner = IpcRequest::new(
        "inspect",
        serde_json::json!({
            "sub": "list",
            "wait_field": "status",
            "wait_contains": "ready",
            "wait_timeout_ms": 5_000,
        }),
        5_000,
    )
    .with_command_id("cmd-inner")
    .expect("static command id should be valid");
    let wrapper = IpcRequest::new(
        "_orchestration_target_dispatch",
        serde_json::json!({
            "target": {
                "session_id": "daemon-b",
                "session_name": "remote",
                "tab_target_id": "tab-target",
                "frame_id": "frame-main",
            },
            "request": inner,
        }),
        5_000,
    )
    .with_command_id("cmd-wrapper")
    .expect("static command id should be valid");

    let projected = project_orchestration_request_onto_deadline(
        &wrapper,
        Instant::now() + Duration::from_millis(200),
    )
    .expect("timeout projection should succeed")
    .expect("remaining budget should still be available");

    assert!(projected.timeout_ms <= 200);
    let nested_timeout = projected
        .args
        .get("request")
        .and_then(|value| value.get("timeout_ms"))
        .and_then(|value| value.as_u64())
        .expect("nested request timeout should remain present");
    assert_eq!(nested_timeout, projected.timeout_ms);
    let nested_wait_timeout = projected
        .args
        .get("request")
        .and_then(|value| value.get("args"))
        .and_then(|value| value.get("wait_timeout_ms"))
        .and_then(|value| value.as_u64())
        .expect("nested inspect list wait timeout should remain present");
    assert_eq!(
        nested_wait_timeout,
        projected
            .timeout_ms
            .saturating_sub(super::IPC_REPLAY_TIMEOUT_BUFFER_MS)
    );
}

#[test]
fn timeout_budget_exhausted_error_retains_original_transport_details() {
    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        "/tmp/rub-orch.sock".to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::Addressable,
        None,
    );
    let failure = RemoteDispatchFailureInfo {
        session: &session,
        role: "target",
        command: "tabs",
        command_id: Some("target-cmd-1"),
        daemon_session_id: Some("daemon-b"),
        dispatch_subject: "request",
        transport_failure_reason: "orchestration_target_dispatch_transport_failed",
        protocol_failure_reason: "orchestration_target_dispatch_protocol_failed",
    };
    let io_error = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "socket write failed");

    let envelope = orchestration_timeout_budget_exhausted_error(
        failure,
        1_000,
        Some("broken_pipe"),
        Some("replay_send"),
        Some(&io_error),
    );

    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("transport_reason")),
        Some(&serde_json::json!("broken_pipe"))
    );
    assert!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("transport_error"))
            .and_then(|value| value.as_str())
            .is_some_and(|message| message.contains("socket write failed"))
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("replay_phase")),
        Some(&serde_json::json!("replay_send"))
    );
    let context = envelope.context.as_ref().expect("timeout context");
    assert_eq!(
        context.get("command_id"),
        Some(&serde_json::json!("target-cmd-1"))
    );
    assert_eq!(
        context
            .get("possible_commit_recovery_contract")
            .and_then(|contract| contract.get("target_command_id")),
        Some(&serde_json::json!("target-cmd-1"))
    );
}
