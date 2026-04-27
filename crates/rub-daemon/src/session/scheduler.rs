use super::*;
use crate::scheduler_policy::{
    AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS, AUTOMATION_QUEUE_WAIT_BUDGET_MS,
};

#[derive(Clone, Copy)]
enum AutomationWorkerKind {
    Trigger,
    Orchestration,
}

impl SessionState {
    pub(crate) fn record_trigger_worker_cycle_started(&self) {
        self.record_automation_worker_cycle_started(AutomationWorkerKind::Trigger);
    }

    pub(crate) fn record_orchestration_worker_cycle_started(&self) {
        self.record_automation_worker_cycle_started(AutomationWorkerKind::Orchestration);
    }

    pub(crate) fn record_queue_pressure_timeout(&self) {
        self.queue_pressure_telemetry
            .queue_timeout_count
            .fetch_add(1, Ordering::SeqCst);
        self.queue_pressure_telemetry
            .last_queue_timeout_uptime_ms
            .store(self.uptime_millis(), Ordering::SeqCst);
    }

    pub(crate) fn record_in_flight_count_observation(&self, in_flight_count: u32) {
        atomic_max_u32(
            &self.queue_pressure_telemetry.max_in_flight_count,
            in_flight_count,
        );
    }

    pub(crate) fn record_shutdown_drain_wait(
        &self,
        in_flight_count: u32,
        connected_client_count: u32,
        pre_request_response_fence_count: u32,
    ) {
        self.shutdown_drain_telemetry
            .wait_loop_count
            .fetch_add(1, Ordering::SeqCst);
        self.shutdown_drain_telemetry
            .last_wait_uptime_ms
            .store(self.uptime_millis(), Ordering::SeqCst);
        atomic_max_u32(
            &self.shutdown_drain_telemetry.max_observed_in_flight_count,
            in_flight_count,
        );
        atomic_max_u32(
            &self
                .shutdown_drain_telemetry
                .max_observed_connected_client_count,
            connected_client_count,
        );
        atomic_max_u32(
            &self
                .shutdown_drain_telemetry
                .max_observed_pre_request_response_fence_count,
            pre_request_response_fence_count,
        );
    }

    pub(crate) fn record_shutdown_drain_soft_timeout(
        &self,
        in_flight_count: u32,
        connected_client_count: u32,
        pre_request_response_fence_count: u32,
    ) {
        self.shutdown_drain_telemetry
            .soft_timeout_count
            .fetch_add(1, Ordering::SeqCst);
        self.shutdown_drain_telemetry
            .last_soft_timeout_uptime_ms
            .store(self.uptime_millis(), Ordering::SeqCst);
        atomic_max_u32(
            &self.shutdown_drain_telemetry.max_observed_in_flight_count,
            in_flight_count,
        );
        atomic_max_u32(
            &self
                .shutdown_drain_telemetry
                .max_observed_connected_client_count,
            connected_client_count,
        );
        atomic_max_u32(
            &self
                .shutdown_drain_telemetry
                .max_observed_pre_request_response_fence_count,
            pre_request_response_fence_count,
        );
    }

