use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use rub_core::command::{
    CommandEffectClass, DomEpochPolicy, TimeoutRecoverySurface, command_metadata,
};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::Timing;
use rub_ipc::protocol::{IpcRequest, IpcResponse};

use crate::scheduler_policy::{
    AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL, AUTOMATION_QUEUE_WAIT_BUDGET,
};
use crate::session::SessionState;

#[cfg(test)]
use crate::scheduler_policy::{
    AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS, AUTOMATION_QUEUE_WAIT_BUDGET_MS,
};

use super::dispatch::execute_named_command_with_fence;
use super::runtime;
use super::timeout_projection::{
    ExecutionTimeoutProjectionRecorder,
    record_effectful_command_possible_commit_timeout_projection, scope_timeout_projection,
};
#[cfg(test)]
use super::transaction::prepare_replay_fence;
use super::transaction::{
    DispatchPreparation, PendingResponseCommit, PreparedCommandDispatch, execution_timeout_error,
    execution_timeout_response, handoff_blocked_error_for_command,
    handoff_blocked_response_for_command, preflight_rejection_response, prepare_command_dispatch,
    prepare_request_preflight_with_inherited_deadline, queue_timeout_response,
};
use super::{
    DaemonRouter, OwnedRouterTransactionGuard, RouterFenceDisposition, RouterTransactionGuard,
    TransactionDeadline,
};

impl DaemonRouter {
    pub async fn dispatch(&self, request: IpcRequest, state: &Arc<SessionState>) -> IpcResponse {
        self.dispatch_for_external_delivery(request, state)
            .await
            .commit_locally(state)
            .await
    }

    pub(crate) async fn dispatch_for_external_delivery(
        &self,
        request: IpcRequest,
        state: &Arc<SessionState>,
    ) -> PendingResponseCommit {
        let prepared = match self
            .prepare_dispatch(request.clone(), state, true, false, None)
            .await
        {
            DispatchPreparation::Final(response) => return *response,
            DispatchPreparation::Prepared(prepared) => prepared,
        };

        self.execute_queued_prepared_request_for_external(&request, state, prepared)
            .await
    }

    #[cfg(test)]
    async fn acquire_fifo_permit<'a>(
        &'a self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<tokio::sync::SemaphorePermit<'a>, IpcResponse> {
        let Some(timeout) = deadline.remaining_duration() else {
            state.record_queue_pressure_timeout();
            return Err(queue_timeout_response(command, request_id, deadline));
        };
        match tokio::time::timeout(timeout, self.exec_semaphore.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(IpcResponse::error(
                request_id,
                ErrorEnvelope::new(ErrorCode::IpcTimeout, "Command queue closed"),
            )),
            Err(_) => {
                state.record_queue_pressure_timeout();
                Err(queue_timeout_response(command, request_id, deadline))
            }
        }
    }

    #[cfg(test)]
    async fn begin_request_transaction<'a>(
        &'a self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<RouterTransactionGuard<'a>, IpcResponse> {
        let permit = self
            .acquire_fifo_permit(command, request_id, deadline, state)
            .await?;
        if state.is_shutdown_requested() {
            return Err(IpcResponse::error(
                request_id,
                ErrorEnvelope::new(
                    ErrorCode::SessionBusy,
                    format!(
                        "Session '{}' is draining for shutdown; command '{}' is temporarily rejected",
                        state.session_name, command
                    ),
                )
                .with_context(serde_json::json!({
                    "command": command,
                    "reason": "session_shutting_down_after_queue_wait",
                })),
            ));
        }
        if let Some(response) =
            handoff_blocked_response_for_command(command, state, request_id).await
        {
            drop(permit);
            return Err(response);
        }
        let in_flight_count = state
            .in_flight_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        state.record_in_flight_count_observation(in_flight_count);
        Ok(RouterTransactionGuard {
            _permit: permit,
            state: state.clone(),
        })
    }

