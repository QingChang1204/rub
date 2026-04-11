use std::path::Path;
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::OrchestrationSessionInfo;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
use serde::de::DeserializeOwned;

use crate::orchestration_runtime::extend_orchestration_session_path_context;

const IPC_REPLAY_TIMEOUT_BUFFER_MS: u64 = 1_000;

#[derive(Clone, Copy)]
pub(crate) struct RemoteDispatchContract {
    pub(crate) dispatch_subject: &'static str,
    pub(crate) unreachable_reason: &'static str,
    pub(crate) transport_failure_reason: &'static str,
    pub(crate) protocol_failure_reason: &'static str,
    pub(crate) missing_error_message: &'static str,
}

#[derive(Clone, Copy)]
struct RemoteDispatchFailureInfo<'a> {
    session: &'a OrchestrationSessionInfo,
    role: &'static str,
    command: &'a str,
    dispatch_subject: &'static str,
    transport_failure_reason: &'static str,
    protocol_failure_reason: &'static str,
}

pub(crate) fn ensure_orchestration_session_protocol(
    session: &OrchestrationSessionInfo,
    role: &str,
) -> Result<(), ErrorEnvelope> {
    if session.ipc_protocol_version == IPC_PROTOCOL_VERSION {
        return Ok(());
    }

    Err(ErrorEnvelope::new(
        ErrorCode::IpcVersionMismatch,
        format!(
            "Orchestration {role} session '{}' uses IPC protocol {}, expected {}",
            session.session_name, session.ipc_protocol_version, IPC_PROTOCOL_VERSION
        ),
    )
    .with_context(serde_json::json!({
        "reason": format!("orchestration_{role}_protocol_mismatch"),
        "session_id": session.session_id,
        "session_name": session.session_name,
        "session_protocol_version": session.ipc_protocol_version,
        "expected_protocol_version": IPC_PROTOCOL_VERSION,
    })))
}

pub(crate) fn bind_orchestration_daemon_authority(
    request: IpcRequest,
    session: &OrchestrationSessionInfo,
    role: &str,
) -> Result<IpcRequest, ErrorEnvelope> {
    let command = request.command.clone();
    request
        .with_daemon_session_id(session.session_id.clone())
        .map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                format!(
                    "Failed to bind orchestration {role} request to daemon authority '{}': {error}",
                    session.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": format!("orchestration_{role}_daemon_authority_bind_failed"),
                "session_id": session.session_id,
                "session_name": session.session_name,
                "command": command,
            }))
        })
}

pub(crate) fn ensure_orchestration_success_response(
    response: IpcResponse,
    missing_error_message: &'static str,
) -> Result<IpcResponse, ErrorEnvelope> {
    match response.status {
        ResponseStatus::Success => Ok(response),
        ResponseStatus::Error => Err(response.error.unwrap_or_else(|| {
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, missing_error_message)
        })),
    }
}

pub(crate) async fn dispatch_remote_orchestration_request(
    session: &OrchestrationSessionInfo,
    role: &'static str,
    request: IpcRequest,
    contract: RemoteDispatchContract,
) -> Result<IpcResponse, ErrorEnvelope> {
    // Replay is only safe once the caller has bound this transport request to a
    // stable command_id. The remote daemon owns the commit fence and can return
    // a cached committed response for that same request identity. Without a
    // wrapper command_id we fail closed after transport loss instead of
    // attempting a best-effort resend.
    ensure_orchestration_session_protocol(session, role)?;
    let mut client = IpcClient::connect(Path::new(&session.socket_path))
        .await
        .map_err(|error| {
            let mut context = serde_json::json!({
                "reason": contract.unreachable_reason,
                "command": request.command,
                "session_id": session.session_id,
                "session_name": session.session_name,
            });
            extend_orchestration_session_path_context(&mut context, session);
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Unable to reach orchestration {role} session '{}' at {} while dispatching {}: {error}",
                    session.session_name, session.socket_path, contract.dispatch_subject
                ),
            )
            .with_context(context)
        })?;
    let request = bind_orchestration_daemon_authority(request, session, role)?;
    let original_timeout_ms = request.timeout_ms;
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.max(1));
    let command = request.command.clone();
    let failure = RemoteDispatchFailureInfo {
        session,
        role,
        command: &command,
        dispatch_subject: contract.dispatch_subject,
        transport_failure_reason: contract.transport_failure_reason,
        protocol_failure_reason: contract.protocol_failure_reason,
    };
    let request = project_orchestration_request_onto_deadline(&request, deadline)
        .map_err(|reason| {
            orchestration_timeout_projection_error(failure, &reason, None, Some("initial_send"))
        })?
        .ok_or_else(|| {
            orchestration_timeout_budget_exhausted_error(
                failure,
                original_timeout_ms,
                None,
                Some("initial_send"),
                None,
            )
        })?;
    let response = match client.send(&request).await {
        Ok(response) => response,
        Err(error) => {
            let retry_reason = orchestration_recoverable_transport_reason(&error)
                .filter(|_| request.command_id.is_some());
            if let Some(retry_reason) = retry_reason {
                let mut replay_client = IpcClient::connect(Path::new(&session.socket_path))
                    .await
                    .map_err(|reconnect_error| {
                    orchestration_transport_dispatch_error(
                        failure,
                        &reconnect_error,
                        Some(retry_reason),
                        Some("replay_reconnect"),
                    )
                })?;
                let replay_request =
                    project_orchestration_request_onto_deadline(&request, deadline)
                        .map_err(|reason| {
                            orchestration_timeout_projection_error(
                                failure,
                                &reason,
                                Some(retry_reason),
                                Some("replay_send"),
                            )
                        })?
                        .ok_or_else(|| {
                            orchestration_timeout_budget_exhausted_error(
                                failure,
                                original_timeout_ms,
                                Some(retry_reason),
                                Some("replay_send"),
                                orchestration_transport_error(&error),
                            )
                        })?;
                match replay_client.send(&replay_request).await {
                    Ok(response) => response,
                    Err(error) => {
                        return Err(orchestration_dispatch_error(
                            failure,
                            &error,
                            Some(retry_reason),
                            Some("replay_send"),
                        ));
                    }
                }
            } else {
                return Err(orchestration_dispatch_error(failure, &error, None, None));
            }
        }
    };
    ensure_orchestration_success_response(response, contract.missing_error_message)
}

