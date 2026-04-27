use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationMode, OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
    TriggerConditionKind, TriggerEvidenceInfo,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::warn;

use crate::orchestration_executor::{
    classify_orchestration_error_status, execute_orchestration_rule,
};
use crate::orchestration_probe::{
    dispatch_remote_orchestration_probe, evaluate_orchestration_probe_for_tab,
};
use crate::router::{
    DaemonRouter, OwnedRouterTransactionGuard, RouterFenceDisposition,
    handoff_blocked_error_for_command,
};
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::scheduler_policy::AUTOMATION_WORKER_POLL_INTERVAL;
use crate::session::SessionState;

mod condition;

pub(crate) use condition::orchestration_evidence_key;
use condition::{
    commit_orchestration_network_progress, load_orchestration_condition_state,
    orchestration_rule_in_cooldown, reconcile_worker_state,
    record_orchestration_failure_with_fallback, record_orchestration_probe_failure,
    should_persist_orchestration_evidence_latch, should_retain_orchestration_evidence_latch,
    skip_latched_orchestration_evidence,
};

const ORCHESTRATION_WORKER_INTERVAL: Duration = AUTOMATION_WORKER_POLL_INTERVAL;

#[derive(Debug, Clone)]
struct OrchestrationWorkerEntry {
    last_status: OrchestrationRuleStatus,
    network_cursor: u64,
    network_cursor_primed: bool,
    observatory_drop_count: u64,
    latched_evidence_key: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct OrchestrationNetworkProgress {
    next_cursor: u64,
    observed_drop_count: u64,
}

struct OrchestrationConditionEvaluation {
    evidence: Option<TriggerEvidenceInfo>,
    network_progress: Option<OrchestrationNetworkProgress>,
}

enum OrchestrationConditionState {
    NotTriggered {
        network_progress: Option<OrchestrationNetworkProgress>,
    },
    Triggered(TriggeredOrchestrationCondition),
}

#[derive(Clone)]
struct TriggeredOrchestrationCondition {
    evidence: TriggerEvidenceInfo,
    evidence_key: String,
    network_progress: Option<OrchestrationNetworkProgress>,
}

struct ReservedOrchestrationExecution {
    runtime: OrchestrationRuntimeInfo,
    rule: OrchestrationRuleInfo,
    evidence: TriggerEvidenceInfo,
    evidence_key: String,
    network_progress: Option<OrchestrationNetworkProgress>,
    _transaction: Option<OwnedRouterTransactionGuard>,
}

struct CompletedOrchestrationReservation {
    rule_id: u32,
    attempt_id: u64,
    result: Result<OwnedRouterTransactionGuard, ErrorEnvelope>,
}

struct PendingOrchestrationConditionPolicy {
    preserved_triggered: Option<TriggeredOrchestrationCondition>,
    requires_revalidation_after_queue: bool,
    rule_semantics_fingerprint: String,
    rule_lifecycle_generation: u64,
}

struct PendingOrchestrationReservation {
    attempt_id: u64,
    fallback_network_progress: Option<OrchestrationNetworkProgress>,
    condition_policy: PendingOrchestrationConditionPolicy,
    task: JoinHandle<()>,
}

pub(crate) fn spawn_orchestration_worker(
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
    shutdown: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ORCHESTRATION_WORKER_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut worker_state = HashMap::<u32, OrchestrationWorkerEntry>::new();
        let (reservation_tx, mut reservation_rx) =
            tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
        let mut pending_reservations = HashMap::<u32, PendingOrchestrationReservation>::new();
        let mut next_reservation_attempt_id = 0_u64;

        loop {
            if state.is_shutdown_requested() {
                abort_pending_orchestration_reservations(&mut pending_reservations);
                break;
            }
            tokio::select! {
                _ = shutdown.notified() => {
                    abort_pending_orchestration_reservations(&mut pending_reservations);
                    break;
                }
                _ = ticker.tick() => {
                    if state.is_shutdown_requested() {
                        abort_pending_orchestration_reservations(&mut pending_reservations);
                        break;
                    }
                    run_orchestration_cycle(
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
                    handle_orchestration_reservation_completion(
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

async fn run_orchestration_cycle(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, OrchestrationWorkerEntry>,
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
    reservation_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CompletedOrchestrationReservation>,
    reservation_tx: &tokio::sync::mpsc::UnboundedSender<CompletedOrchestrationReservation>,
    next_reservation_attempt_id: &mut u64,
) {
    // Queue admission is the authoritative fairness boundary; orchestration work
    // should contend there instead of being pre-emptively gated by in_flight_count.
    state.record_orchestration_worker_cycle_started();

    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    if !runtime.execution_supported {
        abort_pending_orchestration_reservations(pending_reservations);
        return;
    }
    if runtime.rules.is_empty() {
        abort_pending_orchestration_reservations(pending_reservations);
        worker_state.clear();
        return;
    }

    let committed_baselines = state.orchestration_network_request_baselines().await;
    reconcile_worker_state(worker_state, &runtime.rules, &committed_baselines);
    reconcile_pending_orchestration_reservations(&runtime.rules, pending_reservations);
    drain_orchestration_reservation_completions(
        router,
        state,
        worker_state,
        pending_reservations,
        reservation_rx,
    )
    .await;

    for rule in runtime.rules.clone() {
        drain_orchestration_reservation_completions(
            router,
            state,
            worker_state,
            pending_reservations,
            reservation_rx,
        )
        .await;
        if !matches!(rule.status, OrchestrationRuleStatus::Armed)
            || rule.unavailable_reason.is_some()
            || orchestration_rule_in_cooldown(&rule)
        {
            cancel_pending_orchestration_reservation(pending_reservations, rule.id);
            continue;
        }

        let Some(worker_entry) = worker_state.get_mut(&rule.id) else {
            continue;
        };
        process_orchestration_rule(
            router,
            state,
            rule,
            worker_entry,
            pending_reservations,
            reservation_tx,
            next_reservation_attempt_id,
        )
        .await;
        drain_orchestration_reservation_completions(
            router,
            state,
            worker_state,
            pending_reservations,
            reservation_rx,
        )
        .await;
    }
}

async fn drain_orchestration_reservation_completions(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, OrchestrationWorkerEntry>,
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
    reservation_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CompletedOrchestrationReservation>,
) {
    loop {
        match reservation_rx.try_recv() {
            Ok(completion) => {
                handle_orchestration_reservation_completion(
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

fn orchestration_condition_requires_revalidation_after_queue(rule: &OrchestrationRuleInfo) -> bool {
    !matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
}

fn orchestration_rule_semantics_fingerprint(rule: &OrchestrationRuleInfo) -> String {
    serde_json::json!({
        "source": {
            "session_id": rule.source.session_id,
            "tab_target_id": rule.source.tab_target_id,
            "frame_id": rule.source.frame_id,
        },
        "target": {
            "session_id": rule.target.session_id,
            "tab_target_id": rule.target.tab_target_id,
            "frame_id": rule.target.frame_id,
        },
        "mode": rule.mode,
        "execution_policy": rule.execution_policy,
        "condition": rule.condition,
        "actions": rule.actions,
        "correlation_key": rule.correlation_key,
        "idempotency_key": rule.idempotency_key,
    })
    .to_string()
}

async fn process_orchestration_rule(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    rule: OrchestrationRuleInfo,
    worker_entry: &mut OrchestrationWorkerEntry,
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
    reservation_tx: &tokio::sync::mpsc::UnboundedSender<CompletedOrchestrationReservation>,
    next_reservation_attempt_id: &mut u64,
) {
    let requires_revalidation = orchestration_condition_requires_revalidation_after_queue(&rule);
    let rule_semantics_fingerprint = orchestration_rule_semantics_fingerprint(&rule);
    if let Some(pending) = pending_reservations.get(&rule.id)
        && !requires_revalidation
    {
        if pending.condition_policy.rule_semantics_fingerprint == rule_semantics_fingerprint {
            return;
        }
        cancel_pending_orchestration_reservation(pending_reservations, rule.id);
    }

    let condition =
        match load_orchestration_condition_state(router, state, &rule, worker_entry).await {
            Ok(condition) => condition,
            Err(envelope) => {
                record_orchestration_probe_failure(state, &rule, envelope).await;
                return;
            }
        };

    let triggered = match condition {
        OrchestrationConditionState::NotTriggered { network_progress } => {
            worker_entry.latched_evidence_key = None;
            if should_persist_orchestration_evidence_latch(&rule)
                && rule.last_condition_evidence.is_some()
            {
                state
                    .set_orchestration_condition_evidence(
                        rule.id,
                        Some(rule.lifecycle_generation),
                        None,
                    )
                    .await;
            }
            commit_orchestration_network_progress(worker_entry, network_progress);
            cancel_pending_orchestration_reservation(pending_reservations, rule.id);
            return;
        }
        OrchestrationConditionState::Triggered(triggered) => triggered,
    };

    if skip_latched_orchestration_evidence(
        worker_entry,
        &triggered.evidence_key,
        triggered.network_progress,
    ) {
        return;
    }

    if pending_reservations.contains_key(&rule.id) {
        return;
    }

    *next_reservation_attempt_id = next_reservation_attempt_id.saturating_add(1);
    pending_reservations.insert(
        rule.id,
        spawn_orchestration_reservation(
            router.clone(),
            state.clone(),
            rule.id,
            *next_reservation_attempt_id,
            triggered.network_progress,
            PendingOrchestrationConditionPolicy {
                preserved_triggered: (!requires_revalidation).then_some(triggered.clone()),
                requires_revalidation_after_queue: requires_revalidation,
                rule_semantics_fingerprint,
                rule_lifecycle_generation: rule.lifecycle_generation,
            },
            reservation_tx.clone(),
        ),
    );
}

fn spawn_orchestration_reservation(
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
    rule_id: u32,
    attempt_id: u64,
    fallback_network_progress: Option<OrchestrationNetworkProgress>,
    condition_policy: PendingOrchestrationConditionPolicy,
    reservation_tx: tokio::sync::mpsc::UnboundedSender<CompletedOrchestrationReservation>,
) -> PendingOrchestrationReservation {
    let task = tokio::spawn(async move {
        let result = router
            .begin_automation_reservation_transaction_owned(&state, "orchestration_worker")
            .await;
        let _ = reservation_tx.send(CompletedOrchestrationReservation {
            rule_id,
            attempt_id,
            result,
        });
    });
    PendingOrchestrationReservation {
        attempt_id,
        fallback_network_progress,
        condition_policy,
        task,
    }
}

async fn handle_orchestration_reservation_completion(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, OrchestrationWorkerEntry>,
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
    completion: CompletedOrchestrationReservation,
) {
    let Some(pending) = pending_reservations.remove(&completion.rule_id) else {
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

    let Some(worker_entry) = worker_state.get_mut(&completion.rule_id) else {
        if let Ok(transaction) = completion.result {
            drop(transaction);
        }
        return;
    };

    let reserved = match completion.result {
        Ok(transaction) => match complete_orchestration_reservation(
            router,
            state,
            completion.rule_id,
            worker_entry,
            transaction,
            pending.fallback_network_progress,
            pending.condition_policy,
        )
        .await
        {
            Ok(Some(reserved)) => reserved,
            Ok(None) => return,
            Err(envelope) => {
                if let Some(rule) = state
                    .orchestration_runtime()
                    .await
                    .rules
                    .into_iter()
                    .find(|candidate| candidate.id == completion.rule_id)
                {
                    record_orchestration_probe_failure(state, &rule, envelope).await;
                }
                return;
            }
        },
        Err(envelope) => {
            if state.is_shutdown_requested() {
                return;
            }
            if let Some(rule) = state
                .orchestration_runtime()
                .await
                .rules
                .into_iter()
                .find(|candidate| candidate.id == completion.rule_id)
            {
                record_orchestration_probe_failure(state, &rule, envelope).await;
            }
            return;
        }
    };

    if state.is_shutdown_requested() {
        commit_orchestration_network_progress(worker_entry, reserved.network_progress);
        return;
    }
    if let Some(error) = handoff_blocked_error_for_command("orchestration_worker", state).await {
        record_orchestration_failure_with_fallback(
            state,
            &reserved.rule,
            error,
            Some(reserved.evidence.clone()),
        )
        .await;
        commit_orchestration_network_progress(worker_entry, reserved.network_progress);
        return;
    }

    let result = execute_orchestration_rule(
        router,
        state,
        &reserved.runtime,
        &reserved.rule,
        Some(reserved.evidence_key.as_str()),
        None,
        RouterFenceDisposition::ReuseCurrentTransaction,
    )
    .await;
    commit_orchestration_execution(state, worker_entry, reserved, result).await;
}

async fn complete_orchestration_reservation(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    rule_id: u32,
    worker_entry: &mut OrchestrationWorkerEntry,
    transaction: OwnedRouterTransactionGuard,
    fallback_network_progress: Option<OrchestrationNetworkProgress>,
    condition_policy: PendingOrchestrationConditionPolicy,
) -> Result<Option<ReservedOrchestrationExecution>, ErrorEnvelope> {
    refresh_orchestration_runtime(state).await;
    let latest_runtime = state.orchestration_runtime().await;
    let Some(latest_rule) = latest_runtime
        .rules
        .iter()
        .find(|candidate| candidate.id == rule_id)
        .cloned()
    else {
        drop(transaction);
        return Ok(None);
    };
    if !matches!(latest_rule.status, OrchestrationRuleStatus::Armed)
        || latest_rule.unavailable_reason.is_some()
        || orchestration_rule_in_cooldown(&latest_rule)
    {
        drop(transaction);
        return Ok(None);
    }

    let live_requires_revalidation =
        orchestration_condition_requires_revalidation_after_queue(&latest_rule);
    if live_requires_revalidation != condition_policy.requires_revalidation_after_queue {
        drop(transaction);
        return Ok(None);
    }
    if !live_requires_revalidation
        && orchestration_rule_semantics_fingerprint(&latest_rule)
            != condition_policy.rule_semantics_fingerprint
    {
        drop(transaction);
        return Ok(None);
    }
    if latest_rule.lifecycle_generation != condition_policy.rule_lifecycle_generation {
        drop(transaction);
        return Ok(None);
    }

    let triggered = if live_requires_revalidation {
        match load_orchestration_condition_state(router, state, &latest_rule, worker_entry).await? {
            OrchestrationConditionState::Triggered(triggered) => triggered,
            OrchestrationConditionState::NotTriggered { .. } => {
                worker_entry.latched_evidence_key = None;
                drop(transaction);
                return Ok(None);
            }
        }
    } else {
        let Some(preserved_triggered) = condition_policy.preserved_triggered else {
            drop(transaction);
            return Err(ErrorEnvelope::new(
                ErrorCode::InternalError,
                "orchestration reservation lost preserved network_request evidence before queue completion",
            ));
        };
        preserved_triggered
    };

    Ok(Some(ReservedOrchestrationExecution {
        runtime: latest_runtime,
        rule: latest_rule,
        evidence: triggered.evidence,
        evidence_key: triggered.evidence_key,
        network_progress: triggered.network_progress.or(fallback_network_progress),
        _transaction: Some(transaction),
    }))
}

async fn commit_orchestration_execution(
    state: &Arc<SessionState>,
    worker_entry: &mut OrchestrationWorkerEntry,
    reserved: ReservedOrchestrationExecution,
    result: rub_core::model::OrchestrationResultInfo,
) {
    state
        .record_orchestration_outcome_with_fallback(
            &reserved.rule,
            Some(reserved.rule.lifecycle_generation),
            Some(reserved.evidence.clone()),
            result.clone(),
        )
        .await;
    commit_orchestration_network_progress(worker_entry, reserved.network_progress);
    worker_entry.latched_evidence_key =
        should_retain_orchestration_evidence_latch(&reserved.rule, &result)
            .then_some(reserved.evidence_key);
}

fn cancel_pending_orchestration_reservation(
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
    rule_id: u32,
) {
    if let Some(pending) = pending_reservations.remove(&rule_id) {
        pending.task.abort();
    }
}

fn reconcile_pending_orchestration_reservations(
    rules: &[OrchestrationRuleInfo],
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
) {
    let live_fingerprints = rules
        .iter()
        .filter(|rule| matches!(rule.status, OrchestrationRuleStatus::Armed))
        .filter(|rule| rule.unavailable_reason.is_none())
        .filter(|rule| !orchestration_rule_in_cooldown(rule))
        .map(|rule| (rule.id, orchestration_rule_semantics_fingerprint(rule)))
        .collect::<std::collections::HashMap<_, _>>();
    pending_reservations.retain(|rule_id, pending| {
        let keep = live_fingerprints.get(rule_id).is_some_and(|fingerprint| {
            pending.condition_policy.rule_lifecycle_generation
                == rules
                    .iter()
                    .find(|rule| rule.id == *rule_id)
                    .map(|rule| rule.lifecycle_generation)
                    .unwrap_or_default()
                && (pending.condition_policy.requires_revalidation_after_queue
                    || pending.condition_policy.rule_semantics_fingerprint == *fingerprint)
        });
        if keep {
            return true;
        }
        pending.task.abort();
        false
    });
}

fn abort_pending_orchestration_reservations(
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
) {
    for (_, pending) in pending_reservations.drain() {
        pending.task.abort();
    }
}

#[cfg(test)]
mod tests;
