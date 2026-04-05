use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationActionExecutionInfo, OrchestrationMode, OrchestrationResultInfo,
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
    OrchestrationSessionInfo, OrchestrationStepResultInfo, OrchestrationStepStatus,
    TriggerActionKind, TriggerActionSpec,
};
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, ResponseStatus};
use serde::de::DeserializeOwned;
use tracing::{info, warn};

use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::{DaemonRouter, RouterTransactionGuard};
use crate::session::SessionState;
use crate::trigger_workflow_bridge::{
    resolve_trigger_workflow_source_bindings, trigger_workflow_source_var_keys,
};
use crate::workflow_assets::load_named_workflow_spec;
use crate::workflow_params::{
    parse_workflow_json_parameter_bindings, resolve_workflow_binding_map,
};
use uuid::Uuid;

mod retry;
pub(crate) mod target;

use retry::{orchestration_retry_policy, run_with_orchestration_retry};
use target::{dispatch_action_to_target_session, resolve_target_session};

const ORCHESTRATION_ACTION_BASE_TIMEOUT_MS: u64 = 30_000;
const ORCHESTRATION_TRANSIENT_RETRY_LIMIT: u32 = 3;
const ORCHESTRATION_TRANSIENT_RETRY_DELAY_MS: u64 = 100;
const ORCHESTRATION_SOURCE_MATERIALIZATION_TIMEOUT_MS: u64 = 100;

struct OrchestrationActionFailure {
    action: Option<OrchestrationActionExecutionInfo>,
    error: ErrorEnvelope,
    attempts: u32,
}

struct OrchestrationFailureInput {
    rule_id: u32,
    retained_status: OrchestrationRuleStatus,
    total_steps: u32,
    failed_step_index: u32,
    committed_steps: Vec<OrchestrationStepResultInfo>,
    failed_action: Option<OrchestrationActionExecutionInfo>,
    failed_attempts: u32,
    error: ErrorEnvelope,
}

#[derive(Clone, Copy)]
struct OrchestrationExecutionContext<'a> {
    router: &'a DaemonRouter,
    state: &'a Arc<SessionState>,
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &'a OrchestrationRuleInfo,
    execution_id: &'a str,
    rub_home: &'a Path,
}

