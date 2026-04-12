use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::{
    ReadinessInfo, TabInfo, TriggerActionExecutionInfo, TriggerActionKind, TriggerConditionKind,
    TriggerEvidenceInfo, TriggerInfo, TriggerResultInfo, TriggerStatus,
};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::debug;

use crate::router::DaemonRouter;
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::runtime_refresh::{
    refresh_live_frame_runtime, refresh_live_runtime_state, refresh_live_trigger_runtime,
};
use crate::scheduler_policy::AUTOMATION_WORKER_POLL_INTERVAL;
use crate::session::SessionState;

mod action;
mod condition;
mod outcome;
mod reservation;

use action::{
    fire_trigger, trigger_action_command_id, trigger_action_execution_info, trigger_action_summary,
};
#[cfg(test)]
use action::{resolve_trigger_workflow_spec, trigger_target_continuity_failure};
use condition::{
    TriggerConditionState, TriggeredTriggerCondition, commit_trigger_network_progress,
    load_trigger_condition_state, reconcile_worker_state, trigger_evidence_consumption_key,
};
use outcome::record_trigger_failure;
use reservation::{
    PendingTriggerConditionPolicy, PendingTriggerReservation, TriggerReservationCompletion,
    complete_trigger_reservation, spawn_trigger_reservation,
};

const TRIGGER_WORKER_INTERVAL: Duration = AUTOMATION_WORKER_POLL_INTERVAL;
const TRIGGER_ACTION_BASE_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy)]
struct TriggerWorkerEntry {
    last_status: TriggerStatus,
    network_cursor: u64,
    observatory_drop_count: u64,
}

struct TriggerReservationCoordinator<'a> {
    pending_reservations: &'a mut HashMap<u32, PendingTriggerReservation>,
    reservation_tx: &'a tokio::sync::mpsc::UnboundedSender<TriggerReservationCompletion>,
    next_reservation_attempt_id: &'a mut u64,
}

fn trigger_condition_requires_revalidation_after_queue(trigger: &TriggerInfo) -> bool {
    !matches!(trigger.condition.kind, TriggerConditionKind::NetworkRequest)
}

fn trigger_rule_semantics_fingerprint(trigger: &TriggerInfo) -> String {
    serde_json::json!({
        "mode": trigger.mode,
        "source": {
            "target_id": trigger.source_tab.target_id,
            "frame_id": trigger.source_tab.frame_id,
        },
        "target": {
            "target_id": trigger.target_tab.target_id,
            "frame_id": trigger.target_tab.frame_id,
        },
        "condition": trigger.condition,
        "action": trigger.action,
    })
    .to_string()
}

pub(crate) fn spawn_trigger_worker(
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
    shutdown: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TRIGGER_WORKER_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut worker_state = HashMap::<u32, TriggerWorkerEntry>::new();
        let (reservation_tx, mut reservation_rx) =
            tokio::sync::mpsc::unbounded_channel::<TriggerReservationCompletion>();
        let mut pending_reservations = HashMap::<u32, PendingTriggerReservation>::new();
        let mut next_reservation_attempt_id = 0_u64;

        loop {
            if state.is_shutdown_requested() {
                abort_pending_trigger_reservations(&mut pending_reservations);
                break;
            }
            tokio::select! {
                _ = shutdown.notified() => {
                    abort_pending_trigger_reservations(&mut pending_reservations);
                    break;
                }
                _ = ticker.tick() => {
                    if state.is_shutdown_requested() {
                        abort_pending_trigger_reservations(&mut pending_reservations);
                        break;
                    }
                    run_trigger_cycle(
                        &router,
                        &state,
                        &mut worker_state,
                        &mut pending_reservations,
                        &mut reservation_rx,
                        &reservation_tx,
                        &mut next_reservation_attempt_id,
                    ).await;
                }
                Some(completion) = reservation_rx.recv() => {
                    handle_trigger_reservation_completion(
                        &router,
                        &state,
                        &mut worker_state,
                        &mut pending_reservations,
                        completion,
                    ).await;
                }
            }
        }
    })
}

