use rub_core::error::ErrorEnvelope;
use rub_core::model::{
    OrchestrationSessionInfo, OrchestrationStepResultInfo, OrchestrationStepStatus,
    TriggerActionKind, TriggerActionSpec,
};
use rub_ipc::protocol::IpcRequest;

use crate::router::RouterTransactionGuard;
use crate::router::automation_fence::ensure_committed_automation_result;

use super::action_request::{
    build_orchestration_action_request, orchestration_action_execution_info,
    orchestration_action_label, orchestration_step_command_id,
};
use super::retry::{orchestration_retry_policy, run_with_orchestration_retry};
use super::target::dispatch_action_to_target_session;
use super::*;

pub(super) async fn dispatch_orchestration_action(
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

pub(super) fn action_requires_source_materialization(action: &TriggerActionSpec) -> bool {
    matches!(action.kind, TriggerActionKind::Workflow)
        && action
            .payload
            .as_ref()
            .and_then(|payload| payload.as_object())
            .is_some_and(|payload| payload.get("source_vars").is_some())
}
