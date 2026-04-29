use rub_core::error::ErrorEnvelope;
use rub_core::model::{
    OrchestrationSessionInfo, OrchestrationStepResultInfo, OrchestrationStepStatus,
    TriggerActionKind, TriggerActionSpec,
};
use rub_core::recovery_contract::target_replay_or_spent_tombstone_contract_with_fresh_retry_field;
use rub_ipc::protocol::IpcRequest;

use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::{RouterFenceDisposition, RouterTransactionGuard};
use crate::scheduler_policy::AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL;

use super::action_request::{
    build_orchestration_action_request, orchestration_action_execution_info,
    orchestration_action_label, orchestration_source_materialization_wait_budget_ms,
    orchestration_step_command_id,
};
use super::protocol::align_orchestration_timeout_authority;
use super::retry::{
    OrchestrationRetryFailure, OrchestrationRetryPolicy, orchestration_retry_policy,
    run_with_orchestration_retry, run_with_orchestration_retry_with_timeout_error,
};
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
    let command_id = orchestration_step_command_id(
        context.rule,
        context.command_identity_key,
        context.execution_id,
        step_index,
    );
    let retry_policy = orchestration_retry_policy(context.rule);
    let (frozen_request, materialization_attempts) =
        run_with_orchestration_retry(retry_policy, context.outer_deadline, || async {
            build_dispatchable_orchestration_action_request(
                context,
                session,
                action,
                step_index,
                &command_id,
            )
            .await
        })
        .await
        .map_err(|failure| OrchestrationActionFailure {
            action: Some(action_info.clone()),
            error: failure.error,
            attempts: failure.attempts,
        })?;
    let dispatch_retry_policy =
        dispatch_retry_policy_after_materialization(retry_policy, materialization_attempts);
    let (result, attempts) = run_with_frozen_orchestration_request_retry(
        dispatch_retry_policy,
        context.outer_deadline,
        frozen_request,
        |request| async move {
            let command = request.command.clone();
            let response = dispatch_action_to_target_session(
                context.router,
                context.state,
                session,
                &context.rule.target,
                request,
                context.outer_deadline,
            )
            .await?;
            ensure_committed_automation_result(&command, response.data.as_ref())?;
            Ok(response.data)
        },
    )
    .await
    .map_err(|failure| OrchestrationActionFailure {
        action: Some(action_info.clone()),
        error: failure.error,
        attempts: total_orchestration_attempts(materialization_attempts, failure.attempts),
    })?;

    Ok(OrchestrationStepResultInfo {
        step_index,
        status: OrchestrationStepStatus::Committed,
        summary: format!(
            "orchestration step {} committed {}",
            step_index + 1,
            orchestration_action_label(&action_info)
        ),
        attempts: total_orchestration_attempts(materialization_attempts, attempts),
        action: Some(action_info),
        result,
        error_code: None,
        reason: None,
        error_context: None,
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
    trim_action_request_timeout_after_pre_dispatch(
        &mut request,
        step_started_at,
        step_index,
        context.outer_deadline,
    )?;
    Ok(request)
}

async fn run_with_frozen_orchestration_request_retry<T, F, Fut>(
    policy: OrchestrationRetryPolicy,
    outer_deadline: Option<TransactionDeadline>,
    request: IpcRequest,
    mut operation: F,
) -> Result<(T, u32), OrchestrationRetryFailure>
where
    F: FnMut(IpcRequest) -> Fut,
    Fut: std::future::Future<Output = Result<T, ErrorEnvelope>>,
{
    let timeout_request = request.clone();
    run_with_orchestration_retry_with_timeout_error(
        policy,
        outer_deadline,
        || orchestration_target_dispatch_outer_deadline_error(&timeout_request),
        || {
            let request = request.clone();
            operation(request)
        },
    )
    .await
}

fn orchestration_target_dispatch_outer_deadline_error(request: &IpcRequest) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!(
            "Orchestration target dispatch for '{}' exhausted the caller-owned timeout budget after possible target commit",
            request.command
        ),
    )
    .with_context(serde_json::json!({
        "reason": "orchestration_target_dispatch_outer_deadline_exhausted",
        "command": request.command.as_str(),
        "command_id": request.command_id.as_deref(),
        "target_daemon_session_id": request.daemon_session_id.as_deref(),
        "effect_commit_state": "possible_commit",
        "possible_commit_recovery_contract": target_replay_or_spent_tombstone_contract_with_fresh_retry_field(
            request.command_id.as_deref(),
            request.daemon_session_id.as_deref(),
            true,
        ),
    }))
}

fn dispatch_retry_policy_after_materialization(
    policy: OrchestrationRetryPolicy,
    materialization_attempts: u32,
) -> OrchestrationRetryPolicy {
    policy.remaining_after_attempts(materialization_attempts)
}

