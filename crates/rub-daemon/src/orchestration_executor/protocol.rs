use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::OrchestrationSessionInfo;
use rub_core::recovery_contract::target_replay_or_spent_tombstone_contract;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse, ResponseStatus};
use serde::de::DeserializeOwned;

use crate::orchestration_runtime::extend_orchestration_session_path_context;
use crate::router::TransactionDeadline;
#[cfg(test)]
use crate::router::replay_request_fingerprint;

const IPC_REPLAY_TIMEOUT_BUFFER_MS: u64 = 1_000;

#[cfg(test)]
static TEST_REMOTE_ORCHESTRATION_CONNECTIONS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::BTreeMap<
            std::path::PathBuf,
            std::collections::VecDeque<tokio::net::UnixStream>,
        >,
    >,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn queue_remote_orchestration_connection_for_test(
    socket_path: impl Into<std::path::PathBuf>,
    stream: tokio::net::UnixStream,
) {
    let mut connections = TEST_REMOTE_ORCHESTRATION_CONNECTIONS
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
        .lock()
        .expect("test remote orchestration connection queue");
    connections
        .entry(socket_path.into())
        .or_default()
        .push_back(stream);
}

async fn connect_remote_orchestration_client(
    socket_path: &Path,
) -> Result<IpcClient, std::io::Error> {
    #[cfg(test)]
    if let Some(stream) = {
        let mut connections = TEST_REMOTE_ORCHESTRATION_CONNECTIONS
            .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
            .lock()
            .expect("test remote orchestration connection queue");
        let stream = connections
            .get_mut(socket_path)
            .and_then(std::collections::VecDeque::pop_front);
        if connections
            .get(socket_path)
            .is_some_and(std::collections::VecDeque::is_empty)
        {
            connections.remove(socket_path);
        }
        stream
    } {
        return Ok(IpcClient::from_connected_stream_for_test(stream));
    }

    IpcClient::connect(socket_path).await
}

pub(crate) fn bounded_orchestration_timeout_ms(
    cap_ms: u64,
    outer_deadline: Option<TransactionDeadline>,
) -> Option<u64> {
    outer_deadline
        .map(|deadline| cap_ms.min(deadline.remaining_ms()))
        .or(Some(cap_ms))
        .filter(|timeout_ms| *timeout_ms > 0)
}

pub(crate) async fn run_orchestration_future_with_outer_deadline<T, E, F, G>(
    outer_deadline: Option<TransactionDeadline>,
    timeout_error: G,
    future: F,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    G: FnOnce() -> E,
{
    let Some(deadline) = outer_deadline else {
        return future.await;
    };
    let Some(timeout) = deadline.remaining_duration() else {
        return Err(timeout_error());
    };
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result,
        Err(_) => Err(timeout_error()),
    }
}

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
    command_id: Option<&'a str>,
    daemon_session_id: Option<&'a str>,
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

#[cfg(test)]
pub(crate) fn bind_stable_orchestration_phase_command_id(
    request: IpcRequest,
    phase: &'static str,
) -> Result<IpcRequest, ErrorEnvelope> {
    let replay_fingerprint = replay_request_fingerprint(&request);
    let command = request.command.clone();
    let command_id = format!(
        "{phase}:{}",
        hex_encode_command_identity(replay_fingerprint.as_bytes())
    );
    request.with_command_id(command_id).map_err(|error| {
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!("Failed to bind stable orchestration phase command_id for {phase}: {error}"),
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_phase_command_id_bind_failed",
            "phase": phase,
            "command": command,
        }))
    })
}

pub(crate) fn bind_live_orchestration_phase_command_id(
    request: IpcRequest,
    phase: &'static str,
) -> Result<IpcRequest, ErrorEnvelope> {
    let command = request.command.clone();
    let command_id = format!("{phase}:{}", uuid::Uuid::now_v7());
    request.with_command_id(command_id).map_err(|error| {
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!("Failed to bind live orchestration phase command_id for {phase}: {error}"),
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_live_phase_command_id_bind_failed",
            "phase": phase,
            "command": command,
        }))
    })
}