pub(crate) async fn execute_orchestration_rule(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> OrchestrationResultInfo {
    let total_steps = rule.actions.len() as u32;
    let mut steps = Vec::new();
    let execution_id = Uuid::now_v7().to_string();
    info!(
        rule_id = rule.id,
        total_steps,
        execution_id = execution_id.as_str(),
        "orchestration_rule.start"
    );
    let context = OrchestrationExecutionContext {
        router,
        state,
        runtime,
        rule,
        execution_id: &execution_id,
        rub_home: &state.rub_home,
    };
    let target_session = match resolve_target_session(runtime, rule) {
        Ok(session) => session,
        Err(error) => {
            warn!(
                rule_id = rule.id,
                error_code = %error.code,
                reason = error.context.as_ref()
                    .and_then(|c| c.get("reason")).and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
                "orchestration_rule.target_resolve_failed"
            );
            return orchestration_failure_result(OrchestrationFailureInput {
                rule_id: rule.id,
                retained_status: rule.status,
                total_steps,
                failed_step_index: 0,
                committed_steps: steps,
                failed_action: None,
                failed_attempts: 1,
                error,
            });
        }
    };

    for (step_index, action) in rule.actions.iter().enumerate() {
        let step_index = step_index as u32;
        match dispatch_orchestration_action(context, target_session, step_index, action).await {
            Ok(step) => steps.push(step),
            Err(failure) => {
                warn!(
                    rule_id = rule.id,
                    step_index,
                    failed_attempts = failure.attempts,
                    error_code = %failure.error.code,
                    reason = failure.error.context.as_ref()
                        .and_then(|c| c.get("reason")).and_then(|v| v.as_str())
                        .unwrap_or("unknown"),
                    "orchestration_rule.step_failed"
                );
                return orchestration_failure_result(OrchestrationFailureInput {
                    rule_id: rule.id,
                    retained_status: rule.status,
                    total_steps,
                    failed_step_index: step_index,
                    committed_steps: steps,
                    failed_action: failure.action,
                    failed_attempts: failure.attempts,
                    error: failure.error,
                });
            }
        }
    }

    info!(
        rule_id = rule.id,
        total_steps,
        execution_id = execution_id.as_str(),
        "orchestration_rule.committed"
    );
    OrchestrationResultInfo {
        rule_id: rule.id,
        status: OrchestrationRuleStatus::Fired,
        next_status: successful_next_status(rule),
        summary: format!(
            "orchestration rule {} committed {}/{} action(s)",
            rule.id, total_steps, total_steps
        ),
        committed_steps: total_steps,
        total_steps,
        steps,
        cooldown_until_ms: successful_cooldown_until_ms(rule),
        error_code: None,
        reason: None,
    }
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
    response: rub_ipc::protocol::IpcResponse,
    missing_error_message: &'static str,
) -> Result<rub_ipc::protocol::IpcResponse, ErrorEnvelope> {
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
) -> Result<rub_ipc::protocol::IpcResponse, ErrorEnvelope> {
    ensure_orchestration_session_protocol(session, role)?;
    let mut client = IpcClient::connect(Path::new(&session.socket_path))
        .await
        .map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Unable to reach orchestration {role} session '{}' at {} while dispatching {dispatch_subject}: {error}",
                    session.session_name, session.socket_path
                ),
            )
            .with_context(serde_json::json!({
                "reason": unreachable_reason,
                "command": request.command,
                "socket_path": session.socket_path,
                "session_id": session.session_id,
                "session_name": session.session_name,
            }))
        })?;
    let request = bind_orchestration_daemon_authority(request, session, role)?;
    let command = request.command.clone();
    let response = client.send(&request).await.map_err(|error| {
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Failed to dispatch orchestration {dispatch_subject} to {role} session '{}': {error}",
                session.session_name
            ),
        )
        .with_context(serde_json::json!({
            "reason": dispatch_failure_reason,
            "command": command,
            "session_id": session.session_id,
            "session_name": session.session_name,
        }))
    })?;
    ensure_orchestration_success_response(response, missing_error_message)
}

pub(crate) fn decode_orchestration_success_payload<T>(
    response: rub_ipc::protocol::IpcResponse,
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

async fn dispatch_orchestration_action(
    context: OrchestrationExecutionContext<'_>,
    session: &OrchestrationSessionInfo,
    step_index: u32,
    action: &TriggerActionSpec,
) -> Result<OrchestrationStepResultInfo, OrchestrationActionFailure> {
    let action_info =
        orchestration_action_execution_info(action, context.rub_home).map_err(|error| {
            OrchestrationActionFailure {
                action: None,
                error,
                attempts: 1,
            }
        })?;
    let command_id = orchestration_step_command_id(context.rule, context.execution_id, step_index);
    let (result, attempts) =
        run_with_orchestration_retry(orchestration_retry_policy(context.rule), || async {
            let request = build_dispatchable_orchestration_action_request(
                context,
                session,
                action,
                step_index,
                &command_id,
            )
            .await?;
            let command = request.command.clone();
            let response = dispatch_action_to_target_session(
                context.router,
                context.state,
                session,
                &context.rule.target,
                request,
            )
            .await?;
            ensure_committed_automation_result(&command, response.data.as_ref())?;
            Ok(response.data)
        })
        .await
        .map_err(|failure| OrchestrationActionFailure {
            action: Some(action_info.clone()),
            error: failure.error,
            attempts: failure.attempts,
        })?;

    Ok(OrchestrationStepResultInfo {
        step_index,
        status: OrchestrationStepStatus::Committed,
        summary: format!(
            "orchestration step {} committed {}",
            step_index + 1,
            orchestration_action_label(&action_info)
        ),
        attempts,
        action: Some(action_info),
        result,
        error_code: None,
        reason: None,
    })
}

async fn build_dispatchable_orchestration_action_request(
    context: OrchestrationExecutionContext<'_>,
    session: &OrchestrationSessionInfo,
    action: &TriggerActionSpec,
    step_index: u32,
    command_id: &str,
) -> Result<IpcRequest, ErrorEnvelope> {
    let _source_transaction =
        reserve_source_materialization_authority(context, session, action, step_index).await?;
    build_orchestration_action_request(context, action, step_index, command_id).await
}

async fn reserve_source_materialization_authority<'a>(
    context: OrchestrationExecutionContext<'a>,
    session: &OrchestrationSessionInfo,
    action: &TriggerActionSpec,
    step_index: u32,
) -> Result<Option<RouterTransactionGuard<'a>>, ErrorEnvelope> {
    if !requires_remote_source_materialization(context, session, action) {
        return Ok(None);
    }

    context
        .router
        .begin_automation_transaction(
            context.state,
            ORCHESTRATION_SOURCE_MATERIALIZATION_TIMEOUT_MS,
            "orchestration_source_materialization",
        )
        .await
        .map(Some)
        .map_err(|error| {
            ErrorEnvelope::new(
                error.code,
                format!(
                    "Unable to reserve source-session automation authority before remote orchestration dispatch: {}",
                    error.message
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_materialization_transaction_unavailable",
                "source_session_id": context.rule.source.session_id,
                "source_session_name": context.rule.source.session_name,
                "target_session_id": session.session_id,
                "target_session_name": session.session_name,
                "step_index": step_index,
            }))
        })
}

