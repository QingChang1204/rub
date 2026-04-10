use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
use crate::router::{DaemonRouter, RouterTransactionGuard};
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::session::SessionState;

mod condition;

use condition::{
    commit_orchestration_network_progress, load_orchestration_condition_state,
    orchestration_rule_in_cooldown, reconcile_worker_state, record_orchestration_probe_failure,
    should_persist_orchestration_evidence_latch, skip_latched_orchestration_evidence,
};

const ORCHESTRATION_WORKER_INTERVAL: Duration = Duration::from_millis(500);
const ORCHESTRATION_AUTOMATION_TRANSACTION_TIMEOUT_MS: u64 = 100;

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

struct TriggeredOrchestrationCondition {
    evidence: TriggerEvidenceInfo,
    evidence_key: String,
    network_progress: Option<OrchestrationNetworkProgress>,
}

struct ReservedOrchestrationExecution<'a> {
    runtime: OrchestrationRuntimeInfo,
    rule: OrchestrationRuleInfo,
    evidence: TriggerEvidenceInfo,
    evidence_key: String,
    network_progress: Option<OrchestrationNetworkProgress>,
    _transaction: Option<RouterTransactionGuard<'a>>,
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

        loop {
            if state.is_shutdown_requested() {
                break;
            }
            tokio::select! {
                _ = shutdown.notified() => break,
                _ = ticker.tick() => {
                    if state.is_shutdown_requested() {
                        break;
                    }
                    run_orchestration_cycle(&router, &state, &mut worker_state).await;
                }
            }
        }
    })
}

async fn run_orchestration_cycle(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, OrchestrationWorkerEntry>,
) {
    if state.in_flight_count.load(Ordering::SeqCst) > 0 {
        return;
    }

    refresh_orchestration_runtime(state).await;
    let runtime = state.orchestration_runtime().await;
    if !runtime.execution_supported {
        return;
    }
    if runtime.rules.is_empty() {
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

    for rule in runtime.rules.clone() {
        if !matches!(rule.status, OrchestrationRuleStatus::Armed)
            || rule.unavailable_reason.is_some()
            || orchestration_rule_in_cooldown(&rule)
        {
            continue;
        }

        let Some(worker_entry) = worker_state.get_mut(&rule.id) else {
            continue;
        };
        process_orchestration_rule(router, state, rule, worker_entry).await;
    }
}

fn orchestration_condition_requires_revalidation_after_queue(rule: &OrchestrationRuleInfo) -> bool {
    !matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
}

async fn process_orchestration_rule(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    rule: OrchestrationRuleInfo,
    worker_entry: &mut OrchestrationWorkerEntry,
) {
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

    let reserved = match reserve_orchestration_execution(
        router,
        state,
        &rule,
        worker_entry,
        triggered,
    )
    .await
    {
        Ok(Some(reserved)) => reserved,
        Ok(None) => return,
        Err(envelope) => {
            record_orchestration_probe_failure(state, &rule, envelope).await;
            refresh_orchestration_runtime(state).await;
            return;
        }
    };

    let result = execute_orchestration_rule(router, state, &reserved.runtime, &reserved.rule).await;
    commit_orchestration_execution(state, worker_entry, reserved, result).await;
}

async fn reserve_orchestration_execution<'a>(
    router: &'a Arc<DaemonRouter>,
    state: &'a Arc<SessionState>,
    rule: &OrchestrationRuleInfo,
    worker_entry: &mut OrchestrationWorkerEntry,
    triggered: TriggeredOrchestrationCondition,
) -> Result<Option<ReservedOrchestrationExecution<'a>>, ErrorEnvelope> {
    let transaction = match router
        .begin_automation_transaction(
            state,
            ORCHESTRATION_AUTOMATION_TRANSACTION_TIMEOUT_MS,
            "orchestration_worker",
        )
        .await
    {
        Ok(transaction) => transaction,
        Err(_) => return Ok(None),
    };

    refresh_orchestration_runtime(state).await;
    let latest_runtime = state.orchestration_runtime().await;
    let Some(latest_rule) = latest_runtime
        .rules
        .iter()
        .find(|candidate| candidate.id == rule.id)
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

    let mut triggered = triggered;
    if orchestration_condition_requires_revalidation_after_queue(&latest_rule) {
        triggered =
            match load_orchestration_condition_state(router, state, &latest_rule, worker_entry)
                .await?
            {
                OrchestrationConditionState::Triggered(triggered) => triggered,
                OrchestrationConditionState::NotTriggered { .. } => {
                    worker_entry.latched_evidence_key = None;
                    drop(transaction);
                    return Ok(None);
                }
            };
    }

    let target_is_local = latest_rule.target.session_id == state.session_id;
    Ok(Some(ReservedOrchestrationExecution {
        runtime: latest_runtime,
        rule: latest_rule,
        evidence: triggered.evidence,
        evidence_key: triggered.evidence_key,
        network_progress: triggered.network_progress,
        _transaction: target_is_local.then_some(transaction),
    }))
}

async fn commit_orchestration_execution(
    state: &Arc<SessionState>,
    worker_entry: &mut OrchestrationWorkerEntry,
    reserved: ReservedOrchestrationExecution<'_>,
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

#[cfg(test)]
mod tests {
    use super::condition::{
        orchestration_evidence_key, persisted_latched_orchestration_evidence_key,
        reconcile_worker_state, skip_latched_orchestration_evidence,
    };
    use super::{OrchestrationNetworkProgress, OrchestrationWorkerEntry};
    use rub_core::model::{
        OrchestrationAddressInfo, OrchestrationExecutionPolicyInfo, OrchestrationMode,
        OrchestrationRuleInfo, OrchestrationRuleStatus, TriggerConditionKind, TriggerConditionSpec,
        TriggerEvidenceInfo,
    };
    use serde_json::json;
    use std::collections::HashMap;

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
}
