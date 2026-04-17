use super::{
    RemoteDispatchContract, RemoteDispatchFailureInfo, align_orchestration_timeout_authority,
    dispatch_remote_orchestration_request, orchestration_timeout_budget_exhausted_error,
    project_orchestration_request_onto_deadline,
};
use crate::orchestration_runtime::projected_orchestration_session;
use rub_core::error::ErrorCode;
use rub_core::error::ErrorEnvelope;
use rub_ipc::client::IpcClientError;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IpcRequest, IpcResponse, ResponseStatus};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

#[tokio::test]
async fn remote_dispatch_replays_partial_response_frame_for_replayable_requests() {
    let socket_path =
        std::path::PathBuf::from(format!("/tmp/rub-orch-{}.sock", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept first");
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

        let (stream, _) = listener.accept().await.expect("accept replay");
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
    let _ = std::fs::remove_file(&socket_path);
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

#[tokio::test]
async fn remote_dispatch_fails_closed_after_partial_response_without_command_id() {
    let socket_path =
        std::path::PathBuf::from(format!("/tmp/rub-orch-{}.sock", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept first");
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

        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "non-replayable orchestration dispatch must not reconnect for replay"
        );
    });

    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        socket_path.display().to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        None,
    );
    let request = IpcRequest::new("tabs", serde_json::json!({}), 1_000);

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
    .expect_err("partial response without command_id must fail closed");

    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason")),
        Some(&serde_json::json!(
            "orchestration_target_dispatch_transport_failed"
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
    let _ = std::fs::remove_file(&socket_path);
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
fn timeout_budget_exhausted_error_retains_original_transport_details() {
    let session = projected_orchestration_session(
        "daemon-b".to_string(),
        "remote".to_string(),
        42,
        "/tmp/rub-orch.sock".to_string(),
        false,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        None,
    );
    let failure = RemoteDispatchFailureInfo {
        session: &session,
        role: "target",
        command: "tabs",
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
}