    async fn begin_request_transaction_owned(
        &self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<OwnedRouterTransactionGuard, IpcResponse> {
        let Some(timeout) = deadline.remaining_duration() else {
            state.record_queue_pressure_timeout();
            return Err(queue_timeout_response(command, request_id, deadline));
        };
        let permit = match tokio::time::timeout(
            timeout,
            self.exec_semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(IpcResponse::error(
                    request_id,
                    ErrorEnvelope::new(ErrorCode::IpcTimeout, "Command queue closed"),
                ));
            }
            Err(_) => {
                state.record_queue_pressure_timeout();
                return Err(queue_timeout_response(command, request_id, deadline));
            }
        };
        if state.is_shutdown_requested() {
            return Err(IpcResponse::error(
                request_id,
                ErrorEnvelope::new(
                    ErrorCode::SessionBusy,
                    format!(
                        "Session '{}' is draining for shutdown; command '{}' is temporarily rejected",
                        state.session_name, command
                    ),
                )
                .with_context(serde_json::json!({
                    "command": command,
                    "reason": "session_shutting_down_after_queue_wait",
                })),
            ));
        }
        if let Some(response) =
            handoff_blocked_response_for_command(command, state, request_id).await
        {
            drop(permit);
            return Err(response);
        }
        let in_flight_count = state
            .in_flight_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        state.record_in_flight_count_observation(in_flight_count);
        Ok(OwnedRouterTransactionGuard {
            _permit: permit,
            state: state.clone(),
        })
    }

