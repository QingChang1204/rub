use super::IpcClient;
use crate::codec::{MAX_FRAME_BYTES, NdJsonCodec};
use crate::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::Duration;

fn oversized_request() -> IpcRequest {
    IpcRequest::new(
        "doctor",
        serde_json::json!({
            "payload": "a".repeat(MAX_FRAME_BYTES),
        }),
        1_000,
    )
}

#[tokio::test]
async fn client_is_single_use() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "doctor");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::deferred(&socket_path);
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let first = client.send(&request).await.expect("first send");
    assert_eq!(first.status, ResponseStatus::Success);

    let error = client
        .send(&request)
        .await
        .expect_err("second send should fail");
    assert!(
        error.to_string().contains("IpcClient is single-use"),
        "{error}"
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_rejects_mismatched_response_protocol_version() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let mut response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
        response = response
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid");
        response.ipc_protocol_version = "0.9".to_string();
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::deferred(&socket_path);
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("mismatched protocol response should fail");
    assert!(error.to_string().contains("protocol mismatch"));
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_protocol_version_mismatch")
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_rejects_mismatched_response_command_id() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id("other-command")
            .expect("static command_id must be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::deferred(&socket_path);
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
    let error = client
        .send(&request)
        .await
        .expect_err("mismatched command_id response should fail");
    assert!(error.to_string().contains("command_id mismatch"));
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("request_committed")),
        Some(&serde_json::json!(true))
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_rejects_unsolicited_response_command_id() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id("unsolicited-command")
            .expect("static command_id must be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("unexpected command_id response should fail");
    assert!(error.to_string().contains("command_id mismatch"));

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_rejects_missing_response_command_id_for_non_compat_request() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("missing command_id response should fail");
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_missing_command_id")
    );
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("request_committed")),
        Some(&serde_json::json!(true))
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_preserves_protocol_error_when_error_response_echoes_command_id() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let response = IpcResponse::error(
            "req-compat",
            ErrorEnvelope::new(ErrorCode::IpcVersionMismatch, "compat failure").with_context(
                serde_json::json!({
                    "reason": "ipc_request_protocol_mismatch",
                }),
            ),
        )
        .with_command_id(
            request
                .command_id
                .clone()
                .expect("doctor requests should carry command_id"),
        )
        .expect("request command_id should remain protocol-valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
    let response = client
        .send(&request)
        .await
        .expect("daemon-side protocol failure should stay a correlated response frame");
    assert_eq!(response.status, ResponseStatus::Error);
    let envelope = response.error.expect("error envelope");
    assert_eq!(envelope.code, ErrorCode::IpcVersionMismatch);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_request_protocol_mismatch")
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn deferred_bound_client_projects_verified_daemon_session_id() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.daemon_session_id.as_deref(), Some("sess-bound"));
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid")
            .with_daemon_session_id("sess-bound")
            .expect("daemon session id must be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::deferred(&socket_path)
        .bind_daemon_session_id("sess-bound")
        .expect("daemon authority must be valid");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let response = client.send(&request).await.expect("send succeeds");
    assert_eq!(response.status, ResponseStatus::Success);

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn bound_client_rejects_mismatched_response_daemon_authority() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.daemon_session_id.as_deref(), Some("sess-bound"));
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid")
            .with_daemon_session_id("sess-other")
            .expect("daemon session id must be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::deferred(&socket_path)
        .bind_daemon_session_id("sess-bound")
        .expect("daemon authority must be valid");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("mismatched daemon session id in response must fail closed");
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_daemon_session_id_mismatch")
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn bound_client_rejects_missing_response_daemon_authority() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.daemon_session_id.as_deref(), Some("sess-bound"));
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::deferred(&socket_path)
        .bind_daemon_session_id("sess-bound")
        .expect("daemon authority must be valid");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("missing daemon session id in response must fail closed");
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_missing_daemon_session_id")
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn bound_client_rejects_conflicting_explicit_daemon_authority() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "client-side authority mismatch must fail closed before opening a socket"
        );
    });

    let mut client = IpcClient::deferred(&socket_path)
        .bind_daemon_session_id("sess-bound")
        .expect("daemon authority must be valid");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
        .with_daemon_session_id("sess-other")
        .expect("explicit authority must be valid");
    let error = client
        .send(&request)
        .await
        .expect_err("conflicting daemon authority must fail locally");
    assert!(error.to_string().contains("daemon_session_id mismatch"));

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn deferred_client_connect_failure_does_not_consume_single_use_authority() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let mut client = IpcClient::deferred(&socket_path);
    let request = IpcRequest::new("doctor", serde_json::json!({}), 250);

    let error = client
        .send(&request)
        .await
        .expect_err("missing socket should fail to connect");
    let error_text = error.to_string();
    assert!(
        error_text.contains("No such file")
            || error_text.contains("not found")
            || error_text.contains("No such")
    );

    let listener = UnixListener::bind(&socket_path).expect("bind listener");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "doctor");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let response = client
        .send(&request)
        .await
        .expect("client should remain reusable after connect failure");
    assert_eq!(response.status, ResponseStatus::Success);

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn deferred_client_oversized_request_fails_before_spending_socket_authority() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let mut client = IpcClient::deferred(&socket_path);
    let error = client
        .send(&oversized_request())
        .await
        .expect_err("oversized request should fail locally");
    let envelope = error
        .protocol_envelope()
        .expect("oversized request must be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("oversized_ndjson_request")
    );

    let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(
        accepted.is_err(),
        "oversized request must fail before opening the deferred socket"
    );

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "doctor");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let response = client
        .send(&IpcRequest::new("doctor", serde_json::json!({}), 1_000))
        .await
        .expect("oversized local rejection must preserve deferred socket authority");
    assert_eq!(response.status, ResponseStatus::Success);

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn connected_client_oversized_request_does_not_consume_single_use_stream_authority() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "doctor");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor requests should carry command_id"),
            )
            .expect("request command_id should remain protocol-valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let error = client
        .send(&oversized_request())
        .await
        .expect_err("oversized request should fail locally");
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("oversized_ndjson_request")
    );

    let response = client
        .send(&IpcRequest::new("doctor", serde_json::json!({}), 1_000))
        .await
        .expect("local request rejection must preserve connected stream authority");
    assert_eq!(response.status, ResponseStatus::Success);

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_surfaces_structured_response_contract_errors() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let invalid = serde_json::json!({
            "ipc_protocol_version": IPC_PROTOCOL_VERSION,
            "command_id": request.command_id,
            "request_id": "req-1",
            "status": "success",
            "data": { "ok": true },
            "error": serde_json::to_value(
                ErrorEnvelope::new(ErrorCode::IpcProtocolError, "bad"),
            )
            .expect("serialize envelope"),
            "timing": { "queue_ms": 0, "exec_ms": 0, "total_ms": 0 }
        });
        NdJsonCodec::write(&mut writer, &invalid)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("invalid response contract should fail");
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("invalid_ipc_response_contract")
    );
    assert_eq!(
        error
            .protocol_envelope()
            .and_then(|envelope| envelope.context.as_ref())
            .and_then(|ctx| ctx.get("request_committed")),
        Some(&serde_json::json!(true))
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_treats_partial_response_frame_as_committed_request_protocol_failure() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        writer
            .write_all(br#"{"ipc_protocol_version":"1.0","request_id":"req-1""#)
            .await
            .expect("write partial response");
        writer.shutdown().await.expect("shutdown writer");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error = client
        .send(&request)
        .await
        .expect_err("partial response frame should fail");
    let envelope = error
        .protocol_envelope()
        .expect("partial response after request commit must be structured protocol failure");
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    let context = envelope.context.as_ref().expect("context");
    assert_eq!(
        context["reason"],
        serde_json::json!("ipc_response_transport_failure_after_request_commit")
    );
    assert_eq!(
        context["transport_reason"],
        serde_json::json!("partial_ndjson_frame")
    );
    assert_eq!(context["request_committed"], serde_json::json!(true));
    assert_eq!(
        context["command_id"],
        serde_json::json!(
            request
                .command_id
                .as_ref()
                .expect("doctor request should carry command_id")
        )
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_classifies_oversized_response_frame_without_string_matching() {
    let oversized = format!("{{\"payload\":\"{}\"}}\n", "a".repeat(MAX_FRAME_BYTES));
    let mut reader = BufReader::new(oversized.as_bytes());
    let decode_error = NdJsonCodec::read::<serde_json::Value, _>(&mut reader)
        .await
        .expect_err("oversized response frame should fail");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    let error =
        super::IpcClientError::response_read_error(&request, Duration::from_secs(1), decode_error);
    let envelope = error
        .protocol_envelope()
        .expect("oversized response should be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("oversized_ndjson_frame")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("request_committed")),
        Some(&serde_json::json!(true))
    );
}

#[tokio::test]
async fn client_marks_write_transport_failure_as_possible_commit() {
    let (client_stream, server_stream) = UnixStream::pair().expect("create unix stream pair");
    drop(server_stream);
    let mut client = IpcClient {
        stream: Some(client_stream),
        deferred_socket_path: None,
        bound_daemon_session_id: None,
        used: false,
    };
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
        .with_command_id("cmd-write-fail")
        .expect("static command_id must be valid");

    let error = client
        .send(&request)
        .await
        .expect_err("closed peer should fail during request write");
    let envelope = error
        .protocol_envelope()
        .expect("write transport failure after possible commit must be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    let context = envelope.context.as_ref().expect("context");
    assert_eq!(
        context["reason"],
        serde_json::json!("ipc_request_write_transport_failure_after_possible_commit")
    );
    assert_eq!(
        context["request_commit_state"],
        serde_json::json!("possible")
    );
    assert_eq!(context["command_id"], serde_json::json!("cmd-write-fail"));
}

#[tokio::test]
async fn client_treats_post_write_round_trip_timeout_as_protocol_timeout_for_compat_request_without_command_id()
 {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, _writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "_handshake");
        assert_eq!(request.command_id, None);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("_handshake", serde_json::json!({}), 1);
    let error = client
        .send(&request)
        .await
        .expect_err("post-write timeout should fail conservatively");
    let envelope = error
        .protocol_envelope()
        .expect("post-write timeout must be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_timeout_after_request_commit")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("command_id_present")),
        Some(&serde_json::json!(false))
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[test]
fn client_treats_write_timeout_as_protocol_timeout_after_possible_commit_for_compat_request_without_command_id()
 {
    let request = IpcRequest::new("_handshake", serde_json::json!({}), 1);
    let error = super::IpcClientError::possible_request_write_timeout(
        &request,
        Duration::from_millis(1_001),
    );
    let envelope = error
        .protocol_envelope()
        .expect("write timeout must be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_request_write_timeout_after_possible_commit")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("phase"))
            .and_then(|value| value.as_str()),
        Some("ipc_request_write")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("request_commit_state"))
            .and_then(|value| value.as_str()),
        Some("possible")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("command_id_present")),
        Some(&serde_json::json!(false))
    );
}

