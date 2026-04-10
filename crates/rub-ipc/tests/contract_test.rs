use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::Timing;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
use tokio::io::BufReader;

#[test]
fn ipc_request_roundtrip_through_json() {
    let req = IpcRequest::new(
        "click",
        serde_json::json!({"index": 3, "snapshot_id": "abc"}),
        30000,
    )
    .with_command_id("cmd-001")
    .expect("static command_id must be valid");

    let json = serde_json::to_string(&req).unwrap();
    let back: IpcRequest = serde_json::from_str(&json).unwrap();

    assert_eq!(back.command, "click");
    assert_eq!(back.command_id.as_deref(), Some("cmd-001"));
    assert_eq!(back.args["index"], 3);
    assert_eq!(back.timeout_ms, 30000);
    assert_eq!(back.ipc_protocol_version, IPC_PROTOCOL_VERSION);
}

#[test]
fn ipc_request_rejects_unknown_fields() {
    let error = serde_json::from_str::<IpcRequest>(
        r#"{
            "ipc_protocol_version":"1.0",
            "command":"doctor",
            "args":{},
            "timeout_ms":1000,
            "unexpected":"field"
        }"#,
    )
    .expect_err("unknown field should be rejected");
    assert!(error.to_string().contains("unknown field"), "{error}");
}

#[test]
fn strict_request_decode_surfaces_structured_contract_reason() {
    let error = IpcRequest::from_value_strict(serde_json::json!({
        "ipc_protocol_version": "1.0",
        "command": "doctor",
        "args": {},
        "timeout_ms": 1000,
        "unexpected": "field"
    }))
    .expect_err("strict request decode should reject unknown fields");
    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("invalid_ipc_request_contract")
    );
}

#[test]
fn ipc_request_rejects_blank_command_id() {
    let error = serde_json::from_str::<IpcRequest>(
        r#"{
            "ipc_protocol_version":"1.0",
            "command":"doctor",
            "command_id":"   ",
            "args":{},
            "timeout_ms":1000
        }"#,
    )
    .expect_err("blank command_id should be rejected");
    assert!(
        error.to_string().contains("non-empty and non-whitespace"),
        "{error}"
    );
}

#[test]
fn ipc_response_success_roundtrip() {
    let resp = IpcResponse::success("req-123", serde_json::json!({"url": "https://example.com"}))
        .with_command_id("cmd-001")
        .expect("static command_id must be valid")
        .with_timing(Timing {
            queue_ms: 2,
            exec_ms: 145,
            total_ms: 147,
        });

    let json = serde_json::to_string(&resp).unwrap();
    let back: IpcResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(back.status, ResponseStatus::Success);
    assert_eq!(back.request_id, "req-123");
    assert_eq!(back.command_id.as_deref(), Some("cmd-001"));
    assert_eq!(back.data.unwrap()["url"], "https://example.com");
    assert!(back.error.is_none());
    assert_eq!(back.timing.total_ms, 147);
}

#[test]
fn ipc_response_error_roundtrip() {
    let envelope = ErrorEnvelope::new(ErrorCode::StaleSnapshot, "Snapshot is stale")
        .with_context(serde_json::json!({"snapshot_epoch": 3, "current_epoch": 5}));
    let resp = IpcResponse::error("req-456", envelope);

    let json = serde_json::to_string(&resp).unwrap();
    let back: IpcResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(back.status, ResponseStatus::Error);
    assert!(back.data.is_none());
    let err = back.error.unwrap();
    assert_eq!(err.code, ErrorCode::StaleSnapshot);
    assert_eq!(err.context.unwrap()["snapshot_epoch"], 3);
}

#[test]
fn ipc_response_rejects_unknown_fields() {
    let error = serde_json::from_str::<IpcResponse>(
        r#"{
            "ipc_protocol_version":"1.0",
            "request_id":"req-1",
            "status":"success",
            "data":{},
            "timing":{"queue_ms":0,"exec_ms":0,"total_ms":0},
            "unexpected":"field"
        }"#,
    )
    .expect_err("unknown field should be rejected");
    assert!(error.to_string().contains("unknown field"), "{error}");
}

#[test]
fn ipc_response_rejects_blank_command_id() {
    let error = serde_json::from_str::<IpcResponse>(
        r#"{
            "ipc_protocol_version":"1.0",
            "command_id":" ",
            "request_id":"req-1",
            "status":"success",
            "data":{},
            "timing":{"queue_ms":0,"exec_ms":0,"total_ms":0}
        }"#,
    )
    .expect_err("blank response command_id should be rejected");
    assert!(
        error.to_string().contains("non-empty and non-whitespace"),
        "{error}"
    );
}

#[test]
fn ipc_response_rejects_success_with_error_envelope() {
    let error = serde_json::from_str::<IpcResponse>(
        r#"{
            "ipc_protocol_version":"1.0",
            "request_id":"req-1",
            "status":"success",
            "data":{},
            "error":{"code":"IPC_PROTOCOL_ERROR","message":"bad","suggestion":"report"},
            "timing":{"queue_ms":0,"exec_ms":0,"total_ms":0}
        }"#,
    )
    .expect_err("invalid success/error combination should be rejected");
    assert!(
        error.to_string().contains("carried an error envelope"),
        "{error}"
    );
}