#[cfg(test)]
fn hex_encode_command_identity(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

pub(crate) async fn dispatch_remote_orchestration_request(
    session: &OrchestrationSessionInfo,
    role: &'static str,
    request: IpcRequest,
    contract: RemoteDispatchContract,
) -> Result<IpcResponse, ErrorEnvelope> {
    // Replay is only safe once the caller has bound this transport request to
    // an explicit command_id for the owning phase. Live-read phases use
    // request-scoped identities; deterministic frozen phases may choose stable
    // identities. Without a wrapper command_id we fail closed after transport
    // loss instead of attempting a best-effort resend.
    ensure_orchestration_session_protocol(session, role)?;
    let mut client = connect_remote_orchestration_client(Path::new(&session.socket_path))
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
        command_id: request.command_id.as_deref(),
        daemon_session_id: request.daemon_session_id.as_deref(),
        dispatch_subject: contract.dispatch_subject,
        transport_failure_reason: contract.transport_failure_reason,
        protocol_failure_reason: contract.protocol_failure_reason,
    };
    let mut response_retry_reason = None;
    let mut response_replay_phase = None;
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
                let mut replay_client =
                    connect_remote_orchestration_client(Path::new(&session.socket_path))
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
                    Ok(response) => {
                        response_retry_reason = Some(retry_reason);
                        response_replay_phase = Some("replay_send");
                        response
                    }
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
    ensure_remote_orchestration_success_response(
        response,
        failure,
        contract.missing_error_message,
        response_retry_reason,
        response_replay_phase,
    )
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
        "inspect"
            if request
                .args
                .get("sub")
                .and_then(|value| value.as_str())
                .is_some_and(|sub| sub == "list")
                && request.args.get("wait_timeout_ms").is_some() =>
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
    {
        if object.contains_key("timeout_ms") {
            object.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
        }
        if request.command == "inspect"
            && object
                .get("sub")
                .and_then(|value| value.as_str())
                .is_some_and(|sub| sub == "list")
            && object.contains_key("wait_timeout_ms")
        {
            object.insert("wait_timeout_ms".to_string(), serde_json::json!(timeout_ms));
        }
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
        IpcClientError::Protocol(envelope) => orchestration_recoverable_protocol_reason(envelope),
    }
}

fn orchestration_recoverable_protocol_reason(
    envelope: &rub_core::error::ErrorEnvelope,
) -> Option<&'static str> {
    match envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
    {
        Some("ipc_response_timeout_after_request_commit") => {
            Some("response_timeout_after_request_commit")
        }
        Some("ipc_response_transport_failure_after_request_commit") => {
            Some("response_transport_failure_after_request_commit")
        }
        _ => None,
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
        "command_id": failure.command_id,
        "daemon_session_id": failure.daemon_session_id,
        "session_id": failure.session.session_id,
        "session_name": failure.session.session_name,
        "possible_commit_recovery_contract": target_replay_or_spent_tombstone_contract(
            failure.command_id,
            failure.daemon_session_id,
        ),
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

fn ensure_remote_orchestration_success_response(
    response: IpcResponse,
    failure: RemoteDispatchFailureInfo<'_>,
    missing_error_message: &'static str,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
) -> Result<IpcResponse, ErrorEnvelope> {
    match response.status {
        ResponseStatus::Success => Ok(response),
        ResponseStatus::Error => {
            let error = response.error.unwrap_or_else(|| {
                ErrorEnvelope::new(ErrorCode::IpcProtocolError, missing_error_message)
            });
            Err(augment_remote_orchestration_error(
                failure,
                error,
                replay_retry_reason,
                replay_phase,
            ))
        }
    }
}

fn augment_remote_orchestration_error(
    failure: RemoteDispatchFailureInfo<'_>,
    mut error: ErrorEnvelope,
    replay_retry_reason: Option<&str>,
    replay_phase: Option<&str>,
) -> ErrorEnvelope {
    let remote_context = error
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let remote_reason = remote_context.get("reason").cloned();
    let dispatch_context = orchestration_dispatch_context(
        failure,
        "orchestration_remote_error_response",
        replay_retry_reason,
        replay_phase,
    );
    let mut context = dispatch_context.as_object().cloned().unwrap_or_default();
    if !remote_context.is_empty() {
        context.insert(
            "remote_context".to_string(),
            serde_json::Value::Object(remote_context),
        );
    }
    if let Some(reason) = remote_reason {
        context.insert("remote_reason".to_string(), reason);
    }
    context.insert(
        "reason".to_string(),
        serde_json::json!("orchestration_remote_error_response"),
    );
    context.insert(
        "local_dispatch_reason".to_string(),
        serde_json::json!(failure.protocol_failure_reason),
    );
    error.context = Some(serde_json::Value::Object(context));
    error
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
mod tests;
