use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationRegistrationSpec,
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationSessionInfo,
};
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest};
use uuid::Uuid;

use crate::orchestration_executor::execute_orchestration_rule;
use crate::orchestration_executor::target::{
    dispatch_to_target_session, ensure_orchestration_target_continuity,
};
use crate::orchestration_probe::evaluate_orchestration_probe_for_tab;
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::session::SessionState;
use crate::trigger_workflow_bridge::resolve_trigger_workflow_source_bindings;

use super::DaemonRouter;
use super::request_args::{parse_json_args, parse_json_spec, required_string_arg};

mod addressing;
mod rule;

use addressing::resolve_orchestration_address;
use rule::{
    blocked_cooldown_result, orchestration_rule_in_cooldown,
    orchestration_rule_to_registration_spec, orchestration_status_name,
    validate_orchestration_registration_spec,
};

const ORCHESTRATION_ADDRESS_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug)]
enum OrchestrationCommand {
    Add(OrchestrationAddArgs),
    List,
    Trace(OrchestrationTraceArgs),
    Remove(OrchestrationIdArgs),
    Pause(OrchestrationIdArgs),
    Resume(OrchestrationIdArgs),
    Execute(OrchestrationIdArgs),
    Export(OrchestrationIdArgs),
}

impl OrchestrationCommand {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("list")
        {
            "add" => Ok(Self::Add(parse_json_args(args, "orchestration add")?)),
            "list" => Ok(Self::List),
            "trace" => Ok(Self::Trace(parse_json_args(args, "orchestration trace")?)),
            "remove" => Ok(Self::Remove(parse_json_args(args, "orchestration remove")?)),
            "pause" => Ok(Self::Pause(parse_json_args(args, "orchestration pause")?)),
            "resume" => Ok(Self::Resume(parse_json_args(args, "orchestration resume")?)),
            "execute" => Ok(Self::Execute(parse_json_args(
                args,
                "orchestration execute",
            )?)),
            "export" => Ok(Self::Export(parse_json_args(args, "orchestration export")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown orchestration subcommand '{other}'"),
            )),
        }
    }

    async fn execute(
        self,
        router: &DaemonRouter,
        state: &Arc<SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        match self {
            Self::Add(args) => cmd_orchestration_add(router, args, state).await,
            Self::List => cmd_orchestration_list(state).await,
            Self::Trace(args) => cmd_orchestration_trace(args, state).await,
            Self::Remove(args) => cmd_orchestration_remove(args, state).await,
            Self::Pause(args) => {
                update_orchestration_status(args.id, state, OrchestrationRuleStatus::Paused).await
            }
            Self::Resume(args) => {
                update_orchestration_status(args.id, state, OrchestrationRuleStatus::Armed).await
            }
            Self::Execute(args) => cmd_orchestration_execute(router, args.id, state).await,
            Self::Export(args) => cmd_orchestration_export(args.id, state).await,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OrchestrationAddArgs {
    #[serde(rename = "sub")]
    _sub: String,
    spec: String,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    spec_source: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OrchestrationTraceArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default = "default_trace_last")]
    last: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OrchestrationIdArgs {
    #[serde(rename = "sub")]
    _sub: String,
    id: u32,
}

const fn default_trace_last() -> u64 {
    20
}

pub(super) async fn cmd_orchestration(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    OrchestrationCommand::parse(args)?
        .execute(router, state)
        .await
}

fn orchestration_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: &rub_core::model::OrchestrationRuntimeInfo,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

fn orchestration_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "orchestration_registry",
    })
}

fn orchestration_rule_subject(id: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "orchestration_rule",
        "id": id,
    })
}

