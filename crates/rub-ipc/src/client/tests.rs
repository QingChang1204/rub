use super::IpcClient;
use crate::codec::{MAX_FRAME_BYTES, NdJsonCodec};
use crate::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
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
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
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
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let mut response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
        response.ipc_protocol_version = "0.9".to_string();
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
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

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
    let error = client
        .send(&request)
        .await
        .expect_err("mismatched command_id response should fail");
    assert!(error.to_string().contains("command_id mismatch"));

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
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
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
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
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
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
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
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));
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
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let invalid = serde_json::json!({
            "ipc_protocol_version": IPC_PROTOCOL_VERSION,
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

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn client_treats_partial_response_frame_as_transport_error() {
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
    match error {
        super::IpcClientError::Transport(io_error) => {
            assert_eq!(io_error.kind(), std::io::ErrorKind::UnexpectedEof);
        }
        super::IpcClientError::Protocol(envelope) => {
            panic!("partial response must remain transport-scoped, got {envelope:?}");
        }
    }

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
    let error = super::IpcClientError::response_read_error(decode_error);
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
}

#[tokio::test]
async fn client_treats_post_write_round_trip_timeout_as_protocol_timeout_without_command_id() {
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
        assert_eq!(request.command, "doctor");
        tokio::time::sleep(Duration::from_millis(1_100)).await;
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let request = IpcRequest::new("doctor", serde_json::json!({}), 1);
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