fn requires_remote_source_materialization(
    context: OrchestrationExecutionContext<'_>,
    session: &OrchestrationSessionInfo,
    action: &TriggerActionSpec,
) -> bool {
    session.session_id != context.state.session_id
        && context.rule.source.session_id == context.state.session_id
        && action_requires_source_materialization(action)
}

fn action_requires_source_materialization(action: &TriggerActionSpec) -> bool {
    matches!(action.kind, TriggerActionKind::Workflow)
        && action
            .payload
            .as_ref()
            .and_then(|payload| payload.as_object())
            .is_some_and(|payload| payload.get("source_vars").is_some())
}

fn orchestration_failure_result(input: OrchestrationFailureInput) -> OrchestrationResultInfo {
    let OrchestrationFailureInput {
        rule_id,
        retained_status,
        total_steps,
        failed_step_index,
        mut committed_steps,
        failed_action,
        failed_attempts,
        error,
    } = input;
    let status = classify_orchestration_error_status(error.code);
    let step_status = match status {
        OrchestrationRuleStatus::Blocked => OrchestrationStepStatus::Blocked,
        OrchestrationRuleStatus::Degraded => OrchestrationStepStatus::Degraded,
        OrchestrationRuleStatus::Armed
        | OrchestrationRuleStatus::Paused
        | OrchestrationRuleStatus::Fired
        | OrchestrationRuleStatus::Expired => OrchestrationStepStatus::Blocked,
    };
    let reason = error
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    committed_steps.push(OrchestrationStepResultInfo {
        step_index: failed_step_index,
        status: step_status,
        summary: format!(
            "orchestration step {} failed: {}: {}",
            failed_step_index + 1,
            error.code,
            error.message
        ),
        attempts: failed_attempts,
        action: failed_action,
        result: None,
        error_code: Some(error.code),
        reason: reason.clone(),
    });
    OrchestrationResultInfo {
        rule_id,
        status,
        next_status: if failed_step_index == 0 {
            retained_status
        } else {
            status
        },
        summary: format!(
            "orchestration rule {} {} after committing {}/{} action(s)",
            rule_id,
            match status {
                OrchestrationRuleStatus::Blocked => "blocked",
                OrchestrationRuleStatus::Degraded => "degraded",
                OrchestrationRuleStatus::Armed
                | OrchestrationRuleStatus::Paused
                | OrchestrationRuleStatus::Fired
                | OrchestrationRuleStatus::Expired => "stopped",
            },
            failed_step_index,
            total_steps
        ),
        committed_steps: failed_step_index,
        total_steps,
        steps: committed_steps,
        cooldown_until_ms: None,
        error_code: Some(error.code),
        reason,
    }
}