    pub(crate) async fn begin_automation_transaction_with_wait_budget<'a>(
        &'a self,
        state: &Arc<SessionState>,
        command: &str,
        queue_wait_budget: Duration,
        shutdown_poll_interval: Duration,
    ) -> Result<RouterTransactionGuard<'a>, ErrorEnvelope> {
        let acquire = self.exec_semaphore.acquire();
        tokio::pin!(acquire);
        let queue_deadline = tokio::time::Instant::now() + queue_wait_budget;
        loop {
            let now = tokio::time::Instant::now();
            if now >= queue_deadline {
                state.record_queue_pressure_timeout();
                return Err(automation_queue_timeout_rejection(
                    command,
                    queue_wait_budget,
                ));
            }
            let remaining = queue_deadline.saturating_duration_since(now);
            tokio::select! {
                permit = &mut acquire => {
                    let permit = permit.map_err(|_| {
                        ErrorEnvelope::new(
                            ErrorCode::IpcTimeout,
                            format!("Automation transaction '{command}' failed because the command queue closed"),
                        )
                    })?;
                    if state.is_shutdown_requested() {
                        drop(permit);
                        return Err(automation_shutdown_rejection(state, command));
                    }
                    if let Some(error) = handoff_blocked_error_for_command(command, state).await {
                        drop(permit);
                        return Err(error);
                    }
                    let in_flight_count = state
                        .in_flight_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        .saturating_add(1);
                    state.record_in_flight_count_observation(in_flight_count);
                    return Ok(RouterTransactionGuard {
                        _permit: permit,
                        state: state.clone(),
                    });
                }
                _ = tokio::time::sleep(shutdown_poll_interval.min(remaining)) => {
                    if state.is_shutdown_requested() {
                        return Err(automation_shutdown_rejection(state, command));
                    }
                    if let Some(error) = handoff_blocked_error_for_command(command, state).await {
                        return Err(error);
                    }
                }
            }
        }
    }

    pub(crate) async fn begin_automation_transaction_if_needed<'a>(
        &'a self,
        state: &Arc<SessionState>,
        command: &str,
        queue_wait_budget: Duration,
        shutdown_poll_interval: Duration,
        disposition: RouterFenceDisposition,
    ) -> Result<Option<RouterTransactionGuard<'a>>, ErrorEnvelope> {
        match disposition {
            RouterFenceDisposition::Acquire => self
                .begin_automation_transaction_with_wait_budget(
                    state,
                    command,
                    queue_wait_budget,
                    shutdown_poll_interval,
                )
                .await
                .map(Some),
            RouterFenceDisposition::ReuseCurrentTransaction => Ok(None),
        }
    }

    pub(crate) async fn begin_automation_transaction_with_wait_budget_owned(
        &self,
        state: &Arc<SessionState>,
        command: &str,
        queue_wait_budget: Duration,
        shutdown_poll_interval: Duration,
    ) -> Result<OwnedRouterTransactionGuard, ErrorEnvelope> {
        let acquire = self.exec_semaphore.clone().acquire_owned();
        tokio::pin!(acquire);
        let queue_deadline = tokio::time::Instant::now() + queue_wait_budget;
        loop {
            let now = tokio::time::Instant::now();
            if now >= queue_deadline {
                state.record_queue_pressure_timeout();
                return Err(automation_queue_timeout_rejection(
                    command,
                    queue_wait_budget,
                ));
            }
            let remaining = queue_deadline.saturating_duration_since(now);
            tokio::select! {
                permit = &mut acquire => {
                    let permit = permit.map_err(|_| {
                        ErrorEnvelope::new(
                            ErrorCode::IpcTimeout,
                            format!("Automation transaction '{command}' failed because the command queue closed"),
                        )
                    })?;
                    if state.is_shutdown_requested() {
                        drop(permit);
                        return Err(automation_shutdown_rejection(state, command));
                    }
                    if let Some(error) = handoff_blocked_error_for_command(command, state).await {
                        drop(permit);
                        return Err(error);
                    }
                    let in_flight_count = state
                        .in_flight_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        .saturating_add(1);
                    state.record_in_flight_count_observation(in_flight_count);
                    return Ok(OwnedRouterTransactionGuard {
                        _permit: permit,
                        state: state.clone(),
                    });
                }
                _ = tokio::time::sleep(shutdown_poll_interval.min(remaining)) => {
                    if state.is_shutdown_requested() {
                        return Err(automation_shutdown_rejection(state, command));
                    }
                    if let Some(error) = handoff_blocked_error_for_command(command, state).await {
                        return Err(error);
                    }
                }
            }
        }
    }

    pub(crate) async fn begin_automation_reservation_transaction_owned(
        &self,
        state: &Arc<SessionState>,
        command: &str,
    ) -> Result<OwnedRouterTransactionGuard, ErrorEnvelope> {
        self.begin_automation_transaction_with_wait_budget_owned(
            state,
            command,
            AUTOMATION_QUEUE_WAIT_BUDGET,
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL,
        )
        .await
    }

    pub(crate) fn dispatch_within_active_transaction_preserving_replay<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        self.dispatch_within_active_transaction_preserving_replay_until(request, state, None)
    }

    pub(crate) fn dispatch_within_active_transaction_preserving_replay_until<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
        inherited_deadline: Option<TransactionDeadline>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        Box::pin(async move {
            match self
                .prepare_dispatch(request.clone(), state, false, true, inherited_deadline)
                .await
            {
                DispatchPreparation::Final(response) => (*response).commit_locally(state).await,
                DispatchPreparation::Prepared(prepared) => {
                    self.execute_prepared_request(&request, state, prepared)
                        .await
                }
            }
        })
    }

    pub(crate) fn dispatch_within_active_transaction<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        self.dispatch_within_active_transaction_preserving_replay(request, state)
    }

    async fn execute_request_once(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        request_id: &str,
        queue_ms: u64,
        deadline: TransactionDeadline,
    ) -> (IpcResponse, bool) {
        let exec_start = Instant::now();
        let exec_budget_ms = deadline.remaining_ms();
        let Some(exec_timeout) = deadline.remaining_duration() else {
            return (
                execution_timeout_response(request, request_id, queue_ms, 0, deadline.timeout_ms),
                false,
            );
        };
        let timeout_projection = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        let result = match tokio::time::timeout(
            exec_timeout,
            scope_timeout_projection(timeout_projection.clone(), async {
                record_effectful_command_possible_commit_timeout_projection(
                    &request.command,
                    request.command_id.as_deref(),
                );
                self.execute_command(request, deadline, state).await
            }),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                apply_execution_timeout_authority_fence(request, state).await;
                Err(execution_timeout_error(
                    request,
                    queue_ms,
                    exec_budget_ms,
                    deadline.timeout_ms,
                    timeout_projection.snapshot(),
                ))
            }
        };
        let exec_ms = exec_start.elapsed().as_millis() as u64;
        let timing = Timing {
            queue_ms,
            exec_ms,
            total_ms: queue_ms + exec_ms,
        };

        tracing::debug!(
            command = request.command.as_str(),
            request_id,
            queue_ms,
            exec_ms,
            total_ms = timing.total_ms,
            success = result.is_ok(),
            "command_complete"
        );

        let response = match result {
            Ok(data) => IpcResponse::success(request_id, data).with_timing(timing),
            Err(err) => IpcResponse::error(request_id, err.into_envelope()).with_timing(timing),
        };
        (response, true)
    }

    async fn execute_prepared_request(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        mut prepared: PreparedCommandDispatch,
    ) -> IpcResponse {
        let queue_ms = prepared.queue_ms();
        let (response, execution_entered) = self
            .execute_request_once(
                request,
                state,
                prepared.request_id(),
                queue_ms,
                prepared.deadline(),
            )
            .await;
        if execution_entered {
            prepared.mark_execution_started();
        }
        prepared
            .prepare_response_commit(request, response)
            .commit_locally(state)
            .await
    }

    async fn execute_prepared_request_for_external(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        mut prepared: PreparedCommandDispatch,
    ) -> PendingResponseCommit {
        let queue_ms = prepared.queue_ms();
        let (response, execution_entered) = self
            .execute_request_once(
                request,
                state,
                prepared.request_id(),
                queue_ms,
                prepared.deadline(),
            )
            .await;
        if execution_entered {
            prepared.mark_execution_started();
        }
        prepared.prepare_response_commit(request, response)
    }

    async fn execute_queued_prepared_request_for_external(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        prepared: PreparedCommandDispatch,
    ) -> PendingResponseCommit {
        let transaction = match self
            .begin_request_transaction_owned(
                request.command.as_str(),
                prepared.request_id(),
                prepared.deadline(),
                state,
            )
            .await
        {
            Ok(permit) => permit,
            Err(response) => {
                return prepared.prepare_response_commit(request, response);
            }
        };

        let response = self
            .execute_prepared_request_for_external(request, state, prepared)
            .await;
        response.with_request_transaction(transaction)
    }

    async fn prepare_dispatch(
        &self,
        request: IpcRequest,
        state: &Arc<SessionState>,
        allow_handshake: bool,
        in_process_dispatch: bool,
        inherited_deadline: Option<TransactionDeadline>,
    ) -> DispatchPreparation {
        let preflight =
            prepare_request_preflight_with_inherited_deadline(&request, inherited_deadline);
        let request_id = preflight.request_id.clone();

        if allow_handshake && request.command == "_handshake" {
            let response = match runtime::cmd_handshake(self, state).await {
                Ok(data) => IpcResponse::success(&request_id, data),
                Err(error) => IpcResponse::error(&request_id, error.into_envelope()),
            };
            return DispatchPreparation::Final(Box::new(
                super::transaction::PendingResponseCommit::new(
                    request, response, true, false, None,
                ),
            ));
        }

        if let Some(response) =
            preflight_rejection_response(&request, &preflight, state, in_process_dispatch)
        {
            return DispatchPreparation::Final(Box::new(
                super::transaction::PendingResponseCommit::new(
                    request,
                    response,
                    preflight.internal_command,
                    false,
                    None,
                ),
            ));
        }

        match prepare_command_dispatch(&request, state, preflight).await {
            Ok(prepared) => DispatchPreparation::Prepared(prepared),
            Err(response) => DispatchPreparation::Final(Box::new(response)),
        }
    }

    async fn execute_command(
        &self,
        request: &IpcRequest,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        execute_named_command_with_fence(
            self,
            request.command.as_str(),
            &request.args,
            deadline,
            state,
        )
        .await
    }
}