async fn run_trigger_cycle(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
    pending_reservations: &mut HashMap<u32, PendingTriggerReservation>,
    reservation_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TriggerReservationCompletion>,
    reservation_tx: &tokio::sync::mpsc::UnboundedSender<TriggerReservationCompletion>,
    next_reservation_attempt_id: &mut u64,
) {
    // Queue admission is the authoritative fairness boundary; don't short-circuit
    // trigger evaluation just because another transaction is currently in flight.
    state.record_trigger_worker_cycle_started();

    let triggers = state.triggers().await;
    if triggers.is_empty() {
        abort_pending_trigger_reservations(pending_reservations);
        worker_state.clear();
        return;
    }

    let browser = router.browser_port();
    let tabs = match refresh_live_trigger_runtime(&browser, state).await {
        Ok(tabs) => tabs,
        Err(error) => {
            debug!(error = %error, "Trigger worker skipped cycle after tab refresh failure");
            return;
        }
    };

    let active_request_cursor = state.network_request_cursor().await;
    let observatory_drop_count = state.network_request_drop_count().await;
    reconcile_worker_state(
        worker_state,
        &triggers,
        active_request_cursor,
        observatory_drop_count,
    );
    reconcile_pending_trigger_reservations(&triggers, pending_reservations);
    drain_trigger_reservation_completions(
        router,
        state,
        worker_state,
        pending_reservations,
        reservation_rx,
    )
    .await;

    for trigger in triggers {
        drain_trigger_reservation_completions(
            router,
            state,
            worker_state,
            pending_reservations,
            reservation_rx,
        )
        .await;
        if !matches!(trigger.status, TriggerStatus::Armed) || trigger.unavailable_reason.is_some() {
            cancel_pending_trigger_reservation(pending_reservations, trigger.id);
            continue;
        }

        let mut reservation_coordinator = TriggerReservationCoordinator {
            pending_reservations,
            reservation_tx,
            next_reservation_attempt_id,
        };
        process_trigger_rule(
            router,
            state,
            &browser,
            &tabs,
            trigger,
            worker_state,
            &mut reservation_coordinator,
        )
        .await;
        drain_trigger_reservation_completions(
            router,
            state,
            worker_state,
            pending_reservations,
            reservation_rx,
        )
        .await;
    }
}