fn successful_next_status(rule: &OrchestrationRuleInfo) -> OrchestrationRuleStatus {
    match rule.mode {
        OrchestrationMode::Once => OrchestrationRuleStatus::Fired,
        OrchestrationMode::Repeat => OrchestrationRuleStatus::Armed,
    }
}

fn successful_cooldown_until_ms(rule: &OrchestrationRuleInfo) -> Option<u64> {
    match rule.mode {
        OrchestrationMode::Once => None,
        OrchestrationMode::Repeat if rule.execution_policy.cooldown_ms == 0 => None,
        OrchestrationMode::Repeat => {
            Some(current_time_ms().saturating_add(rule.execution_policy.cooldown_ms))
        }
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn classify_orchestration_error_status(code: ErrorCode) -> OrchestrationRuleStatus {
    match code {
        ErrorCode::InvalidInput
        | ErrorCode::ElementNotFound
        | ErrorCode::ElementNotInteractable
        | ErrorCode::StaleSnapshot
        | ErrorCode::StaleIndex
        | ErrorCode::WaitTimeout
        | ErrorCode::NoMatchingOption
        | ErrorCode::FileNotFound
        | ErrorCode::AutomationPaused => OrchestrationRuleStatus::Blocked,
        _ => OrchestrationRuleStatus::Degraded,
    }
}

async fn build_orchestration_action_request(
    context: OrchestrationExecutionContext<'_>,
    action: &TriggerActionSpec,
    step_index: u32,
    command_id: &str,
) -> Result<IpcRequest, ErrorEnvelope> {
    match action.kind {
        TriggerActionKind::BrowserCommand => {
            let command = action.command.as_deref().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration browser_command action is missing action.command",
                )
            })?;
            let mut payload = action
                .payload
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            let object = payload.as_object_mut().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration browser_command action payload must be a JSON object",
                )
            })?;
            object.insert(
                "_orchestration".to_string(),
                orchestration_request_meta(
                    context.rule,
                    context.execution_id,
                    step_index,
                    "action",
                ),
            );
            let args = serde_json::Value::Object(object.clone());
            Ok(IpcRequest::new(
                command,
                args.clone(),
                orchestration_action_timeout_ms(command, &args),
            )
            .with_command_id(command_id)
            .map_err(|reason| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("orchestration step command_id is not protocol-valid: {reason}"),
                )
            })?)
        }
        TriggerActionKind::Workflow => {
            let payload = action.payload.as_ref().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration workflow action is missing action.payload",
                )
            })?;
            let object = payload.as_object().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration workflow action payload must be a JSON object",
                )
            })?;
            let (raw_spec, mut spec_source) =
                resolve_orchestration_workflow_spec(object, context.rub_home)?;
            let (parameterized, source_var_keys) = resolve_orchestration_workflow_parameterization(
                context.router,
                context.state,
                context.runtime,
                context.rule,
                object,
                &raw_spec,
            )
            .await?;
            if let Some(spec_source_object) = spec_source.as_object_mut() {
                spec_source_object.insert(
                    "vars".to_string(),
                    serde_json::json!(parameterized.parameter_keys),
                );
                if !source_var_keys.is_empty() {
                    spec_source_object.insert(
                        "source_vars".to_string(),
                        serde_json::json!(source_var_keys),
                    );
                }
            }
            let args = serde_json::json!({
                "spec": parameterized.resolved_spec,
                "spec_source": spec_source,
                "_orchestration": orchestration_request_meta(
                    context.rule,
                    context.execution_id,
                    step_index,
                    "action"
                ),
            });
            Ok(IpcRequest::new(
                "pipe",
                args.clone(),
                orchestration_action_timeout_ms("pipe", &args),
            )
            .with_command_id(command_id)
            .map_err(|reason| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!("orchestration step command_id is not protocol-valid: {reason}"),
                )
            })?)
        }
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Err(ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "orchestration action.kind is not yet executable in this runtime slice",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_action_kind_not_supported",
                "kind": action.kind,
            })))
        }
    }
}

fn orchestration_action_timeout_ms(command: &str, args: &serde_json::Value) -> u64 {
    ORCHESTRATION_ACTION_BASE_TIMEOUT_MS.saturating_add(
        rub_core::automation_timeout::command_additional_timeout_ms(command, args),
    )
}

