use rub_core::model::{OrchestrationSessionInfo, TriggerActionKind, TriggerActionSpec};
use rub_ipc::protocol::IpcRequest;

mod projection;
mod workflow;

#[cfg(test)]
pub(crate) use self::workflow::SOURCE_MATERIALIZATION_TIMEOUT_SENTINEL;
use self::workflow::{
    normalized_resolved_workflow_spec_value, orchestration_action_timeout_ms,
    orchestration_request_meta, resolve_orchestration_workflow_parameterization,
};
pub(crate) use self::workflow::{
    orchestration_action_execution_info, orchestration_source_materialization_wait_budget_ms,
    orchestration_step_command_id, resolve_orchestration_workflow_spec,
};

use super::*;

pub(super) async fn build_orchestration_action_request(
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
                    context.command_identity_key,
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
            let resolved_spec =
                normalized_resolved_workflow_spec_value(&parameterized.resolved_spec)?;
            let args = serde_json::json!({
                "spec": resolved_spec,
                "spec_source": spec_source,
                "_orchestration": orchestration_request_meta(
                    context.rule,
                    context.command_identity_key,
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

pub(super) fn orchestration_action_label(action: &OrchestrationActionExecutionInfo) -> String {
    projection::orchestration_action_label(action)
}

#[cfg(test)]
mod tests;
