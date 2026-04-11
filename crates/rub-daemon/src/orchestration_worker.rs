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
use crate::router::{DaemonRouter, OwnedRouterTransactionGuard};
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::scheduler_policy::AUTOMATION_WORKER_POLL_INTERVAL;
use crate::session::SessionState;

mod condition;

use condition::{
    commit_orchestration_network_progress, load_orchestration_condition_state,
    orchestration_rule_in_cooldown, reconcile_worker_state, record_orchestration_probe_failure,
    should_persist_orchestration_evidence_latch, skip_latched_orchestration_evidence,
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

    let active_request_cursor = state.network_request_cursor().await;
    let observatory_drop_count = state.network_request_drop_count().await;
    reconcile_worker_state(
        worker_state,
        &runtime.rules,
        active_request_cursor,
        observatory_drop_count,
        &state.session_id,
    );
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
                refresh_orchestration_runtime(state).await;
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
                    .set_orchestration_condition_evidence(rule.id, None)
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
            .begin_automation_transaction_until_shutdown_owned(&state, "orchestration_worker")
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
                    refresh_orchestration_runtime(state).await;
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
                refresh_orchestration_runtime(state).await;
            }
            return;
        }
    };

    let result = execute_orchestration_rule(router, state, &reserved.runtime, &reserved.rule).await;
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

    let target_is_local = latest_rule.target.session_id == state.session_id;
    Ok(Some(ReservedOrchestrationExecution {
        runtime: latest_runtime,
        rule: latest_rule,
        evidence: triggered.evidence,
        evidence_key: triggered.evidence_key,
        network_progress: triggered.network_progress.or(fallback_network_progress),
        _transaction: target_is_local.then_some(transaction),
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
            Some(reserved.evidence.clone()),
            result.clone(),
        )
        .await;
    commit_orchestration_network_progress(worker_entry, reserved.network_progress);
    worker_entry.latched_evidence_key = match result.next_status {
        OrchestrationRuleStatus::Armed => Some(reserved.evidence_key),
        _ => None,
    };
    refresh_orchestration_runtime(state).await;
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

fn abort_pending_orchestration_reservations(
    pending_reservations: &mut HashMap<u32, PendingOrchestrationReservation>,
) {
    for (_, pending) in pending_reservations.drain() {
        pending.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::condition::{
        orchestration_evidence_key, persisted_latched_orchestration_evidence_key,
        reconcile_worker_state, skip_latched_orchestration_evidence,
    };
    use super::{
        CompletedOrchestrationReservation, OrchestrationNetworkProgress, OrchestrationWorkerEntry,
        PendingOrchestrationConditionPolicy, PendingOrchestrationReservation,
        TriggeredOrchestrationCondition, complete_orchestration_reservation,
        drain_orchestration_reservation_completions, orchestration_rule_semantics_fingerprint,
        process_orchestration_rule, reconcile_pending_orchestration_reservations,
        run_orchestration_cycle,
    };
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
        OrchestrationRuleInfo, OrchestrationRuleStatus, TriggerConditionKind, TriggerConditionSpec,
        TriggerEvidenceInfo,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use uuid::Uuid;

    use crate::router::DaemonRouter;
    use crate::session::SessionState;

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
        std::env::temp_dir().join(format!(
            "rub-orchestration-worker-{label}-{}",
            Uuid::now_v7()
        ))
    }

    fn rule(id: u32, status: OrchestrationRuleStatus) -> OrchestrationRuleInfo {
        OrchestrationRuleInfo {
            id,
            status,
            source: OrchestrationAddressInfo {
                session_id: "source-session".to_string(),
                session_name: "source".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("source-tab".to_string()),
                frame_id: None,
            },
            target: OrchestrationAddressInfo {
                session_id: "target-session".to_string(),
                session_name: "target".to_string(),
                tab_index: Some(0),
                tab_target_id: Some("target-tab".to_string()),
                frame_id: None,
            },
            mode: OrchestrationMode::Repeat,
            execution_policy: OrchestrationExecutionPolicyInfo {
                cooldown_ms: 1000,
                max_retries: 0,
                cooldown_until_ms: None,
            },
            condition: TriggerConditionSpec {
                kind: TriggerConditionKind::TextPresent,
                locator: None,
                text: Some("Ready".to_string()),
                url_pattern: None,
                readiness_state: None,
                method: None,
                status_code: None,
                storage_area: None,
                key: None,
                value: None,
            },
            actions: vec![rub_core::model::TriggerActionSpec {
                kind: rub_core::model::TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(json!({"selector":"#apply"})),
            }],
            correlation_key: "corr".to_string(),
            idempotency_key: format!("idem-{id}"),
            unavailable_reason: None,
            last_condition_evidence: None,
            last_result: None,
        }
    }

    #[test]
    fn orchestration_evidence_key_prefers_fingerprint_when_present() {
        let evidence = TriggerEvidenceInfo {
            summary: "source_tab_text_present:Ready".to_string(),
            fingerprint: Some("doc-1".to_string()),
        };
        assert_eq!(
            orchestration_evidence_key(&evidence),
            "source_tab_text_present:Ready::doc-1"
        );
    }

    #[test]
    fn reconcile_worker_state_clears_latched_evidence_when_rule_rearms() {
        let mut worker_state = HashMap::from([(
            7,
            OrchestrationWorkerEntry {
                last_status: OrchestrationRuleStatus::Blocked,
                network_cursor: 4,
                network_cursor_primed: true,
                observatory_drop_count: 0,
                latched_evidence_key: Some("source_tab_text_present:Ready".to_string()),
            },
        )]);
        reconcile_worker_state(
            &mut worker_state,
            &[rule(7, OrchestrationRuleStatus::Armed)],
            11,
            0,
            "source-session",
        );
        let entry = worker_state.get(&7).expect("entry should exist");
        assert_eq!(entry.network_cursor, 11);
        assert!(entry.network_cursor_primed);
        assert_eq!(entry.latched_evidence_key, None);
    }

    #[test]
    fn reconcile_worker_state_leaves_remote_network_rules_unprimed_until_remote_cursor_is_read() {
        let mut worker_state = HashMap::new();
        let mut remote_rule = rule(8, OrchestrationRuleStatus::Armed);
        remote_rule.condition.kind = TriggerConditionKind::NetworkRequest;

        reconcile_worker_state(&mut worker_state, &[remote_rule], 17, 0, "current-session");

        let entry = worker_state.get(&8).expect("entry should exist");
        assert_eq!(entry.network_cursor, 0);
        assert!(!entry.network_cursor_primed);
    }

    #[test]
    fn latched_evidence_still_commits_network_progress() {
        let mut worker = OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Armed,
            network_cursor: 4,
            network_cursor_primed: true,
            observatory_drop_count: 1,
            latched_evidence_key: Some("same-evidence".to_string()),
        };

        assert!(skip_latched_orchestration_evidence(
            &mut worker,
            "same-evidence",
            Some(OrchestrationNetworkProgress {
                next_cursor: 9,
                observed_drop_count: 3,
            })
        ));
        assert_eq!(worker.network_cursor, 9);
        assert_eq!(worker.observatory_drop_count, 3);
        assert!(worker.network_cursor_primed);
    }

    #[test]
    fn persisted_repeat_evidence_latch_survives_manual_cooldown_projection() {
        let mut repeat_rule = rule(9, OrchestrationRuleStatus::Armed);
        repeat_rule.last_condition_evidence = Some(TriggerEvidenceInfo {
            summary: "source_tab_text_present:Ready".to_string(),
            fingerprint: Some("Ready".to_string()),
        });
        repeat_rule.last_result = Some(rub_core::model::OrchestrationResultInfo {
            rule_id: 9,
            status: OrchestrationRuleStatus::Blocked,
            next_status: OrchestrationRuleStatus::Armed,
            summary: "orchestration cooldown active".to_string(),
            committed_steps: 0,
            total_steps: 1,
            steps: Vec::new(),
            cooldown_until_ms: Some(1234),
            error_code: None,
            reason: Some("orchestration_cooldown_active".to_string()),
        });

        assert_eq!(
            persisted_latched_orchestration_evidence_key(&repeat_rule),
            Some("source_tab_text_present:Ready::Ready".to_string())
        );
    }

    #[test]
    fn reconcile_worker_state_seeds_latch_from_persisted_repeat_evidence() {
        let mut worker_state = HashMap::new();
        let mut repeat_rule = rule(10, OrchestrationRuleStatus::Armed);
        repeat_rule.last_condition_evidence = Some(TriggerEvidenceInfo {
            summary: "source_tab_text_present:Ready".to_string(),
            fingerprint: Some("Ready".to_string()),
        });
        repeat_rule.last_result = Some(rub_core::model::OrchestrationResultInfo {
            rule_id: 10,
            status: OrchestrationRuleStatus::Fired,
            next_status: OrchestrationRuleStatus::Armed,
            summary: "repeat orchestration rule 10 committed 1/1 action(s)".to_string(),
            committed_steps: 1,
            total_steps: 1,
            steps: Vec::new(),
            cooldown_until_ms: Some(1234),
            error_code: None,
            reason: None,
        });

        reconcile_worker_state(&mut worker_state, &[repeat_rule], 17, 0, "source-session");

        let entry = worker_state.get(&10).expect("entry should exist");
        assert_eq!(
            entry.latched_evidence_key.as_deref(),
            Some("source_tab_text_present:Ready::Ready")
        );
    }

    #[tokio::test]
    async fn orchestration_cycle_uses_queue_authority_even_with_foreground_in_flight() {
        let router = test_router();
        let state = Arc::new(SessionState::new("default", temp_home("fairness"), None));
        state
            .in_flight_count
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let mut worker_state = HashMap::new();
        let mut pending_reservations = HashMap::new();
        let (reservation_tx, mut reservation_rx) =
            tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
        let mut next_reservation_attempt_id = 0_u64;

        run_orchestration_cycle(
            &router,
            &state,
            &mut worker_state,
            &mut pending_reservations,
            &mut reservation_rx,
            &reservation_tx,
            &mut next_reservation_attempt_id,
        )
        .await;

        let metrics = state.automation_scheduler_metrics().await;
        assert_eq!(
            metrics["orchestration_worker"]["metrics"]["cycle_count"],
            json!(1)
        );
        assert_eq!(
            metrics["authority_inventory"]["orchestration_worker_pre_queue_gate"],
            json!("none")
        );
    }

    #[tokio::test]
    async fn ready_orchestration_reservation_completion_releases_idle_queue_permit() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("reservation-completion-release"),
            None,
        ));
        let reserved = router
            .begin_automation_transaction_until_shutdown_owned(&state, "queued_orchestration")
            .await
            .expect("queued orchestration reservation should acquire immediately in test");
        let mut worker_state = HashMap::new();
        let mut pending_reservations = HashMap::from([(
            7_u32,
            PendingOrchestrationReservation {
                attempt_id: 1,
                fallback_network_progress: None,
                condition_policy: PendingOrchestrationConditionPolicy {
                    preserved_triggered: None,
                    requires_revalidation_after_queue: true,
                    rule_semantics_fingerprint: String::new(),
                },
                task: tokio::spawn(async {}),
            },
        )]);
        let (reservation_tx, mut reservation_rx) =
            tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
        reservation_tx
            .send(CompletedOrchestrationReservation {
                rule_id: 7,
                attempt_id: 1,
                result: Ok(reserved),
            })
            .expect("reservation completion should enqueue");

        drain_orchestration_reservation_completions(
            &router,
            &state,
            &mut worker_state,
            &mut pending_reservations,
            &mut reservation_rx,
        )
        .await;

        assert!(pending_reservations.is_empty());
        let foreground = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            router.begin_automation_transaction_with_wait_budget(
                &state,
                "foreground_after_completion",
                std::time::Duration::from_secs(1),
                std::time::Duration::from_millis(5),
            ),
        )
        .await
        .expect("foreground request should not remain blocked behind drained completion")
        .expect("foreground request should acquire after drained completion");
        drop(foreground);
    }

    #[tokio::test]
    async fn pending_network_request_orchestration_is_not_re_evaluated_during_queue_wait() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("pending-network-request"),
            None,
        ));
        let mut worker_entry = OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Armed,
            network_cursor: 0,
            network_cursor_primed: true,
            observatory_drop_count: 0,
            latched_evidence_key: None,
        };
        let mut network_rule = rule(7, OrchestrationRuleStatus::Armed);
        network_rule.condition.kind = TriggerConditionKind::NetworkRequest;
        let mut pending_reservations = HashMap::from([(
            7_u32,
            PendingOrchestrationReservation {
                attempt_id: 1,
                fallback_network_progress: None,
                condition_policy: PendingOrchestrationConditionPolicy {
                    preserved_triggered: None,
                    requires_revalidation_after_queue: false,
                    rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(
                        &network_rule,
                    ),
                },
                task: tokio::spawn(async {}),
            },
        )]);
        let (reservation_tx, _reservation_rx) =
            tokio::sync::mpsc::unbounded_channel::<CompletedOrchestrationReservation>();
        let mut next_reservation_attempt_id = 0_u64;

        process_orchestration_rule(
            &router,
            &state,
            network_rule.clone(),
            &mut worker_entry,
            &mut pending_reservations,
            &reservation_tx,
            &mut next_reservation_attempt_id,
        )
        .await;

        assert!(pending_reservations.contains_key(&network_rule.id));
        assert_eq!(next_reservation_attempt_id, 0);
    }

    #[tokio::test]
    async fn reconcile_pending_network_request_orchestration_drops_semantics_drift() {
        let mut stale_rule = rule(7, OrchestrationRuleStatus::Armed);
        stale_rule.condition.kind = TriggerConditionKind::NetworkRequest;
        stale_rule.condition.url_pattern = Some("/old".to_string());
        let mut live_rule = stale_rule.clone();
        live_rule.condition.url_pattern = Some("/new".to_string());

        let mut pending_reservations = HashMap::from([(
            live_rule.id,
            PendingOrchestrationReservation {
                attempt_id: 1,
                fallback_network_progress: None,
                condition_policy: PendingOrchestrationConditionPolicy {
                    preserved_triggered: None,
                    requires_revalidation_after_queue: false,
                    rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(
                        &stale_rule,
                    ),
                },
                task: tokio::spawn(async {}),
            },
        )]);

        reconcile_pending_orchestration_reservations(&[live_rule], &mut pending_reservations);

        assert!(pending_reservations.is_empty());
    }

    #[tokio::test]
    async fn complete_network_request_orchestration_reservation_fails_closed_on_semantics_drift() {
        let router = test_router();
        let state = Arc::new(SessionState::new(
            "default",
            temp_home("reservation-semantics-drift"),
            None,
        ));
        let mut live_rule = rule(7, OrchestrationRuleStatus::Armed);
        live_rule.condition.kind = TriggerConditionKind::NetworkRequest;
        live_rule.condition.url_pattern = Some("/new".to_string());
        let live_rule = state
            .register_orchestration_rule(live_rule)
            .await
            .expect("rule should register");
        let transaction = router
            .begin_automation_transaction_until_shutdown_owned(&state, "queued_orchestration")
            .await
            .expect("reservation should acquire");
        let mut worker_entry = OrchestrationWorkerEntry {
            last_status: OrchestrationRuleStatus::Armed,
            network_cursor: 0,
            network_cursor_primed: true,
            observatory_drop_count: 0,
            latched_evidence_key: None,
        };
        let mut stale_rule = live_rule.clone();
        stale_rule.condition.url_pattern = Some("/old".to_string());

        let reserved = complete_orchestration_reservation(
            &router,
            &state,
            live_rule.id,
            &mut worker_entry,
            transaction,
            None,
            PendingOrchestrationConditionPolicy {
                preserved_triggered: Some(TriggeredOrchestrationCondition {
                    evidence: TriggerEvidenceInfo {
                        summary: "network_request_matched:req-1".to_string(),
                        fingerprint: Some("req-1".to_string()),
                    },
                    evidence_key: "network_request_matched:req-1::req-1".to_string(),
                    network_progress: None,
                }),
                requires_revalidation_after_queue: false,
                rule_semantics_fingerprint: orchestration_rule_semantics_fingerprint(&stale_rule),
            },
        )
        .await
        .expect("reservation completion should fail closed, not error");

        assert!(reserved.is_none());
    }
}