fn project_orchestration_request_onto_deadline(
    request: &IpcRequest,
    deadline: Instant,
) -> Result<Option<IpcRequest>, String> {
    let remaining_timeout_ms = remaining_budget_ms(deadline);
    if remaining_timeout_ms == 0 {
        return Ok(None);
    }
    let mut projected = request.clone();
    projected.timeout_ms = projected.timeout_ms.min(remaining_timeout_ms);
    align_orchestration_timeout_authority(&mut projected)?;
    Ok(Some(projected))
}

fn remaining_budget_ms(deadline: Instant) -> u64 {
    deadline
        .checked_duration_since(Instant::now())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn align_orchestration_timeout_authority(
    request: &mut IpcRequest,
) -> Result<(), String> {
    align_embedded_timeout_authority(request);
    if request.command != "_orchestration_target_dispatch" {
        return Ok(());
    }
    let Some(args) = request.args.as_object_mut() else {
        return Ok(());
    };
    let Some(inner_request_value) = args.get_mut("request") else {
        return Ok(());
    };
    let Ok(mut inner_request) = serde_json::from_value::<IpcRequest>(inner_request_value.clone())
    else {
        return Ok(());
    };
    inner_request.timeout_ms = inner_request.timeout_ms.min(request.timeout_ms);
    align_orchestration_timeout_authority(&mut inner_request)?;
    *inner_request_value = serde_json::to_value(inner_request).map_err(|error| {
        format!(
            "embedded orchestration request could not be re-serialized after timeout projection: {error}"
        )
    })?;
    Ok(())
}

fn align_embedded_timeout_authority(request: &mut IpcRequest) {
    let embedded_timeout_ms = match request.command.as_str() {
        "wait" => Some(
            request
                .timeout_ms
                .saturating_sub(IPC_REPLAY_TIMEOUT_BUFFER_MS),
        ),
        "inspect"
            if request
                .args
                .get("sub")
                .and_then(|value| value.as_str())
                .is_some_and(|sub| sub == "network")
                && request
                    .args
                    .get("wait")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false) =>
        {
            Some(
                request
                    .timeout_ms
                    .saturating_sub(IPC_REPLAY_TIMEOUT_BUFFER_MS),
            )
        }
        "download"
            if request
                .args
                .get("sub")
                .and_then(|value| value.as_str())
                .is_some_and(|sub| sub == "wait" || sub == "save") =>
        {
            Some(
                request
                    .timeout_ms
                    .saturating_sub(IPC_REPLAY_TIMEOUT_BUFFER_MS),
            )
        }
        _ => None,
    };

    if let Some(timeout_ms) = embedded_timeout_ms
        && let Some(object) = request.args.as_object_mut()
        && object.contains_key("timeout_ms")
    {
        object.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
    }
}