pub(super) async fn cmd_orchestration_probe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let tab_target_id = required_string_arg(args, "tab_target_id")?;
    let condition_value = args.get("condition").cloned().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "Missing required argument: 'condition'",
        )
    })?;
    let condition = serde_json::from_value(condition_value).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("_orchestration_probe condition payload is invalid: {error}"),
        )
    })?;
    let after_sequence = args
        .get("after_sequence")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let last_observed_drop_count = args
        .get("last_observed_drop_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let frame_id = args
        .get("frame_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let result = evaluate_orchestration_probe_for_tab(
        &router.browser_port(),
        state,
        &tab_target_id,
        frame_id,
        &condition,
        after_sequence,
        last_observed_drop_count,
    )
    .await?;

    serde_json::to_value(result).map_err(RubError::from)
}

pub(super) async fn cmd_orchestration_workflow_source_vars(
    router: &DaemonRouter,
    args: &serde_json::Value,
    _state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let tab_target_id = required_string_arg(args, "tab_target_id")?;
    let frame_id = args
        .get("frame_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let payload = args
        .get("payload")
        .and_then(|value| value.as_object())
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required argument: 'payload' (expected workflow action payload object)",
            )
        })?;

    let bindings = resolve_trigger_workflow_source_bindings(
        &router.browser_port(),
        &tab_target_id,
        frame_id,
        payload,
    )
    .await?;
    serde_json::to_value(bindings).map_err(RubError::from)
}

pub(super) async fn cmd_orchestration_target_dispatch(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let target: OrchestrationAddressInfo =
        serde_json::from_value(args.get("target").cloned().ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required argument: 'target'",
            )
        })?)
        .map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("_orchestration_target_dispatch target payload is invalid: {error}"),
            )
        })?;
    let request: IpcRequest =
        serde_json::from_value(args.get("request").cloned().ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required argument: 'request'",
            )
        })?)
        .map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("_orchestration_target_dispatch request payload is invalid: {error}"),
            )
        })?;

    if target.session_id != state.session_id {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Target dispatch request does not belong to this session authority",
            serde_json::json!({
                "reason": "orchestration_target_session_mismatch",
                "requested_session_id": target.session_id,
                "current_session_id": state.session_id,
            }),
        ));
    }

    let current_session = OrchestrationSessionInfo {
        session_id: state.session_id.clone(),
        session_name: state.session_name.clone(),
        pid: std::process::id(),
        socket_path: state.socket_path().display().to_string(),
        current: true,
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: router.browser.launch_policy().user_data_dir,
    };
    ensure_orchestration_target_continuity(router, state, &current_session, &target)
        .await
        .map_err(RubError::Domain)?;
    let response = dispatch_to_target_session(router, state, &current_session, request)
        .await
        .map_err(RubError::Domain)?;
    response.data.ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "_orchestration_target_dispatch succeeded without a payload",
            serde_json::json!({
                "reason": "orchestration_target_dispatch_payload_missing",
                "target_session_id": state.session_id,
                "target_session_name": state.session_name,
            }),
        )
    })
}

async fn cmd_orchestration_add(
    router: &DaemonRouter,
    args: OrchestrationAddArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let mut spec =
        parse_json_spec::<OrchestrationRegistrationSpec>(&args.spec, "orchestration add")?;
    validate_orchestration_registration_spec(&mut spec)?;

    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    ensure_orchestration_addressing_available(&runtime)?;
    let source = resolve_orchestration_address(
        router,
        state,
        &runtime.known_sessions,
        &spec.source,
        "source",
    )
    .await?;
    let target = resolve_orchestration_address(
        router,
        state,
        &runtime.known_sessions,
        &spec.target,
        "target",
    )
    .await?;

    let correlation_key = spec
        .correlation_key
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let idempotency_key = spec
        .idempotency_key
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let rule = state
        .register_orchestration_rule(OrchestrationRuleInfo {
            id: 0,
            status: if args.paused {
                OrchestrationRuleStatus::Paused
            } else {
                OrchestrationRuleStatus::Armed
            },
            source,
            target,
            mode: spec.mode,
            execution_policy: OrchestrationExecutionPolicyInfo {
                cooldown_ms: spec.execution_policy.cooldown_ms,
                max_retries: spec.execution_policy.max_retries,
                cooldown_until_ms: None,
            },
            condition: spec.condition,
            actions: spec.actions,
            correlation_key: correlation_key.clone(),
            idempotency_key: idempotency_key.clone(),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        })
        .await
        .map_err(|existing_rule_id| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Orchestration idempotency_key '{idempotency_key}' is already registered on rule {existing_rule_id}"
                ),
                serde_json::json!({
                    "reason": "duplicate_idempotency_key",
                    "idempotency_key": idempotency_key,
                    "existing_rule_id": existing_rule_id,
                    "correlation_key": correlation_key,
                }),
            )
        })?;
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == rule.id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(rule.id),
        serde_json::json!({
            "rule": rule,
            "spec_source": args.spec_source.unwrap_or_else(|| serde_json::json!({ "kind": "inline" })),
        }),
        &runtime,
    ))
}