async fn drain_trigger_reservation_completions(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
    pending_reservations: &mut HashMap<u32, PendingTriggerReservation>,
    reservation_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TriggerReservationCompletion>,
) {
    loop {
        match reservation_rx.try_recv() {
            Ok(completion) => {
                handle_trigger_reservation_completion(
                    router,
                    state,
                    worker_state,
                    pending_reservations,
                    completion,
                )
                .await;
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
        }
    }
}

async fn process_trigger_rule(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    tabs: &[TabInfo],
    trigger: TriggerInfo,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
    reservation_coordinator: &mut TriggerReservationCoordinator<'_>,
) {
    let requires_revalidation = trigger_condition_requires_revalidation_after_queue(&trigger);
    let rule_semantics_fingerprint = trigger_rule_semantics_fingerprint(&trigger);
    if let Some(pending) = reservation_coordinator
        .pending_reservations
        .get(&trigger.id)
        && !requires_revalidation
    {
        if pending.condition_policy.rule_semantics_fingerprint == rule_semantics_fingerprint {
            return;
        }
        cancel_pending_trigger_reservation(
            reservation_coordinator.pending_reservations,
            trigger.id,
        );
    }

    let condition = match load_trigger_condition_state(
        browser,
        state,
        tabs,
        &trigger,
        worker_state
            .get_mut(&trigger.id)
            .expect("worker_state entry should exist"),
    )
    .await
    {
        Ok(condition) => condition,
        Err(error) => {
            record_trigger_failure(
                state,
                &trigger,
                ErrorEnvelope::new(
                    ErrorCode::BrowserCrashed,
                    format!("trigger condition evaluation failed: {error}"),
                ),
                None,
                None,
            )
            .await;
            return;
        }
    };

    let triggered = match condition {
        TriggerConditionState::NotTriggered { network_progress } => {
            let _ = state.set_trigger_condition_evidence(trigger.id, None).await;
            if let Some(worker) = worker_state.get_mut(&trigger.id) {
                commit_trigger_network_progress(worker, network_progress);
            }
            cancel_pending_trigger_reservation(
                reservation_coordinator.pending_reservations,
                trigger.id,
            );
            return;
        }
        TriggerConditionState::Triggered(triggered) => triggered,
    };

    if reservation_coordinator
        .pending_reservations
        .contains_key(&trigger.id)
    {
        return;
    }

    *reservation_coordinator.next_reservation_attempt_id =
        (*reservation_coordinator.next_reservation_attempt_id).saturating_add(1);
    reservation_coordinator.pending_reservations.insert(
        trigger.id,
        spawn_trigger_reservation(
            router.clone(),
            state.clone(),
            trigger.id,
            *reservation_coordinator.next_reservation_attempt_id,
            triggered.network_progress,
            PendingTriggerConditionPolicy {
                preserved_triggered: (!requires_revalidation).then_some(
                    TriggeredTriggerCondition {
                        evidence: triggered.evidence.clone(),
                        evidence_fingerprint: triggered.evidence_fingerprint.clone(),
                        network_progress: triggered.network_progress,
                    },
                ),
                requires_revalidation_after_queue: requires_revalidation,
                rule_semantics_fingerprint,
            },
            reservation_coordinator.reservation_tx.clone(),
        ),
    );
}

async fn handle_trigger_reservation_completion(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
    pending_reservations: &mut HashMap<u32, PendingTriggerReservation>,
    completion: TriggerReservationCompletion,
) {
    let Some(pending) = pending_reservations.remove(&completion.trigger_id) else {
        if let Ok(transaction) = completion.result {
            drop(transaction);
        }
        return;
    };
    if pending.attempt_id != completion.attempt_id {
        if let Ok(transaction) = completion.result {
            drop(transaction);
        }
        return;
    }

    let worker = match worker_state.get_mut(&completion.trigger_id) {
        Some(worker) => worker,
        None => {
            if let Ok(transaction) = completion.result {
                drop(transaction);
            }
            return;
        }
    };

    let browser = router.browser_port();
    let reserved = match completion.result {
        Ok(transaction) => match complete_trigger_reservation(
            state,
            &browser,
            completion.trigger_id,
            worker,
            transaction,
            pending.fallback_network_progress,
            pending.condition_policy,
        )
        .await
        {
            Ok(Some(reserved)) => reserved,
            Ok(None) => return,
            Err(envelope) => {
                if let Some(trigger) = state.trigger_rule(completion.trigger_id).await {
                    record_trigger_failure(state, &trigger, envelope, None, None).await;
                }
                return;
            }
        },
        Err(envelope) => {
            if state.is_shutdown_requested() {
                return;
            }
            if let Some(trigger) = state.trigger_rule(completion.trigger_id).await {
                record_trigger_failure(state, &trigger, envelope, None, None).await;
            }
            return;
        }
    };

    let command_id = trigger_action_command_id(&reserved.trigger, &reserved.evidence);
    match fire_trigger(
        router,
        state,
        &reserved.tabs,
        &reserved.trigger,
        &reserved.evidence,
        &command_id,
    )
    .await
    {
        Err(envelope) => {
            record_trigger_failure(
                state,
                &reserved.trigger,
                envelope,
                Some(reserved.evidence.clone()),
                Some(command_id),
            )
            .await;
            if let Some(worker) = worker_state.get_mut(&reserved.trigger.id) {
                commit_trigger_network_progress(worker, reserved.network_progress);
            }
        }
        Ok(result) => {
            let action_summary = trigger_action_summary(&reserved.trigger);
            let summary = format!(
                "trigger fired after {} and executed {} on target tab {}",
                reserved.evidence.summary, action_summary, reserved.trigger.target_tab.index
            );
            let _ = state
                .record_trigger_outcome(
                    reserved.trigger.id,
                    TriggerStatus::Fired,
                    Some(reserved.evidence),
                    TriggerResultInfo {
                        trigger_id: reserved.trigger.id,
                        status: TriggerStatus::Fired,
                        summary,
                        command_id: Some(command_id),
                        action: Some(trigger_action_execution_info(
                            &reserved.trigger,
                            &state.rub_home,
                        )),
                        result,
                        error_code: None,
                        reason: None,
                        consumed_evidence_fingerprint: Some(reserved.evidence_fingerprint),
                    },
                )
                .await;
            if let Some(worker) = worker_state.get_mut(&reserved.trigger.id) {
                commit_trigger_network_progress(worker, reserved.network_progress);
            }
        }
    }
}

fn cancel_pending_trigger_reservation(
    pending_reservations: &mut HashMap<u32, PendingTriggerReservation>,
    trigger_id: u32,
) {
    if let Some(pending) = pending_reservations.remove(&trigger_id) {
        pending.task.abort();
    }
}

fn reconcile_pending_trigger_reservations(
    triggers: &[TriggerInfo],
    pending_reservations: &mut HashMap<u32, PendingTriggerReservation>,
) {
    let live_fingerprints = triggers
        .iter()
        .filter(|trigger| matches!(trigger.status, TriggerStatus::Armed))
        .filter(|trigger| trigger.unavailable_reason.is_none())
        .map(|trigger| (trigger.id, trigger_rule_semantics_fingerprint(trigger)))
        .collect::<std::collections::HashMap<_, _>>();
    pending_reservations.retain(|trigger_id, pending| {
        let keep = live_fingerprints
            .get(trigger_id)
            .is_some_and(|fingerprint| {
                pending.condition_policy.requires_revalidation_after_queue
                    || pending.condition_policy.rule_semantics_fingerprint == *fingerprint
            });
        if keep {
            return true;
        }
        pending.task.abort();
        false
    });
}

fn abort_pending_trigger_reservations(
    pending_reservations: &mut HashMap<u32, PendingTriggerReservation>,
) {
    for (_, pending) in pending_reservations.drain() {
        pending.task.abort();
    }
}

#[cfg(test)]
mod tests;