fn orchestration_timeout_budget_exhausted_error(
    failure: RemoteDispatchFailureInfo<'_>,
    original_timeout_ms: u64,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
    transport_error: Option<&std::io::Error>,
) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!(
            "Orchestration {} to {} session '{}' exhausted the declared timeout budget of {}ms",
            failure.dispatch_subject,
            failure.role,
            failure.session.session_name,
            original_timeout_ms
        ),
    )
    .with_context({
        let mut context = orchestration_dispatch_context(
            failure,
            "orchestration_dispatch_timeout_budget_exhausted",
            replay_retry_reason,
            replay_phase,
        );
        if let Some(context_object) = context.as_object_mut() {
            context_object.insert(
                "original_timeout_ms".to_string(),
                serde_json::json!(original_timeout_ms),
            );
            if let Some(transport_error) = transport_error {
                insert_transport_error_context(context_object, transport_error);
            }
        }
        context
    })
}

fn orchestration_timeout_projection_error(
    failure: RemoteDispatchFailureInfo<'_>,
    projection_reason: &str,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
) -> ErrorEnvelope {
    let mut context = orchestration_dispatch_context(
        failure,
        "orchestration_timeout_authority_projection_failed",
        replay_retry_reason,
        replay_phase,
    );
    if let Some(context_object) = context.as_object_mut() {
        context_object.insert(
            "projection_reason".to_string(),
            serde_json::json!(projection_reason),
        );
    }
    ErrorEnvelope::new(
        ErrorCode::IpcProtocolError,
        format!(
            "Failed to align orchestration {} timeout authority for {} session '{}': {projection_reason}",
            failure.dispatch_subject, failure.role, failure.session.session_name
        ),
    )
    .with_context(context)
}

fn orchestration_recoverable_transport_reason(error: &IpcClientError) -> Option<&'static str> {
    match error {
        IpcClientError::Transport(io_error) => classify_orchestration_transport(io_error),
        IpcClientError::Protocol(_) => None,
    }
}

fn orchestration_transport_error(error: &IpcClientError) -> Option<&std::io::Error> {
    match error {
        IpcClientError::Transport(io_error) => Some(io_error),
        IpcClientError::Protocol(_) => None,
    }
}

fn classify_orchestration_transport(error: &std::io::Error) -> Option<&'static str> {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => Some("connection_refused"),
        std::io::ErrorKind::ConnectionReset => Some("connection_reset"),
        std::io::ErrorKind::ConnectionAborted => Some("connection_aborted"),
        std::io::ErrorKind::TimedOut => Some("timed_out"),
        std::io::ErrorKind::Interrupted => Some("interrupted"),
        std::io::ErrorKind::WouldBlock => Some("would_block"),
        std::io::ErrorKind::BrokenPipe => Some("broken_pipe"),
        std::io::ErrorKind::UnexpectedEof => Some("unexpected_eof"),
        std::io::ErrorKind::NotFound => Some("socket_not_found"),
        _ => None,
    }
}

fn orchestration_dispatch_context(
    failure: RemoteDispatchFailureInfo<'_>,
    reason: &'static str,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
) -> serde_json::Value {
    let mut context = serde_json::json!({
        "reason": reason,
        "command": failure.command,
        "session_id": failure.session.session_id,
        "session_name": failure.session.session_name,
    });
    extend_orchestration_session_path_context(&mut context, failure.session);
    if let Some(context_object) = context.as_object_mut() {
        if let Some(retry_reason) = replay_retry_reason {
            context_object.insert("retry_reason".to_string(), serde_json::json!(retry_reason));
        }
        if let Some(replay_phase) = replay_phase {
            context_object.insert("replay_phase".to_string(), serde_json::json!(replay_phase));
        }
    }
    context
}

fn orchestration_transport_dispatch_error(
    failure: RemoteDispatchFailureInfo<'_>,
    error: &std::io::Error,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
) -> ErrorEnvelope {
    let mut context = orchestration_dispatch_context(
        failure,
        failure.transport_failure_reason,
        replay_retry_reason,
        replay_phase,
    );
    if let Some(context_object) = context.as_object_mut() {
        insert_transport_error_context(context_object, error);
    }
    ErrorEnvelope::new(
        ErrorCode::IpcProtocolError,
        format!(
            "Failed to dispatch orchestration {} to {} session '{}': {error}",
            failure.dispatch_subject, failure.role, failure.session.session_name
        ),
    )
    .with_context(context)
}

fn insert_transport_error_context(
    context_object: &mut serde_json::Map<String, serde_json::Value>,
    error: &std::io::Error,
) {
    context_object.insert(
        "transport_reason".to_string(),
        serde_json::json!(classify_orchestration_transport(error)),
    );
    context_object.insert(
        "transport_error".to_string(),
        serde_json::json!(error.to_string()),
    );
    if let Some(raw_os_error) = error.raw_os_error() {
        context_object.insert(
            "transport_os_error".to_string(),
            serde_json::json!(raw_os_error),
        );
    }
}

