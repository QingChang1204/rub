use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::Timing;
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use uuid::Uuid;

use crate::session::SessionState;

use super::dispatch::execute_named_command_with_fence;
use super::runtime;
use super::transaction::{
    DispatchPreparation, PreparedCommandDispatch, execution_timeout_error,
    execution_timeout_response, finalize_response, preflight_rejection_response,
    prepare_command_dispatch, prepare_request_preflight, queue_timeout_response,
};
use super::{DaemonRouter, RouterTransactionGuard, TransactionDeadline};

impl DaemonRouter {
    pub async fn dispatch(&self, request: IpcRequest, state: &Arc<SessionState>) -> IpcResponse {
        let prepared = match self.prepare_dispatch(request.clone(), state, true).await {
            DispatchPreparation::Final(response) => return response,
            DispatchPreparation::Prepared(prepared) => prepared,
        };

        self.execute_queued_prepared_request(&request, state, prepared)
            .await
    }

    async fn acquire_fifo_permit<'a>(
        &'a self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
    ) -> Result<tokio::sync::SemaphorePermit<'a>, IpcResponse> {
        let Some(timeout) = deadline.remaining_duration() else {
            return Err(queue_timeout_response(command, request_id, deadline));
        };
        match tokio::time::timeout(timeout, self.exec_semaphore.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(IpcResponse::error(
                request_id,
                ErrorEnvelope::new(ErrorCode::IpcTimeout, "Command queue closed"),
            )),
            Err(_) => Err(queue_timeout_response(command, request_id, deadline)),
        }
    }

    async fn begin_request_transaction<'a>(
        &'a self,
        command: &str,
        request_id: &str,
        deadline: TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<RouterTransactionGuard<'a>, IpcResponse> {
        let permit = self
            .acquire_fifo_permit(command, request_id, deadline)
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
        state
            .in_flight_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(RouterTransactionGuard {
            _permit: permit,
            state: state.clone(),
        })
    }

    pub(crate) async fn begin_automation_transaction<'a>(
        &'a self,
        state: &Arc<SessionState>,
        timeout_ms: u64,
        command: &str,
    ) -> Result<RouterTransactionGuard<'a>, ErrorEnvelope> {
        let request_id = Uuid::now_v7().to_string();
        self.begin_request_transaction(
            command,
            &request_id,
            TransactionDeadline::new(timeout_ms),
            state,
        )
        .await
        .map_err(|response| {
            response.error.unwrap_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::IpcTimeout,
                    format!("Automation transaction '{command}' failed to acquire FIFO"),
                )
            })
        })
    }

    pub(crate) fn dispatch_within_active_transaction_preserving_replay<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        Box::pin(async move {
            match self.prepare_dispatch(request.clone(), state, false).await {
                DispatchPreparation::Final(response) => response,
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
    ) -> IpcResponse {
        let exec_start = Instant::now();
        let exec_budget_ms = deadline.remaining_ms();
        let Some(exec_timeout) = deadline.remaining_duration() else {
            return execution_timeout_response(request, request_id, queue_ms, 0);
        };
        let result = if request_owns_authoritative_timeout(request) {
            self.execute_command(request, deadline, state).await
        } else {
            match tokio::time::timeout(exec_timeout, self.execute_command(request, deadline, state))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(execution_timeout_error(request, queue_ms, exec_budget_ms)),
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

        match result {
            Ok(data) => IpcResponse::success(request_id, data).with_timing(timing),
            Err(err) => IpcResponse::error(request_id, err.into_envelope()).with_timing(timing),
        }
    }

    async fn execute_prepared_request(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        mut prepared: PreparedCommandDispatch,
    ) -> IpcResponse {
        let queue_ms = prepared.queue_ms();
        prepared.mark_execution_started();
        let response = self
            .execute_request_once(
                request,
                state,
                prepared.request_id(),
                queue_ms,
                prepared.deadline(),
            )
            .await;
        prepared.finalize(request, response, state).await
    }

    async fn execute_queued_prepared_request(
        &self,
        request: &IpcRequest,
        state: &Arc<SessionState>,
        prepared: PreparedCommandDispatch,
    ) -> IpcResponse {
        let transaction = match self
            .begin_request_transaction(
                request.command.as_str(),
                prepared.request_id(),
                prepared.deadline(),
                state,
            )
            .await
        {
            Ok(permit) => permit,
            Err(response) => {
                return prepared.finalize(request, response, state).await;
            }
        };

        let response = self
            .execute_prepared_request(request, state, prepared)
            .await;
        drop(transaction);
        response
    }

    async fn prepare_dispatch(
        &self,
        request: IpcRequest,
        state: &Arc<SessionState>,
        allow_handshake: bool,
    ) -> DispatchPreparation {
        let preflight = prepare_request_preflight(&request);
        let request_id = preflight.request_id.clone();

        if allow_handshake && request.command == "_handshake" {
            let response = match runtime::cmd_handshake(self, state).await {
                Ok(data) => IpcResponse::success(&request_id, data),
                Err(error) => IpcResponse::error(&request_id, error.into_envelope()),
            };
            return DispatchPreparation::Final(
                finalize_response(&request, response, true, None, state).await,
            );
        }

        if let Some(response) = preflight_rejection_response(&request, &preflight, state) {
            return DispatchPreparation::Final(
                finalize_response(&request, response, preflight.internal_command, None, state)
                    .await,
            );
        }

        match prepare_command_dispatch(&request, state, preflight).await {
            Ok(prepared) => DispatchPreparation::Prepared(prepared),
            Err(response) => DispatchPreparation::Final(response),
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

pub(crate) fn request_owns_authoritative_timeout(request: &IpcRequest) -> bool {
    request.command == "download"
        && request.args.get("sub").and_then(|value| value.as_str()) == Some("save")
}