async fn resolve_orchestration_workflow_parameterization(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    payload: &serde_json::Map<String, serde_json::Value>,
    raw_spec: &str,
) -> Result<
    (
        crate::workflow_params::WorkflowParameterization,
        Vec<String>,
    ),
    ErrorEnvelope,
> {
    let explicit = payload
        .get("vars")
        .and_then(|value| value.as_object())
        .map(parse_workflow_json_parameter_bindings)
        .transpose()
        .map_err(|error| error.into_envelope())?;
    let mut bindings = explicit.unwrap_or_default();
    let source_bindings =
        resolve_orchestration_workflow_source_bindings(router, state, runtime, rule, payload)
            .await?;
    let mut source_var_keys = source_bindings.keys().cloned().collect::<Vec<_>>();
    source_var_keys.sort();
    for (name, value) in source_bindings {
        if bindings.insert(name.clone(), value).is_some() {
            return Err(ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                format!(
                    "orchestration workflow parameter '{name}' is defined by both payload.vars and payload.source_vars"
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_workflow_duplicate_var_binding",
                "name": name,
            })));
        }
    }
    let parameterized =
        resolve_workflow_binding_map(raw_spec, &bindings).map_err(|error| error.into_envelope())?;
    Ok((parameterized, source_var_keys))
}

async fn resolve_orchestration_workflow_source_bindings(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<std::collections::BTreeMap<String, String>, ErrorEnvelope> {
    if payload.get("source_vars").is_none() {
        return Ok(Default::default());
    }

    let source_target_id = rule.source.tab_target_id.as_deref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "orchestration workflow source_vars require source.tab_target_id to be bound",
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_source_tab_target_missing",
            "source_session_id": rule.source.session_id,
            "source_session_name": rule.source.session_name,
        }))
    })?;

    if rule.source.session_id == state.session_id {
        return resolve_trigger_workflow_source_bindings(
            &router.browser_port(),
            source_target_id,
            rule.source.frame_id.as_deref(),
            payload,
        )
        .await
        .map_err(|error| error.into_envelope());
    }

    let source_session = resolve_source_session(runtime, rule)?;
    dispatch_to_source_session_for_workflow_bindings(
        source_session,
        source_target_id,
        rule.source.frame_id.as_deref(),
        payload,
    )
    .await
}

fn resolve_source_session<'a>(
    runtime: &'a OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
) -> Result<&'a OrchestrationSessionInfo, ErrorEnvelope> {
    runtime
        .known_sessions
        .iter()
        .find(|session| session.session_id == rule.source.session_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Source session '{}' is not available for orchestration workflow parameterization",
                    rule.source.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_session_missing",
                "source_session_id": rule.source.session_id,
                "source_session_name": rule.source.session_name,
            }))
        })
}

async fn dispatch_to_source_session_for_workflow_bindings(
    session: &OrchestrationSessionInfo,
    source_target_id: &str,
    source_frame_id: Option<&str>,
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<std::collections::BTreeMap<String, String>, ErrorEnvelope> {
    let response = dispatch_remote_orchestration_request(
        session,
        "source",
        IpcRequest::new(
            "_orchestration_workflow_source_vars",
            serde_json::json!({
                "tab_target_id": source_target_id,
                "frame_id": source_frame_id,
                "payload": payload,
            }),
            ORCHESTRATION_ACTION_BASE_TIMEOUT_MS,
        ),
        "workflow source vars",
        "orchestration_source_session_unreachable",
        "orchestration_source_var_dispatch_failed",
        "remote orchestration workflow source vars returned an error without an envelope",
    )
    .await?;

    decode_orchestration_success_payload(
        response,
        session,
        "orchestration_source_var_payload_missing",
        "orchestration workflow source vars returned success without a payload",
        "orchestration_source_var_payload_invalid",
        "orchestration workflow source vars payload",
    )
}

fn resolve_orchestration_workflow_spec(
    payload: &serde_json::Map<String, serde_json::Value>,
    rub_home: &Path,
) -> Result<(String, serde_json::Value), ErrorEnvelope> {
    match (
        payload.get("workflow_name").and_then(|value| value.as_str()),
        payload.get("steps"),
    ) {
        (Some(name), None) => {
            let (normalized, contents, path) =
                load_named_workflow_spec(rub_home, name).map_err(|error| error.into_envelope())?;
            Ok((
                contents,
                serde_json::json!({
                    "kind": "orchestration_workflow",
                    "name": normalized,
                    "path": path.display().to_string(),
                }),
            ))
        }
        (None, Some(steps)) if steps.is_array() => {
            let raw_steps = serde_json::to_string(steps).map_err(|error| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    format!("Failed to serialize orchestration inline workflow: {error}"),
                )
            })?;
            Ok((
                raw_steps,
                serde_json::json!({
                    "kind": "orchestration_inline_workflow",
                    "step_count": steps.as_array().map(|steps| steps.len()).unwrap_or(0),
                }),
            ))
        }
        (Some(_), Some(_)) => Err(
            ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "orchestration workflow payload must provide exactly one of payload.workflow_name or payload.steps",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_workflow_shape_invalid",
            })),
        ),
        _ => Err(
            ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "orchestration workflow payload requires non-empty payload.workflow_name or payload.steps",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_workflow_shape_invalid",
            })),
        ),
    }
}