    pub async fn automation_scheduler_metrics(&self) -> serde_json::Value {
        let uptime_ms = self.uptime_millis();
        let active_trigger_count = self.active_trigger_count().await;
        let active_orchestration_count = self.active_orchestration_count().await;
        let resident_orchestration_count = self.resident_orchestration_count().await;
        serde_json::json!({
            "slice": "shared_fifo_scheduler_policy",
            "authority_inventory": {
                "queue_owner": "router.exec_semaphore",
                "transaction_admission_fence": "router.begin_request_transaction",
                "in_flight_authority": "session.in_flight_count",
                "accepted_connection_fence_authority": "session.connected_client_count",
                "pre_request_response_fence_authority": "session.pre_request_response_fence_count",
                "trigger_worker_cycle_entry": "trigger_worker.run_trigger_cycle",
                "orchestration_worker_cycle_entry": "orchestration_worker.run_orchestration_cycle",
                "trigger_worker_pre_queue_gate": "none",
                "orchestration_worker_pre_queue_gate": "none",
                "automation_reservation_fence": "router.begin_automation_reservation_transaction_owned",
                "orchestration_source_materialization_reservation_fence": "router.current_transaction_or_begin_automation_transaction",
                "shutdown_drain_fence": "daemon.shutdown.wait_for_transaction_drain",
            },
            "reservation_wait_policy": {
                "response_delivery": {
                    "mode": "transport_delivery_holds_fifo_until_write_or_fallback_commit",
                    "transport_timeout_authority": "daemon.IPC_WRITE_TIMEOUT",
                    "fallback_commit_authority": "router.transaction.commit_after_delivery_failure",
                },
                "worker_cycle": {
                    "mode": "bounded_worker_cycle_budget",
                    "wait_budget_ms": AUTOMATION_QUEUE_WAIT_BUDGET_MS,
                    "shutdown_poll_interval_ms": AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS,
                },
                "active_orchestration_step": {
                    "mode": "action_timeout_budget",
                    "timeout_authority": "orchestration_action_request.timeout_ms",
                    "shutdown_poll_interval_ms": AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS,
                },
            },
            "queue_pressure": {
                "max_in_flight_count": self
                    .queue_pressure_telemetry
                    .max_in_flight_count
                    .load(Ordering::SeqCst),
                "queue_timeout_count": self
                    .queue_pressure_telemetry
                    .queue_timeout_count
                    .load(Ordering::SeqCst),
                "last_queue_timeout_uptime_ms": nonzero_u64(
                    self.queue_pressure_telemetry
                        .last_queue_timeout_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "last_queue_timeout_age_ms": age_from_uptime(
                    uptime_ms,
                    self.queue_pressure_telemetry
                        .last_queue_timeout_uptime_ms
                        .load(Ordering::SeqCst)
                ),
            },
            "trigger_worker": automation_worker_metrics_projection(
                &self.trigger_worker_telemetry,
                uptime_ms,
                active_trigger_count,
            ),
            "orchestration_worker": {
                "active_rule_count": active_orchestration_count,
                "resident_rule_count": resident_orchestration_count,
                "metrics": automation_worker_metrics_projection(
                    &self.orchestration_worker_telemetry,
                    uptime_ms,
                    active_orchestration_count,
                ),
            },
            "shutdown_drain": {
                "wait_loop_count": self
                    .shutdown_drain_telemetry
                    .wait_loop_count
                    .load(Ordering::SeqCst),
                "soft_timeout_count": self
                    .shutdown_drain_telemetry
                    .soft_timeout_count
                    .load(Ordering::SeqCst),
                "connected_only_soft_release_count": self
                    .shutdown_drain_telemetry
                    .connected_only_soft_release_count
                    .load(Ordering::SeqCst),
                "last_wait_uptime_ms": nonzero_u64(
                    self.shutdown_drain_telemetry
                        .last_wait_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "last_wait_age_ms": age_from_uptime(
                    uptime_ms,
                    self.shutdown_drain_telemetry
                        .last_wait_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "last_soft_timeout_uptime_ms": nonzero_u64(
                    self.shutdown_drain_telemetry
                        .last_soft_timeout_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "last_soft_timeout_age_ms": age_from_uptime(
                    uptime_ms,
                    self.shutdown_drain_telemetry
                        .last_soft_timeout_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "last_connected_only_soft_release_uptime_ms": nonzero_u64(
                    self.shutdown_drain_telemetry
                        .last_connected_only_soft_release_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "last_connected_only_soft_release_age_ms": age_from_uptime(
                    uptime_ms,
                    self.shutdown_drain_telemetry
                        .last_connected_only_soft_release_uptime_ms
                        .load(Ordering::SeqCst)
                ),
                "max_observed_in_flight_count": self
                    .shutdown_drain_telemetry
                    .max_observed_in_flight_count
                    .load(Ordering::SeqCst),
                "max_observed_connected_client_count": self
                    .shutdown_drain_telemetry
                    .max_observed_connected_client_count
                    .load(Ordering::SeqCst),
                "max_observed_pre_request_response_fence_count": self
                    .shutdown_drain_telemetry
                    .max_observed_pre_request_response_fence_count
                    .load(Ordering::SeqCst),
            },
        })
    }

    fn automation_worker_telemetry(
        &self,
        kind: AutomationWorkerKind,
    ) -> &AutomationWorkerTelemetry {
        match kind {
            AutomationWorkerKind::Trigger => &self.trigger_worker_telemetry,
            AutomationWorkerKind::Orchestration => &self.orchestration_worker_telemetry,
        }
    }

    fn record_automation_worker_cycle_started(&self, kind: AutomationWorkerKind) {
        let telemetry = self.automation_worker_telemetry(kind);
        telemetry.cycle_count.fetch_add(1, Ordering::SeqCst);
        telemetry
            .last_cycle_uptime_ms
            .store(self.uptime_millis(), Ordering::SeqCst);
    }
}

fn automation_worker_metrics_projection(
    telemetry: &AutomationWorkerTelemetry,
    uptime_ms: u64,
    rule_count: u32,
) -> serde_json::Value {
    let last_cycle_uptime_ms = telemetry.last_cycle_uptime_ms.load(Ordering::SeqCst);
    serde_json::json!({
        "rule_count": rule_count,
        "cycle_count": telemetry.cycle_count.load(Ordering::SeqCst),
        "last_cycle_uptime_ms": nonzero_u64(last_cycle_uptime_ms),
        "last_cycle_age_ms": age_from_uptime(uptime_ms, last_cycle_uptime_ms),
    })
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn age_from_uptime(current_uptime_ms: u64, event_uptime_ms: u64) -> Option<u64> {
    (event_uptime_ms != 0).then_some(current_uptime_ms.saturating_sub(event_uptime_ms))
}

fn atomic_max_u32(target: &AtomicU32, candidate: u32) {
    let mut current = target.load(Ordering::SeqCst);
    while candidate > current {
        match target.compare_exchange(current, candidate, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}