#[test]
fn ipc_response_rejects_success_without_data() {
    let error = serde_json::from_str::<IpcResponse>(
        r#"{
            "ipc_protocol_version":"1.0",
            "request_id":"req-1",
            "status":"success",
            "timing":{"queue_ms":0,"exec_ms":0,"total_ms":0}
        }"#,
    )
    .expect_err("success response missing data should be rejected");
    assert!(
        error.to_string().contains("omitted success data"),
        "{error}"
    );
}

#[test]
fn ipc_response_rejects_error_without_error_envelope() {
    let error = serde_json::from_str::<IpcResponse>(
        r#"{
            "ipc_protocol_version":"1.0",
            "request_id":"req-1",
            "status":"error",
            "timing":{"queue_ms":0,"exec_ms":0,"total_ms":0}
        }"#,
    )
    .expect_err("error response missing envelope should be rejected");
    assert!(
        error.to_string().contains("omitted the error envelope"),
        "{error}"
    );
}

#[test]
fn ipc_response_rejects_error_with_success_data() {
    let error = serde_json::from_str::<IpcResponse>(
        r#"{
            "ipc_protocol_version":"1.0",
            "request_id":"req-1",
            "status":"error",
            "data":{"ok":true},
            "error":{"code":"IPC_PROTOCOL_ERROR","message":"bad","suggestion":"report"},
            "timing":{"queue_ms":0,"exec_ms":0,"total_ms":0}
        }"#,
    )
    .expect_err("error response carrying data should be rejected");
    assert!(
        error.to_string().contains("carried success data"),
        "{error}"
    );
}

#[test]
fn strict_response_decode_surfaces_schema_reason() {
    let error = IpcResponse::from_value_strict(serde_json::json!({
        "ipc_protocol_version": "1.0",
        "request_id": "req-1",
        "status": "success",
        "data": {},
        "timing": {"queue_ms":0,"exec_ms":0,"total_ms":0},
        "unexpected": "field"
    }))
    .expect_err("strict response decode should reject unknown fields");
    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("invalid_ipc_response_schema")
    );
}

#[test]
fn strict_response_decode_surfaces_contract_reason() {
    let error = IpcResponse::from_value_strict(serde_json::json!({
        "ipc_protocol_version": "1.0",
        "request_id": "req-1",
        "status": "success",
        "data": {},
        "error": {"code":"IPC_PROTOCOL_ERROR","message":"bad","suggestion":"report"},
        "timing": {"queue_ms":0,"exec_ms":0,"total_ms":0}
    }))
    .expect_err("strict response decode should reject invalid success/error combination");
    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("invalid_ipc_response_contract")
    );
}

/// SC-013: Both protocol version fields present in serialized JSON.
#[test]
fn protocol_version_present_in_request() {
    let req = IpcRequest::new("state", serde_json::json!({}), 30000);
    let json = serde_json::to_value(&req).unwrap();
    assert!(json.get("ipc_protocol_version").is_some());
    assert_eq!(json["ipc_protocol_version"], IPC_PROTOCOL_VERSION);
}

#[test]
fn protocol_version_present_in_response() {
    let resp = IpcResponse::success("req-789", serde_json::json!({}));
    let json = serde_json::to_value(&resp).unwrap();
    assert!(json.get("ipc_protocol_version").is_some());
    assert_eq!(json["ipc_protocol_version"], IPC_PROTOCOL_VERSION);
}

#[tokio::test]
async fn ndjson_codec_roundtrip_for_ipc_request() {
    let req = IpcRequest::new(
        "open",
        serde_json::json!({"url": "https://example.com"}),
        30000,
    )
    .with_command_id("cmd-002")
    .expect("static command_id must be valid");

    let encoded = NdJsonCodec::encode(&req).unwrap();
    let mut reader = BufReader::new(encoded.as_slice());
    let decoded: IpcRequest = NdJsonCodec::read(&mut reader).await.unwrap().unwrap();

    assert_eq!(decoded.command, "open");
    assert_eq!(decoded.command_id.as_deref(), Some("cmd-002"));
}

#[tokio::test]
async fn ndjson_codec_roundtrip_for_ipc_response() {
    let resp =
        IpcResponse::success("req-abc", serde_json::json!({"title": "Test"})).with_timing(Timing {
            queue_ms: 0,
            exec_ms: 50,
            total_ms: 50,
        });

    let encoded = NdJsonCodec::encode(&resp).unwrap();
    let mut reader = BufReader::new(encoded.as_slice());
    let decoded: IpcResponse = NdJsonCodec::read(&mut reader).await.unwrap().unwrap();

    assert_eq!(decoded.request_id, "req-abc");
    assert_eq!(decoded.timing.exec_ms, 50);
}