fn orchestration_step_command_id(
    rule: &OrchestrationRuleInfo,
    execution_id: &str,
    step_index: u32,
) -> String {
    format!(
        "orchestration:{}:{}:{}",
        rule.idempotency_key, execution_id, step_index
    )
}

fn orchestration_request_meta(
    rule: &OrchestrationRuleInfo,
    execution_id: &str,
    step_index: u32,
    phase: &str,
) -> serde_json::Value {
    let command_id = orchestration_step_command_id(rule, execution_id, step_index);
    serde_json::json!({
        "id": rule.id,
        "execution_id": execution_id,
        "phase": phase,
        "step_index": step_index,
        "command_id": command_id,
        "correlation_key": rule.correlation_key,
        "idempotency_key": rule.idempotency_key,
        "source_session_id": rule.source.session_id,
        "target_session_id": rule.target.session_id,
        "target_tab_target_id": rule.target.tab_target_id,
        "frame_id": rule.target.frame_id,
    })
}

fn orchestration_action_execution_info(
    action: &TriggerActionSpec,
    rub_home: &Path,
) -> Result<OrchestrationActionExecutionInfo, ErrorEnvelope> {
    let mut vars = action
        .payload
        .as_ref()
        .and_then(|payload| payload.get("vars"))
        .and_then(|vars| vars.as_object())
        .map(|vars| vars.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    vars.sort();
    let source_vars = action
        .payload
        .as_ref()
        .and_then(|payload| payload.as_object())
        .and_then(|payload| trigger_workflow_source_var_keys(payload).ok())
        .unwrap_or_default();

    match action.kind {
        TriggerActionKind::BrowserCommand => Ok(OrchestrationActionExecutionInfo {
            kind: TriggerActionKind::BrowserCommand,
            command: action.command.clone(),
            workflow_name: None,
            workflow_path: None,
            inline_step_count: None,
            vars,
            source_vars,
        }),
        TriggerActionKind::Workflow => {
            let payload = action.payload.as_ref().ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::InvalidInput,
                    "orchestration workflow action is missing action.payload",
                )
            })?;
            let workflow_name = payload
                .get("workflow_name")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let workflow_path = workflow_name
                .as_deref()
                .and_then(|name| load_named_workflow_spec(rub_home, name).ok())
                .map(|(_, _, path)| path.display().to_string());
            let inline_step_count = payload
                .get("steps")
                .and_then(|value| value.as_array())
                .map(|steps| steps.len() as u32);
            Ok(OrchestrationActionExecutionInfo {
                kind: TriggerActionKind::Workflow,
                command: None,
                workflow_name,
                workflow_path,
                inline_step_count,
                vars,
                source_vars,
            })
        }
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Ok(OrchestrationActionExecutionInfo {
                kind: action.kind,
                command: action.command.clone(),
                workflow_name: None,
                workflow_path: None,
                inline_step_count: None,
                vars,
                source_vars,
            })
        }
    }
}

