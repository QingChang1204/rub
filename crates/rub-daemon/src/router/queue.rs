use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::Timing;
use rub_ipc::protocol::{IpcRequest, IpcResponse};

use crate::scheduler_policy::AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL;
use crate::session::SessionState;

#[cfg(test)]
use crate::scheduler_policy::{
    AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS, AUTOMATION_QUEUE_WAIT_BUDGET,
    AUTOMATION_QUEUE_WAIT_BUDGET_MS,
};

use super::dispatch::execute_named_command_with_fence;
use super::runtime;
use super::transaction::{
    DispatchPreparation, PreparedCommandDispatch, execution_timeout_error,
    execution_timeout_response, finalize_response, preflight_rejection_response,
    prepare_command_dispatch, prepare_request_preflight, queue_timeout_response,
};
use super::{
    DaemonRouter, OwnedRouterTransactionGuard, RouterTransactionGuard, TransactionDeadline,
};

impl DaemonRouter {
    pub async fn dispatch(&self, request: IpcRequest, state: &Arc<SessionState>) -> IpcResponse {
        let prepared = match self
            .prepare_dispatch(request.clone(), state, true, false)
            .await
        {
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

    pub(crate) async fn begin_automation_transaction_with_wait_budget<'a>(
        &'a self,
        state: &Arc<SessionState>,
        command: &str,
        queue_wait_budget: std::time::Duration,
        shutdown_poll_interval: std::time::Duration,
    ) -> Result<RouterTransactionGuard<'a>, ErrorEnvelope> {
        let acquire = self.exec_semaphore.acquire();
        tokio::pin!(acquire);
        let queue_deadline = tokio::time::Instant::now() + queue_wait_budget;
        loop {
            if tokio::time::Instant::now() >= queue_deadline {
                state.record_queue_pressure_timeout();
                return Err(automation_queue_timeout_rejection(
                    command,
                    queue_wait_budget,
                ));
            }
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
                _ = tokio::time::sleep(shutdown_poll_interval) => {
                    if state.is_shutdown_requested() {
                        return Err(automation_shutdown_rejection(state, command));
                    }
                }
            }
        }
    }

    pub(crate) async fn begin_automation_transaction_until_shutdown_owned(
        &self,
        state: &Arc<SessionState>,
        command: &str,
    ) -> Result<OwnedRouterTransactionGuard, ErrorEnvelope> {
        let acquire = self.exec_semaphore.clone().acquire_owned();
        tokio::pin!(acquire);
        loop {
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
                _ = tokio::time::sleep(AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL) => {
                    if state.is_shutdown_requested() {
                        return Err(automation_shutdown_rejection(state, command));
                    }
                }
            }
        }
    }

    pub(crate) fn dispatch_within_active_transaction_preserving_replay<'a>(
        &'a self,
        request: IpcRequest,
        state: &'a Arc<SessionState>,
    ) -> Pin<Box<dyn Future<Output = IpcResponse> + Send + 'a>> {
        Box::pin(async move {
            match self
                .prepare_dispatch(request.clone(), state, false, true)
                .await
            {
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
        in_process_dispatch: bool,
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

        if let Some(response) =
            preflight_rejection_response(&request, &preflight, state, in_process_dispatch)
        {
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
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use tokio::sync::{mpsc, oneshot};
    use uuid::Uuid;

    fn test_router() -> Arc<DaemonRouter> {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        Arc::new(DaemonRouter::new(adapter))
    }

    fn temp_home(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rub-queue-{label}-{}", Uuid::now_v7()))
    }

    #[tokio::test]
    async fn automation_transactions_share_fifo_authority_with_default_budget() {
        let router = test_router();
        let state = Arc::new(SessionState::new("default", temp_home("fairness"), None));
        let foreground_hold = Duration::from_millis(
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS
                .saturating_mul(3)
                .min(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_sub(50)),
        );

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let queued = router.begin_automation_transaction_with_wait_budget(
            &state,
            "queued_automation",
            AUTOMATION_QUEUE_WAIT_BUDGET,
            Duration::from_millis(5),
        );
        tokio::pin!(queued);

        tokio::select! {
            _ = tokio::time::sleep(foreground_hold) => {}
            _ = &mut queued => panic!("queued automation should still be waiting for the held transaction"),
        }
        drop(held);

        let guard = queued
            .await
            .expect("queued automation should acquire after the first transaction releases");
        drop(guard);
    }

    #[tokio::test]
    async fn queued_automation_is_rejected_after_shutdown_request() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("shutdown-fence"),
            None,
        ));
        let foreground_hold = Duration::from_millis(
            AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS
                .saturating_mul(3)
                .min(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_sub(50)),
        );

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let queued = router.begin_automation_transaction_with_wait_budget(
            &state,
            "queued_automation",
            AUTOMATION_QUEUE_WAIT_BUDGET,
            Duration::from_millis(5),
        );
        tokio::pin!(queued);

        tokio::select! {
            _ = tokio::time::sleep(foreground_hold) => {}
            _ = &mut queued => panic!("queued automation should still be waiting for the held transaction"),
        }
        state.request_shutdown();
        drop(held);

        let error = match queued.await {
            Ok(_) => panic!("queued automation should be fenced out during shutdown"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::SessionBusy);
        assert_eq!(
            error.context,
            Some(serde_json::json!({
                "command": "queued_automation",
                "reason": "session_shutting_down_after_queue_wait",
            }))
        );
    }

    #[tokio::test]
    async fn automation_transaction_returns_timeout_once_wait_budget_expires() {
        let router = test_router();
        let state = Arc::new(SessionState::new("default", temp_home("wait-budget"), None));

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let error = match router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "queued_automation",
                Duration::from_millis(20),
                Duration::from_millis(5),
            )
            .await
        {
            Ok(_) => panic!("queue wait should time out once the worker budget expires"),
            Err(error) => error,
        };

        assert_eq!(error.code, ErrorCode::IpcTimeout);
        assert_eq!(
            error.context,
            Some(serde_json::json!({
                "command": "queued_automation",
                "reason": "automation_queue_wait_budget_exceeded",
                "wait_budget_ms": 20,
            }))
        );
        drop(held);
    }

    #[tokio::test]
    async fn active_automation_transactions_can_wait_longer_than_worker_cycle_budget() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("active-step-budget"),
            None,
        ));

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let queued = router.begin_automation_transaction_with_wait_budget(
            &state,
            "orchestration_source_materialization",
            Duration::from_millis(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_add(250)),
            Duration::from_millis(5),
        );
        tokio::pin!(queued);

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_add(25))) => {}
            _ = &mut queued => panic!("active-step reservation should keep waiting past the worker fairness budget"),
        }
        drop(held);

        let guard = queued.await.expect(
            "active-step reservation should still acquire once the foreground slot releases",
        );
        drop(guard);
    }

    #[tokio::test]
    async fn waiting_automation_acquires_when_foreground_releases_within_wait_budget() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("within-budget"),
            None,
        ));

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let queued = router.begin_automation_transaction_with_wait_budget(
            &state,
            "queued_automation",
            AUTOMATION_QUEUE_WAIT_BUDGET,
            Duration::from_millis(5),
        );
        tokio::pin!(queued);

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(30)) => {}
            _ = &mut queued => panic!("queued automation should still be waiting for the held transaction"),
        }
        drop(held);

        let guard = queued.await.expect(
            "queued automation should eventually acquire once the foreground transaction releases",
        );
        drop(guard);
    }

    #[tokio::test]
    async fn queued_automation_keeps_fifo_priority_over_later_foreground_arrivals() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("fifo-priority"),
            None,
        ));

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let (order_tx, mut order_rx) = mpsc::unbounded_channel();
        let (release_automation_tx, release_automation_rx) = oneshot::channel();
        let (release_foreground_tx, release_foreground_rx) = oneshot::channel();

        let automation_router = router.clone();
        let automation_state = state.clone();
        let automation_order_tx = order_tx.clone();
        let automation_task = tokio::spawn(async move {
            let guard = automation_router
                .begin_automation_transaction_with_wait_budget(
                    &automation_state,
                    "queued_automation",
                    AUTOMATION_QUEUE_WAIT_BUDGET,
                    Duration::from_millis(5),
                )
                .await
                .expect("queued automation should eventually acquire");
            automation_order_tx
                .send("automation")
                .expect("automation acquisition order should send");
            let _ = release_automation_rx.await;
            drop(guard);
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        let foreground_router = router.clone();
        let foreground_state = state.clone();
        let foreground_order_tx = order_tx.clone();
        let foreground_task = tokio::spawn(async move {
            let guard = foreground_router
                .begin_request_transaction(
                    "later_foreground",
                    "req-later-foreground",
                    TransactionDeadline::new(1_000),
                    &foreground_state,
                )
                .await
                .expect("later foreground request should eventually acquire");
            foreground_order_tx
                .send("foreground")
                .expect("foreground acquisition order should send");
            let _ = release_foreground_rx.await;
            drop(guard);
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(5), order_rx.recv())
                .await
                .is_err(),
            "no waiter should acquire while the initial guard is still held"
        );

        drop(held);

        let first = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
            .await
            .expect("first queued waiter should acquire after the held guard releases")
            .expect("first queued waiter label should be present");
        assert_eq!(first, "automation");

        release_automation_tx
            .send(())
            .expect("automation release signal should send");

        let second = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
            .await
            .expect("second queued waiter should acquire after automation releases")
            .expect("second queued waiter label should be present");
        assert_eq!(second, "foreground");

        release_foreground_tx
            .send(())
            .expect("foreground release signal should send");

        automation_task
            .await
            .expect("automation task should complete cleanly");
        foreground_task
            .await
            .expect("foreground task should complete cleanly");
    }

    #[tokio::test]
    async fn persistent_automation_reservation_keeps_fifo_priority_past_worker_cycle_budget() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("persistent-fifo-priority"),
            None,
        ));

        let held = router
            .begin_automation_transaction_with_wait_budget(
                &state,
                "hold_foreground_slot",
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
            .await
            .expect("first automation transaction should acquire immediately");

        let (order_tx, mut order_rx) = mpsc::unbounded_channel();
        let (release_automation_tx, release_automation_rx) = oneshot::channel();
        let (release_foreground_tx, release_foreground_rx) = oneshot::channel();

        let automation_router = router.clone();
        let automation_state = state.clone();
        let automation_order_tx = order_tx.clone();
        let automation_task = tokio::spawn(async move {
            let guard = automation_router
                .begin_automation_transaction_until_shutdown_owned(
                    &automation_state,
                    "queued_automation",
                )
                .await
                .expect("persistent automation reservation should eventually acquire");
            automation_order_tx
                .send("automation")
                .expect("automation acquisition order should send");
            let _ = release_automation_rx.await;
            drop(guard);
        });

        tokio::time::sleep(Duration::from_millis(
            AUTOMATION_QUEUE_WAIT_BUDGET_MS.saturating_add(25),
        ))
        .await;

        let foreground_router = router.clone();
        let foreground_state = state.clone();
        let foreground_order_tx = order_tx.clone();
        let foreground_task = tokio::spawn(async move {
            let guard = foreground_router
                .begin_request_transaction(
                    "later_foreground",
                    "req-later-foreground",
                    TransactionDeadline::new(1_000),
                    &foreground_state,
                )
                .await
                .expect("later foreground request should eventually acquire");
            foreground_order_tx
                .send("foreground")
                .expect("foreground acquisition order should send");
            let _ = release_foreground_rx.await;
            drop(guard);
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(5), order_rx.recv())
                .await
                .is_err(),
            "persistent automation contender should still be queued while the foreground hold remains active"
        );

        drop(held);

        let first = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
            .await
            .expect("first queued waiter should acquire after the held guard releases")
            .expect("first queued waiter label should be present");
        assert_eq!(first, "automation");

        release_automation_tx
            .send(())
            .expect("automation release signal should send");

        let second = tokio::time::timeout(Duration::from_millis(100), order_rx.recv())
            .await
            .expect("second queued waiter should acquire after automation releases")
            .expect("second queued waiter label should be present");
        assert_eq!(second, "foreground");

        release_foreground_tx
            .send(())
            .expect("foreground release signal should send");

        automation_task
            .await
            .expect("automation task should complete cleanly");
        foreground_task
            .await
            .expect("foreground task should complete cleanly");
    }
}