fn orchestration_dispatch_error(
    failure: RemoteDispatchFailureInfo<'_>,
    error: &IpcClientError,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
) -> ErrorEnvelope {
    match error {
        IpcClientError::Transport(io_error) => orchestration_transport_dispatch_error(
            failure,
            io_error,
            replay_retry_reason,
            replay_phase,
        ),
        IpcClientError::Protocol(envelope) => {
            let mut context = orchestration_dispatch_context(
                failure,
                failure.protocol_failure_reason,
                replay_retry_reason,
                replay_phase,
            );
            if let Some(context_object) = context.as_object_mut() {
                context_object.insert(
                    "ipc_protocol_error".to_string(),
                    serde_json::to_value(envelope).unwrap_or_else(|_| {
                        serde_json::json!({
                            "code": envelope.code.to_string(),
                            "message": envelope.message,
                        })
                    }),
                );
            }
            ErrorEnvelope::new(
                envelope.code,
                format!(
                    "Failed to dispatch orchestration {} to {} session '{}': {}",
                    failure.dispatch_subject,
                    failure.role,
                    failure.session.session_name,
                    envelope.message
                ),
            )
            .with_context(context)
        }
    }
}

pub(crate) fn decode_orchestration_success_payload<T>(
    response: IpcResponse,
    session: &OrchestrationSessionInfo,
    missing_payload_reason: &'static str,
    missing_payload_message: &'static str,
    invalid_payload_reason: &'static str,
    invalid_payload_subject: &'static str,
) -> Result<T, ErrorEnvelope>
where
    T: DeserializeOwned,
{
    response
        .data
        .ok_or_else(|| {
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, missing_payload_message).with_context(
                serde_json::json!({
                    "reason": missing_payload_reason,
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }),
            )
        })
        .and_then(|data| {
            serde_json::from_value::<T>(data).map_err(|error| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("Failed to decode {invalid_payload_subject}: {error}"),
                )
                .with_context(serde_json::json!({
                    "reason": invalid_payload_reason,
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }))
            })
        })
}

pub(crate) fn decode_orchestration_success_payload_field<T>(
    response: IpcResponse,
    session: &OrchestrationSessionInfo,
    field_name: &str,
    missing_payload_reason: &str,
    missing_payload_message: &str,
    invalid_payload_reason: &str,
    invalid_payload_subject: &str,
) -> Result<T, ErrorEnvelope>
where
    T: DeserializeOwned,
{
    response
        .data
        .and_then(|data| data.get(field_name).cloned())
        .ok_or_else(|| {
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, missing_payload_message).with_context(
                serde_json::json!({
                    "reason": missing_payload_reason,
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }),
            )
        })
        .and_then(|payload| {
            serde_json::from_value::<T>(payload).map_err(|error| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("Failed to decode {invalid_payload_subject}: {error}"),
                )
                .with_context(serde_json::json!({
                    "reason": invalid_payload_reason,
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }))
            })
        })
}

pub(crate) fn decode_orchestration_success_result_items<T>(
    response: IpcResponse,
    session: &OrchestrationSessionInfo,
    missing_items_reason: &str,
    missing_items_message: &str,
    invalid_items_reason: &str,
    invalid_items_subject: &str,
) -> Result<Vec<T>, ErrorEnvelope>
where
    T: DeserializeOwned,
{
    response
        .data
        .and_then(|data| {
            data.get("result")
                .and_then(|result| result.get("items"))
                .cloned()
        })
        .ok_or_else(|| {
            ErrorEnvelope::new(ErrorCode::IpcProtocolError, missing_items_message).with_context(
                serde_json::json!({
                    "reason": missing_items_reason,
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }),
            )
        })
        .and_then(|items| {
            serde_json::from_value::<Vec<T>>(items).map_err(|error| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("Failed to decode {invalid_items_subject}: {error}"),
                )
                .with_context(serde_json::json!({
                    "reason": invalid_items_reason,
                    "session_id": session.session_id,
                    "session_name": session.session_name,
                }))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{
        RemoteDispatchContract, RemoteDispatchFailureInfo, align_orchestration_timeout_authority,
        dispatch_remote_orchestration_request, orchestration_timeout_budget_exhausted_error,
        project_orchestration_request_onto_deadline,
    };
    use crate::orchestration_runtime::projected_orchestration_session;
    use rub_core::error::ErrorCode;
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
}