async fn cmd_orchestration_list(state: &Arc<SessionState>) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    Ok(orchestration_payload(
        orchestration_registry_subject(),
        serde_json::json!({
            "items": runtime.rules.clone(),
        }),
        &runtime,
    ))
}

async fn cmd_orchestration_trace(
    args: OrchestrationTraceArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let last = usize::try_from(args.last).unwrap_or(usize::MAX);
    let runtime = state.orchestration_runtime().await;
    let trace = state.orchestration_trace(last).await;
    Ok(orchestration_payload(
        serde_json::json!({
            "kind": "orchestration_trace",
            "last": last,
        }),
        serde_json::to_value(trace).map_err(RubError::from)?,
        &runtime,
    ))
}

async fn cmd_orchestration_remove(
    args: OrchestrationIdArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let id = args.id;
    let removed = state.remove_orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "removed": removed,
        }),
        &runtime,
    ))
}

async fn cmd_orchestration_execute(
    router: &DaemonRouter,
    id: u32,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    ensure_orchestration_execution_available(&runtime)?;
    let rule = state.orchestration_rule(id).await.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is not present in the current registry"),
        )
    })?;
    if !matches!(rule.status, OrchestrationRuleStatus::Armed) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Orchestration rule id {id} must be 'armed' before execute; current status is '{}'",
                orchestration_status_name(rule.status),
            ),
        ));
    }
    if rule.unavailable_reason.is_some() {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Orchestration rule id {id} is currently unavailable"),
            serde_json::json!({
                "reason": "orchestration_rule_unavailable",
                "unavailable_reason": rule.unavailable_reason,
            }),
        ));
    }
    if orchestration_rule_in_cooldown(&rule) {
        let result = blocked_cooldown_result(&rule);
        let rule = state
            .record_orchestration_outcome(id, None, result.clone())
            .await
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Orchestration rule id {id} disappeared while recording cooldown outcome"
                ))
            })?;
        refresh_orchestration_runtime(state).await;
        let runtime = state.orchestration_runtime().await;
        let rule = runtime
            .rules
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .unwrap_or(rule);

        return Ok(orchestration_payload(
            orchestration_rule_subject(id),
            serde_json::json!({
                "rule": rule,
                "execution": result,
            }),
            &runtime,
        ));
    }

    let runtime = state.orchestration_runtime().await;
    let result = execute_orchestration_rule(router, state, &runtime, &rule).await;
    let rule = state
        .record_orchestration_outcome(id, None, result.clone())
        .await
        .ok_or_else(|| {
            RubError::Internal(format!(
                "Orchestration rule id {id} disappeared while recording execution outcome"
            ))
        })?;
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "rule": rule,
            "execution": result,
        }),
        &runtime,
    ))
}

fn ensure_orchestration_addressing_available(
    runtime: &rub_core::model::OrchestrationRuntimeInfo,
) -> Result<(), RubError> {
    if runtime.addressing_supported {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "Orchestration session addressing is temporarily unavailable because the live registry authority is degraded",
        serde_json::json!({
            "reason": "orchestration_registry_degraded",
            "degraded_reason": runtime.degraded_reason,
            "addressing_supported": runtime.addressing_supported,
            "execution_supported": runtime.execution_supported,
        }),
    ))
}