fn orchestration_action_label(action: &OrchestrationActionExecutionInfo) -> String {
    match action.kind {
        TriggerActionKind::BrowserCommand => format!(
            "'{}'",
            action.command.as_deref().unwrap_or("browser_command")
        ),
        TriggerActionKind::Workflow => action
            .workflow_name
            .as_deref()
            .map(|name| format!("workflow '{name}'"))
            .unwrap_or_else(|| "inline workflow".to_string()),
        TriggerActionKind::Provider => "provider action".to_string(),
        TriggerActionKind::Script => "script action".to_string(),
        TriggerActionKind::Webhook => "webhook action".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OrchestrationFailureInput, action_requires_source_materialization,
        bind_orchestration_daemon_authority, classify_orchestration_error_status,
        orchestration_failure_result,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::{
        OrchestrationRuleStatus, OrchestrationSessionInfo, OrchestrationStepStatus,
        TriggerActionKind, TriggerActionSpec,
    };
    use rub_ipc::protocol::IpcRequest;

    #[test]
    fn classify_orchestration_error_status_preserves_blocked_vs_degraded_boundary() {
        assert_eq!(
            classify_orchestration_error_status(ErrorCode::ElementNotFound),
            OrchestrationRuleStatus::Blocked
        );
        assert_eq!(
            classify_orchestration_error_status(ErrorCode::DaemonNotRunning),
            OrchestrationRuleStatus::Degraded
        );
    }

    #[test]
    fn orchestration_failure_result_blocks_rearm_after_partial_commit() {
        let result = orchestration_failure_result(OrchestrationFailureInput {
            rule_id: 7,
            retained_status: OrchestrationRuleStatus::Armed,
            total_steps: 3,
            failed_step_index: 1,
            committed_steps: vec![rub_core::model::OrchestrationStepResultInfo {
                step_index: 0,
                status: OrchestrationStepStatus::Committed,
                summary: "step 1 committed".to_string(),
                attempts: 1,
                action: None,
                result: None,
                error_code: None,
                reason: None,
            }],
            failed_action: None,
            failed_attempts: 1,
            error: ErrorEnvelope::new(ErrorCode::ElementNotFound, "missing element"),
        });
        assert_eq!(result.status, OrchestrationRuleStatus::Blocked);
        assert_eq!(result.next_status, OrchestrationRuleStatus::Blocked);
        assert_eq!(result.committed_steps, 1);
        assert_eq!(result.total_steps, 3);
        assert_eq!(result.steps.len(), 2);
        assert_eq!(result.steps[1].status, OrchestrationStepStatus::Blocked);
        assert_eq!(result.steps[1].attempts, 1);
    }

    #[test]
    fn orchestration_remote_request_binds_remote_daemon_authority() {
        let session = OrchestrationSessionInfo {
            current: false,
            session_id: "daemon-b".to_string(),
            session_name: "remote".to_string(),
            pid: 42,
            socket_path: "/tmp/rub.sock".to_string(),
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
        };

        let request = bind_orchestration_daemon_authority(
            IpcRequest::new("tabs", serde_json::json!({}), 1_000),
            &session,
            "target",
        )
        .expect("binding should succeed");

        assert_eq!(request.daemon_session_id.as_deref(), Some("daemon-b"));
    }

    #[test]
    fn workflow_source_vars_require_source_materialization_but_plain_actions_do_not() {
        assert!(action_requires_source_materialization(&TriggerActionSpec {
            kind: TriggerActionKind::Workflow,
            command: None,
            payload: Some(serde_json::json!({
                "source_vars": {
                    "greeting": {
                        "kind": "text",
                        "selector": "#hero"
                    }
                }
            })),
        }));
        assert!(!action_requires_source_materialization(
            &TriggerActionSpec {
                kind: TriggerActionKind::Workflow,
                command: None,
                payload: Some(serde_json::json!({
                    "vars": {
                        "name": "rub"
                    }
                })),
            }
        ));
        assert!(!action_requires_source_materialization(
            &TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(serde_json::json!({ "selector": "#submit" })),
            }
        ));
    }
}