#[test]
fn client_marks_write_timeout_as_replay_sensitive_when_command_id_exists() {
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1)
        .with_command_id("cmd-write-timeout")
        .expect("static command id should be valid");
    let error = super::IpcClientError::possible_request_write_timeout(
        &request,
        Duration::from_millis(1_001),
    );
    let envelope = error
        .protocol_envelope()
        .expect("write timeout must be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_request_write_timeout_after_possible_commit")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("command_id"))
            .and_then(|value| value.as_str()),
        Some("cmd-write-timeout")
    );
    assert_eq!(
        envelope.suggestion.as_str(),
        "Retry only through the same command_id or replay-recovery lane; do not send a fresh command."
    );
}

#[tokio::test]
async fn client_marks_post_write_timeout_as_replay_sensitive_when_command_id_exists() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, _writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        assert_eq!(request.command_id.as_deref(), Some("cmd-timeout"));
        tokio::time::sleep(Duration::from_millis(1_100)).await;
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1)
        .with_command_id("cmd-timeout")
        .expect("static command id should be valid");
    let error = client
        .send(&request)
        .await
        .expect_err("post-write timeout should remain replay-sensitive");
    let envelope = error
        .protocol_envelope()
        .expect("post-write timeout must be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("command_id"))
            .and_then(|value| value.as_str()),
        Some("cmd-timeout")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("command_id_present")),
        Some(&serde_json::json!(true))
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_rejects_invalid_non_compat_request_contract_before_connect() {
    let socket_dir = std::env::temp_dir().join(format!("rubipc-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).expect("create socket dir");
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind listener");

    let server = tokio::spawn(async move {
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "invalid non-compat request must fail locally before opening a socket"
        );
    });

    let mut client = IpcClient::deferred(&socket_path);
    let mut request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
    request.command_id = None;

    let error = client
        .send(&request)
        .await
        .expect_err("invalid request contract must fail before encode/write");
    let envelope = error
        .protocol_envelope()
        .expect("invalid request contract should be protocol-scoped");
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("invalid_ipc_request_contract")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("field"))
            .and_then(|value| value.as_str()),
        Some("command_id")
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}