fn automation_shutdown_rejection(state: &Arc<SessionState>, command: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::SessionBusy,
        format!(
            "Session '{}' is draining for shutdown; command '{}' is temporarily rejected",
            state.session_name, command
        ),
    )
    .with_context(serde_json::json!({
        "command": command,
        "reason": "session_shutting_down_after_queue_wait",
    }))
}

async fn apply_execution_timeout_authority_fence(request: &IpcRequest, state: &Arc<SessionState>) {
    if command_may_have_dom_commit_after_timeout(request) {
        state.mark_pending_external_dom_change();
        state.clear_all_snapshots().await;
    } else if super::policy::command_invalidates_cached_snapshots_without_epoch_bump(
        &request.command,
        &request.args,
    ) {
        state.clear_all_snapshots().await;
    }
}

fn command_may_have_dom_commit_after_timeout(request: &IpcRequest) -> bool {
    let metadata = command_metadata(&request.command);
    matches!(metadata.dom_epoch_policy, DomEpochPolicy::Bump)
        || (matches!(metadata.dom_epoch_policy, DomEpochPolicy::ArgsDependent)
            && dialog_action_commits_epoch(request))
        || (metadata.effect_class == CommandEffectClass::WorkflowMutation
            && metadata.timeout_recovery_surface == TimeoutRecoverySurface::PossibleCommit
            && metadata.dom_epoch_policy == DomEpochPolicy::None)
}

fn dialog_action_commits_epoch(request: &IpcRequest) -> bool {
    request.command == "dialog"
        && matches!(
            request.args.get("sub").and_then(|value| value.as_str()),
            Some("accept" | "dismiss")
        )
}

fn automation_queue_timeout_rejection(
    command: &str,
    queue_wait_budget: std::time::Duration,
) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!(
            "Automation transaction '{command}' exceeded its queue wait budget before acquiring the shared FIFO"
        ),
    )
    .with_context(serde_json::json!({
        "command": command,
        "reason": "automation_queue_wait_budget_exceeded",
        "wait_budget_ms": queue_wait_budget.as_millis() as u64,
    }))
}

#[cfg(test)]
mod tests;
