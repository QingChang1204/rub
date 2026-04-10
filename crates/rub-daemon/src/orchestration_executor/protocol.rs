use std::path::Path;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::OrchestrationSessionInfo;
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
use serde::de::DeserializeOwned;

use crate::orchestration_runtime::extend_orchestration_session_path_context;

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
    dispatch_subject: &'static str,
    unreachable_reason: &'static str,
    dispatch_failure_reason: &'static str,
    missing_error_message: &'static str,
) -> Result<IpcResponse, ErrorEnvelope> {
    ensure_orchestration_session_protocol(session, role)?;
    let mut client = IpcClient::connect(Path::new(&session.socket_path))
        .await
        .map_err(|error| {
            let mut context = serde_json::json!({
                "reason": unreachable_reason,
                "command": request.command,
                "session_id": session.session_id,
                "session_name": session.session_name,
            });
            extend_orchestration_session_path_context(&mut context, session);
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Unable to reach orchestration {role} session '{}' at {} while dispatching {dispatch_subject}: {error}",
                    session.session_name, session.socket_path
                ),
            )
            .with_context(context)
        })?;
    let request = bind_orchestration_daemon_authority(request, session, role)?;
    let command = request.command.clone();
    let response = client.send(&request).await.map_err(|error| {
        let mut context = serde_json::json!({
            "reason": dispatch_failure_reason,
            "command": command,
            "session_id": session.session_id,
            "session_name": session.session_name,
        });
        extend_orchestration_session_path_context(&mut context, session);
        if let Some(envelope) = error.protocol_envelope()
            && let Some(context_object) = context.as_object_mut()
        {
            context_object.insert(
                "ipc_protocol_error".to_string(),
                serde_json::to_value(envelope).unwrap_or_else(|_| serde_json::json!({
                    "code": envelope.code.to_string(),
                    "message": envelope.message,
                })),
            );
            return ErrorEnvelope::new(
                envelope.code,
                format!(
                    "Failed to dispatch orchestration {dispatch_subject} to {role} session '{}': {}",
                    session.session_name, envelope.message
                ),
            )
            .with_context(serde_json::Value::Object(context_object.clone()));
        }
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Failed to dispatch orchestration {dispatch_subject} to {role} session '{}': {error}",
                session.session_name
            ),
        )
        .with_context(context)
    })?;
    ensure_orchestration_success_response(response, missing_error_message)
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