fn ensure_orchestration_execution_available(
    runtime: &rub_core::model::OrchestrationRuntimeInfo,
) -> Result<(), RubError> {
    if runtime.execution_supported {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        ErrorCode::SessionBusy,
        "Orchestration execution is temporarily unavailable because the live registry authority is degraded",
        serde_json::json!({
            "reason": "orchestration_registry_degraded",
            "degraded_reason": runtime.degraded_reason,
            "addressing_supported": runtime.addressing_supported,
            "execution_supported": runtime.execution_supported,
        }),
    ))
}

async fn cmd_orchestration_export(
    id: u32,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::ElementNotFound,
                format!("Orchestration rule {id} not found"),
                serde_json::json!({
                "reason": "orchestration_rule_not_found",
                "id": id,
                }),
            )
        })?;
    let spec = orchestration_rule_to_registration_spec(&rule);
    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "format": "orchestration",
            "rule": rule,
            "spec": spec,
        }),
        &runtime,
    ))
}

async fn update_orchestration_status(
    id: u32,
    state: &Arc<SessionState>,
    next_status: OrchestrationRuleStatus,
) -> Result<serde_json::Value, RubError> {
    let current = state
        .orchestration_rules()
        .await
        .into_iter()
        .find(|rule| rule.id == id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Orchestration rule id {id} is not present in the current registry"),
            )
        })?;

    let rule = match (current.status, next_status) {
        (OrchestrationRuleStatus::Armed, OrchestrationRuleStatus::Paused)
        | (OrchestrationRuleStatus::Paused, OrchestrationRuleStatus::Armed) => state
            .set_orchestration_rule_status(id, next_status)
            .await
            .ok_or_else(|| {
                RubError::Internal(format!(
                    "Orchestration rule id {id} disappeared while applying status update"
                ))
            })?,
        (status, requested) if status == requested => current,
        _ => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Orchestration rule id {id} cannot transition from '{}' to '{}'",
                    orchestration_status_name(current.status),
                    orchestration_status_name(next_status),
                ),
            ));
        }
    };

    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    let rule = runtime
        .rules
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .unwrap_or(rule);

    Ok(orchestration_payload(
        orchestration_rule_subject(id),
        serde_json::json!({
            "rule": rule,
        }),
        &runtime,
    ))
}

#[cfg(test)]
fn required_u32_arg(args: &serde_json::Value, name: &str) -> Result<u32, RubError> {
    let value = args
        .get(name)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Missing required argument: '{name}'"),
            )
        })?;
    u32::try_from(value).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Argument '{name}' exceeds maximum supported id {}",
                u32::MAX
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{
        OrchestrationIdArgs, ensure_orchestration_addressing_available,
        ensure_orchestration_execution_available, required_u32_arg,
    };
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use rub_core::model::OrchestrationRuntimeInfo;

    #[test]
    fn orchestration_rule_id_rejects_values_larger_than_u32() {
        let error = required_u32_arg(&serde_json::json!({"id": u64::from(u32::MAX) + 1}), "id")
            .expect_err("oversized id should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn typed_orchestration_id_payload_rejects_values_larger_than_u32() {
        let error = parse_json_args::<OrchestrationIdArgs>(
            &serde_json::json!({"sub": "export", "id": u64::from(u32::MAX) + 1}),
            "orchestration export",
        )
        .expect_err("oversized export id should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn degraded_orchestration_runtime_rejects_addressing_and_execution() {
        let runtime = OrchestrationRuntimeInfo {
            addressing_supported: false,
            execution_supported: false,
            degraded_reason: Some("registry_read_failed:boom".to_string()),
            ..OrchestrationRuntimeInfo::default()
        };
        let addressing = ensure_orchestration_addressing_available(&runtime)
            .expect_err("degraded runtime must reject addressing");
        let execution = ensure_orchestration_execution_available(&runtime)
            .expect_err("degraded runtime must reject execution");
        assert_eq!(addressing.into_envelope().code, ErrorCode::SessionBusy);
        assert_eq!(execution.into_envelope().code, ErrorCode::SessionBusy);
    }
}