fn total_orchestration_attempts(materialization_attempts: u32, dispatch_attempts: u32) -> u32 {
    materialization_attempts
        .saturating_add(dispatch_attempts)
        .saturating_sub(1)
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
    if matches!(
        context.router_fence_disposition,
        RouterFenceDisposition::ReuseCurrentTransaction
    ) {
        return Ok(None);
    }

    let queue_wait_budget = std::time::Duration::from_millis(
        orchestration_source_materialization_wait_budget_ms(action, context.rub_home)?,
    );
    let queue_wait_budget = context
        .outer_deadline
        .and_then(|deadline| {
            let remaining_ms = deadline.remaining_ms();
            (remaining_ms > 0)
                .then_some(queue_wait_budget.min(std::time::Duration::from_millis(remaining_ms)))
        })
        .unwrap_or(queue_wait_budget);
    if queue_wait_budget.is_zero() {
        return Err(
            ErrorEnvelope::new(
                ErrorCode::IpcTimeout,
                format!(
                    "Orchestration step {} exhausted the caller-owned timeout budget before reserving source materialization authority",
                    step_index + 1,
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_materialization_timeout_budget_exhausted",
                "source_session_id": context.rule.source.session_id,
                "source_session_name": context.rule.source.session_name,
                "target_session_id": session.session_id,
                "target_session_name": session.session_name,
                "step_index": step_index,
            })),
        );
    }

    context
        .router
        .begin_automation_transaction_if_needed(
            context.state,
            "orchestration_source_materialization",
            queue_wait_budget,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
            context.router_fence_disposition,
        )
        .await
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
    outer_deadline: Option<crate::router::TransactionDeadline>,
) -> Result<(), ErrorEnvelope> {
    let original_timeout_ms = request.timeout_ms;
    let elapsed_ms = step_started_at.elapsed().as_millis() as u64;
    let mut remaining_timeout_ms = original_timeout_ms.saturating_sub(elapsed_ms);
    if let Some(outer_deadline) = outer_deadline {
        remaining_timeout_ms = remaining_timeout_ms.min(outer_deadline.remaining_ms());
    }
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
    use super::{
        OrchestrationRetryPolicy, dispatch_retry_policy_after_materialization,
        orchestration_target_dispatch_outer_deadline_error,
        reserve_source_materialization_authority, run_with_frozen_orchestration_request_retry,
        total_orchestration_attempts, trim_action_request_timeout_after_pre_dispatch,
    };
    use crate::orchestration_executor::OrchestrationExecutionContext;
    use crate::router::{DaemonRouter, RouterFenceDisposition, TransactionDeadline};
    use crate::session::SessionState;
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
        OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
        OrchestrationSessionAvailability, TriggerActionKind, TriggerActionSpec,
        TriggerConditionKind, TriggerConditionSpec,
    };
    use rub_ipc::protocol::IpcRequest;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio::sync::Mutex;

    fn test_router() -> DaemonRouter {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                managed_profile_ephemeral: false,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        DaemonRouter::new(adapter)
    }

    fn sample_rule() -> OrchestrationRuleInfo {
        OrchestrationRuleInfo {
            id: 7,
            status: OrchestrationRuleStatus::Armed,
            lifecycle_generation: 1,
            source: OrchestrationAddressInfo {
                session_id: "sess-local".to_string(),
                session_name: "local".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("source-tab".to_string()),
                frame_id: None,
            },
            target: OrchestrationAddressInfo {
                session_id: "sess-remote".to_string(),
                session_name: "remote".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("target-tab".to_string()),
                frame_id: None,
            },
            mode: OrchestrationMode::Repeat,
            execution_policy: OrchestrationExecutionPolicyInfo::default(),
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::TextPresent,
                locator: None,
                text: Some("ready".to_string()),
                url_pattern: None,
                readiness_state: None,
                method: None,
                status_code: None,
                storage_area: None,
                key: None,
                value: None,
            },
            actions: Vec::new(),
            correlation_key: "corr".to_string(),
            idempotency_key: "idem".to_string(),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        }
    }

    #[test]
    fn trim_action_request_timeout_after_pre_dispatch_projects_remaining_budget() {
        let started_at = tokio::time::Instant::now() - std::time::Duration::from_millis(80);
        let mut request = IpcRequest::new("wait", serde_json::json!({ "timeout_ms": 500 }), 500);

        trim_action_request_timeout_after_pre_dispatch(&mut request, started_at, 0, None)
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

        let error =
            trim_action_request_timeout_after_pre_dispatch(&mut request, started_at, 1, None)
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

    #[tokio::test]
    async fn frozen_orchestration_request_retry_reuses_same_payload_and_command_id() {
        let attempts = Arc::new(AtomicU32::new(0));
        let seen_requests = Arc::new(Mutex::new(Vec::new()));
        let request = IpcRequest::new("pipe", serde_json::json!({ "value": "frozen" }), 500)
            .with_command_id("step-cmd")
            .expect("command id should validate");

        let (request, attempts_used) = run_with_frozen_orchestration_request_retry(
            OrchestrationRetryPolicy {
                max_retries: 1,
                delay: Duration::from_millis(0),
            },
            None,
            request,
            {
                let attempts = attempts.clone();
                let seen_requests = seen_requests.clone();
                move |mut request| {
                    let attempts = attempts.clone();
                    let seen_requests = seen_requests.clone();
                    async move {
                        seen_requests.lock().await.push(request.clone());
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            request.args["value"] = serde_json::json!("mutated");
                            return Err(ErrorEnvelope::new(
                                ErrorCode::IpcProtocolError,
                                "transient dispatch failure",
                            )
                            .with_context(serde_json::json!({
                                "reason": "orchestration_target_dispatch_transport_failed",
                            })));
                        }
                        Ok(request)
                    }
                }
            },
        )
        .await
        .expect("second retry attempt should succeed with the frozen request");

        assert_eq!(attempts_used, 2);
        assert_eq!(request.command_id.as_deref(), Some("step-cmd"));
        assert_eq!(request.args["value"], "frozen");

        let seen_requests = seen_requests.lock().await;
        assert_eq!(seen_requests.len(), 2);
        assert_eq!(seen_requests[0].command_id.as_deref(), Some("step-cmd"));
        assert_eq!(seen_requests[1].command_id.as_deref(), Some("step-cmd"));
        assert_eq!(seen_requests[0].args["value"], "frozen");
        assert_eq!(seen_requests[1].args["value"], "frozen");
    }

    #[test]
    fn total_orchestration_attempts_merges_materialization_and_dispatch_retries() {
        assert_eq!(total_orchestration_attempts(1, 1), 1);
        assert_eq!(total_orchestration_attempts(2, 1), 2);
        assert_eq!(total_orchestration_attempts(1, 3), 3);
        assert_eq!(total_orchestration_attempts(2, 2), 3);
    }

    #[test]
    fn target_dispatch_outer_deadline_error_preserves_possible_commit_contract() {
        let request = IpcRequest::new("click", serde_json::json!({}), 1_000)
            .with_command_id("orchestration:idem:evidence:0")
            .expect("static command id should validate")
            .with_daemon_session_id("target-daemon")
            .expect("daemon session id should validate");

        let envelope = orchestration_target_dispatch_outer_deadline_error(&request);

        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        let context = envelope.context.expect("timeout context");
        assert_eq!(
            context["reason"],
            "orchestration_target_dispatch_outer_deadline_exhausted"
        );
        assert_eq!(context["effect_commit_state"], "possible_commit");
        assert_eq!(
            context["possible_commit_recovery_contract"]["kind"],
            "target_replay_or_spent_tombstone"
        );
        assert_eq!(
            context["possible_commit_recovery_contract"]["target_command_id"],
            "orchestration:idem:evidence:0"
        );
        assert_eq!(
            context["possible_commit_recovery_contract"]["fresh_command_retry_safe"],
            false
        );
    }

    #[tokio::test]
    async fn frozen_request_retry_uses_only_remaining_budget_after_materialization() {
        let attempts = Arc::new(AtomicU32::new(0));
        let request = IpcRequest::new("pipe", serde_json::json!({ "value": "frozen" }), 500)
            .with_command_id("step-cmd")
            .expect("command id should validate");

        let failure = run_with_frozen_orchestration_request_retry(
            dispatch_retry_policy_after_materialization(
                OrchestrationRetryPolicy {
                    max_retries: 1,
                    delay: Duration::from_millis(0),
                },
                2,
            ),
            None,
            request,
            {
                let attempts = attempts.clone();
                move |_request| {
                    let attempts = attempts.clone();
                    async move {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Err::<(), ErrorEnvelope>(ErrorEnvelope::new(
                            ErrorCode::IpcProtocolError,
                            "transient dispatch failure",
                        )
                        .with_context(serde_json::json!({
                            "reason": "orchestration_target_dispatch_transport_failed",
                        })))
                    }
                }
            },
        )
        .await
        .expect_err("dispatch should not get an extra retry after materialization spent the shared retry budget");

        assert_eq!(failure.attempts, 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn source_materialization_reuses_current_router_transaction() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-source-materialization-reuse"),
            None,
        ));
        let runtime = OrchestrationRuntimeInfo {
            known_sessions: vec![
                crate::orchestration_runtime::projected_orchestration_session(
                    "sess-remote".to_string(),
                    "remote".to_string(),
                    42,
                    "/tmp/rub-remote.sock".to_string(),
                    false,
                    rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    OrchestrationSessionAvailability::Addressable,
                    None,
                ),
            ],
            session_count: 1,
            addressing_supported: true,
            execution_supported: true,
            ..OrchestrationRuntimeInfo::default()
        };
        let rule = sample_rule();
        let action = TriggerActionSpec {
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
        };
        let context = OrchestrationExecutionContext {
            router: &router,
            state: &state,
            runtime: &runtime,
            rule: &rule,
            outer_deadline: Some(TransactionDeadline::new(500)),
            execution_id: "exec-1",
            command_identity_key: Some("evidence"),
            rub_home: &state.rub_home,
            router_fence_disposition: RouterFenceDisposition::ReuseCurrentTransaction,
        };

        let reservation = reserve_source_materialization_authority(
            context,
            &runtime.known_sessions[0],
            &action,
            0,
        )
        .await
        .expect("source materialization should reuse the active router transaction");

        assert!(reservation.is_none());
    }
}
