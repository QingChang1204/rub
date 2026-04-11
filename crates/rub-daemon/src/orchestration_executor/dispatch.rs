use rub_core::error::ErrorEnvelope;
use rub_core::model::{
    OrchestrationSessionInfo, OrchestrationStepResultInfo, OrchestrationStepStatus,
    TriggerActionKind, TriggerActionSpec,
};
use rub_ipc::protocol::IpcRequest;

use crate::router::RouterTransactionGuard;
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::scheduler_policy::AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL;

use super::action_request::{
    build_orchestration_action_request, orchestration_action_execution_info,
    orchestration_action_label, orchestration_source_materialization_wait_budget_ms,
    orchestration_step_command_id,
};
use super::protocol::align_orchestration_timeout_authority;
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
    let step_started_at = tokio::time::Instant::now();
    let _source_transaction =
        reserve_source_materialization_authority(context, session, action, step_index).await?;
    let mut request =
        build_orchestration_action_request(context, action, step_index, command_id).await?;
    trim_action_request_timeout_after_pre_dispatch(&mut request, step_started_at, step_index)?;
    Ok(request)
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

    let queue_wait_budget = std::time::Duration::from_millis(
        orchestration_source_materialization_wait_budget_ms(action, context.rub_home)?,
    );

    context
        .router
        .begin_automation_transaction_with_wait_budget(
            context.state,
            "orchestration_source_materialization",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
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
                "wait_budget_ms": queue_wait_budget.as_millis(),
            }))
        })
}

fn trim_action_request_timeout_after_pre_dispatch(
    request: &mut IpcRequest,
    step_started_at: tokio::time::Instant,
    step_index: u32,
) -> Result<(), ErrorEnvelope> {
    let original_timeout_ms = request.timeout_ms;
    let elapsed_ms = step_started_at.elapsed().as_millis() as u64;
    let remaining_timeout_ms = original_timeout_ms.saturating_sub(elapsed_ms);
    if remaining_timeout_ms == 0 {
        return Err(
            ErrorEnvelope::new(
                ErrorCode::IpcTimeout,
                format!(
                    "orchestration step {} exhausted its declared timeout budget of {}ms before target dispatch",
                    step_index + 1,
                    original_timeout_ms
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_action_timeout_budget_exhausted_before_target_dispatch",
                "step_index": step_index,
                "original_timeout_ms": original_timeout_ms,
                "elapsed_pre_dispatch_ms": elapsed_ms,
            })),
        );
    }
    request.timeout_ms = remaining_timeout_ms;
    align_orchestration_timeout_authority(request).map_err(|reason| {
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            format!(
                "Failed to align orchestration step {} timeout authority before target dispatch: {reason}",
                step_index + 1
            ),
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_action_timeout_authority_projection_failed",
            "step_index": step_index,
            "original_timeout_ms": original_timeout_ms,
            "elapsed_pre_dispatch_ms": elapsed_ms,
        }))
    })?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::trim_action_request_timeout_after_pre_dispatch;
    use rub_core::error::ErrorCode;
    use rub_ipc::protocol::IpcRequest;

    #[test]
    fn trim_action_request_timeout_after_pre_dispatch_projects_remaining_budget() {
        let started_at = tokio::time::Instant::now() - std::time::Duration::from_millis(80);
        let mut request = IpcRequest::new("wait", serde_json::json!({ "timeout_ms": 500 }), 500);

        trim_action_request_timeout_after_pre_dispatch(&mut request, started_at, 0)
            .expect("remaining budget should stay positive");

        assert!(request.timeout_ms <= 420);
        assert_eq!(
            request
                .args
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64),
            Some(request.timeout_ms.saturating_sub(1_000))
        );
    }

    #[test]
    fn trim_action_request_timeout_after_pre_dispatch_fails_when_budget_is_exhausted() {
        let started_at = tokio::time::Instant::now() - std::time::Duration::from_millis(50);
        let mut request = IpcRequest::new("wait", serde_json::json!({ "timeout_ms": 10 }), 10);

        let error = trim_action_request_timeout_after_pre_dispatch(&mut request, started_at, 1)
            .expect_err("elapsed pre-dispatch budget should fail closed");

        assert_eq!(error.code, ErrorCode::IpcTimeout);
        let context = error.context.expect("timeout error should publish context");
        assert_eq!(
            context["reason"],
            "orchestration_action_timeout_budget_exhausted_before_target_dispatch"
        );
        assert_eq!(context["step_index"], 1u32);
        assert_eq!(context["original_timeout_ms"], 10u64);
        assert!(
            context["elapsed_pre_dispatch_ms"]
                .as_u64()
                .is_some_and(|elapsed| elapsed >= 50),
            "elapsed pre-dispatch budget should record the consumed wait time"
        );
    }
}
