use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    OrchestrationRuleInfo, OrchestrationRuleStatus, OrchestrationRuntimeInfo,
    OrchestrationSessionInfo, TriggerConditionKind, TriggerEvidenceInfo,
};
use rub_ipc::protocol::IpcRequest;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::warn;

use crate::orchestration_executor::{
    classify_orchestration_error_status, decode_orchestration_success_payload,
    dispatch_remote_orchestration_request, execute_orchestration_rule,
};
use crate::orchestration_probe::{OrchestrationProbeResult, evaluate_orchestration_probe_for_tab};
use crate::router::{DaemonRouter, RouterTransactionGuard};
use crate::runtime_refresh::refresh_orchestration_runtime;
use crate::session::SessionState;

const ORCHESTRATION_WORKER_INTERVAL: Duration = Duration::from_millis(500);
const ORCHESTRATION_PROBE_TIMEOUT_MS: u64 = 30_000;
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

async fn load_orchestration_condition_state(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    rule: &OrchestrationRuleInfo,
    worker: &mut OrchestrationWorkerEntry,
) -> Result<OrchestrationConditionState, ErrorEnvelope> {
    let runtime = state.orchestration_runtime().await;
    let evaluation =
        evaluate_orchestration_condition(router, state, &runtime, rule, worker).await?;
    Ok(match evaluation.evidence {
        Some(evidence) => OrchestrationConditionState::Triggered(TriggeredOrchestrationCondition {
            evidence_key: orchestration_evidence_key(&evidence),
            evidence,
            network_progress: evaluation.network_progress,
        }),
        None => OrchestrationConditionState::NotTriggered {
            network_progress: evaluation.network_progress,
        },
    })
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

fn reconcile_worker_state(
    worker_state: &mut HashMap<u32, OrchestrationWorkerEntry>,
    rules: &[OrchestrationRuleInfo],
    active_request_cursor: u64,
    observatory_drop_count: u64,
    current_session_id: &str,
) {
    let live_ids = rules
        .iter()
        .map(|rule| rule.id)
        .collect::<std::collections::HashSet<_>>();
    worker_state.retain(|id, _| live_ids.contains(id));

    for rule in rules {
        let local_source = rule.source.session_id == current_session_id;
        let network_cursor_primed =
            !matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest) || local_source;
        let entry = worker_state
            .entry(rule.id)
            .or_insert(OrchestrationWorkerEntry {
                last_status: rule.status,
                network_cursor: if local_source {
                    active_request_cursor
                } else {
                    0
                },
                network_cursor_primed,
                observatory_drop_count,
                latched_evidence_key: None,
            });
        if !matches!(entry.last_status, OrchestrationRuleStatus::Armed)
            && matches!(rule.status, OrchestrationRuleStatus::Armed)
        {
            entry.network_cursor = if local_source {
                active_request_cursor
            } else {
                0
            };
            entry.network_cursor_primed = network_cursor_primed;
            entry.observatory_drop_count = observatory_drop_count;
            entry.latched_evidence_key = None;
        }
        entry.last_status = rule.status;
    }
}

fn orchestration_evidence_key(evidence: &TriggerEvidenceInfo) -> String {
    match evidence.fingerprint.as_deref() {
        Some(fingerprint) if !fingerprint.is_empty() => {
            format!("{}::{fingerprint}", evidence.summary)
        }
        _ => evidence.summary.clone(),
    }
}

async fn evaluate_orchestration_condition(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    worker: &mut OrchestrationWorkerEntry,
) -> Result<OrchestrationConditionEvaluation, ErrorEnvelope> {
    let tab_target_id = rule.source.tab_target_id.as_deref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "orchestration source address is missing source.tab_target_id",
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_source_tab_target_missing",
            "source_session_id": rule.source.session_id,
            "source_session_name": rule.source.session_name,
        }))
    })?;

    let result = if rule.source.session_id == state.session_id {
        evaluate_orchestration_probe_for_tab(
            &router.browser_port(),
            state,
            tab_target_id,
            rule.source.frame_id.as_deref(),
            &rule.condition,
            worker.network_cursor,
            worker.observatory_drop_count,
        )
        .await
        .map_err(|error| error.into_envelope())?
    } else {
        if matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
            && !worker.network_cursor_primed
        {
            worker.network_cursor = prime_remote_orchestration_network_cursor(
                runtime,
                rule,
                tab_target_id,
                rule.source.frame_id.as_deref(),
                &rule.condition,
            )
            .await?;
            worker.network_cursor_primed = true;
            return Ok(OrchestrationConditionEvaluation {
                evidence: None,
                network_progress: None,
            });
        }
        let source_session = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == rule.source.session_id)
            .ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::DaemonNotRunning,
                    format!(
                        "Source session '{}' is not available for orchestration condition evaluation",
                        rule.source.session_name
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "orchestration_source_session_missing",
                    "source_session_id": rule.source.session_id,
                    "source_session_name": rule.source.session_name,
                }))
            })?;
        dispatch_remote_orchestration_probe(
            source_session,
            tab_target_id,
            rule.source.frame_id.as_deref(),
            &rule.condition,
            worker.network_cursor,
            worker.observatory_drop_count,
        )
        .await?
    };

    if let Some(reason) = result.degraded_reason {
        worker.network_cursor = result.next_network_cursor;
        worker.network_cursor_primed = true;
        worker.observatory_drop_count = result.observed_drop_count;
        return Err(
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                "orchestration network_request evaluation is not authoritative because observatory evidence was dropped",
            )
            .with_context(serde_json::json!({
                "reason": "runtime_observatory_not_authoritative",
                "degraded_reason": reason,
                "next_network_cursor": worker.network_cursor,
                "dropped_event_count": worker.observatory_drop_count,
            })),
        );
    }
    Ok(OrchestrationConditionEvaluation {
        evidence: if result.matched {
            result.evidence
        } else {
            None
        },
        network_progress: Some(OrchestrationNetworkProgress {
            next_cursor: result.next_network_cursor,
            observed_drop_count: result.observed_drop_count,
        }),
    })
}

fn commit_orchestration_network_progress(
    worker: &mut OrchestrationWorkerEntry,
    progress: Option<OrchestrationNetworkProgress>,
) {
    if let Some(progress) = progress {
        worker.network_cursor = progress.next_cursor;
        worker.network_cursor_primed = true;
        worker.observatory_drop_count = progress.observed_drop_count;
    }
}

fn skip_latched_orchestration_evidence(
    worker: &mut OrchestrationWorkerEntry,
    evidence_key: &str,
    network_progress: Option<OrchestrationNetworkProgress>,
) -> bool {
    if worker
        .latched_evidence_key
        .as_ref()
        .is_some_and(|latched| latched == evidence_key)
    {
        commit_orchestration_network_progress(worker, network_progress);
        return true;
    }
    false
}

async fn prime_remote_orchestration_network_cursor(
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &rub_core::model::TriggerConditionSpec,
) -> Result<u64, ErrorEnvelope> {
    let source_session = runtime
        .known_sessions
        .iter()
        .find(|session| session.session_id == rule.source.session_id)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "Source session '{}' is not available for orchestration condition evaluation",
                    rule.source.session_name
                ),
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_source_session_missing",
                "source_session_id": rule.source.session_id,
                "source_session_name": rule.source.session_name,
            }))
        })?;
    Ok(dispatch_remote_orchestration_probe(
        source_session,
        tab_target_id,
        frame_id,
        condition,
        u64::MAX,
        0,
    )
    .await?
    .next_network_cursor)
}

async fn dispatch_remote_orchestration_probe(
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &rub_core::model::TriggerConditionSpec,
    after_sequence: u64,
    last_observed_drop_count: u64,
) -> Result<OrchestrationProbeResult, ErrorEnvelope> {
    let response = dispatch_remote_orchestration_request(
        session,
        "source",
        IpcRequest::new(
            "_orchestration_probe",
            serde_json::json!({
                "tab_target_id": tab_target_id,
                "frame_id": frame_id,
                "condition": condition,
                "after_sequence": after_sequence,
                "last_observed_drop_count": last_observed_drop_count,
            }),
            ORCHESTRATION_PROBE_TIMEOUT_MS,
        ),
        "probe",
        "orchestration_source_session_unreachable",
        "orchestration_source_probe_dispatch_failed",
        "remote orchestration probe returned an error without an envelope",
    )
    .await?;

    decode_orchestration_success_payload(
        response,
        session,
        "orchestration_source_probe_payload_missing",
        "orchestration probe returned success without a payload",
        "orchestration_source_probe_payload_invalid",
        "orchestration probe payload",
    )
}

fn orchestration_rule_in_cooldown(rule: &OrchestrationRuleInfo) -> bool {
    rule.execution_policy
        .cooldown_until_ms
        .map(|deadline| current_time_ms() < deadline)
        .unwrap_or(false)
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

async fn record_orchestration_probe_failure(
    state: &Arc<SessionState>,
    rule: &OrchestrationRuleInfo,
    envelope: ErrorEnvelope,
) {
    let result_status = classify_orchestration_error_status(envelope.code);
    let reason = envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let result = rub_core::model::OrchestrationResultInfo {
        rule_id: rule.id,
        status: result_status,
        next_status: rule.status,
        summary: format!(
            "orchestration condition evaluation failed: {}: {}",
            envelope.code, envelope.message
        ),
        committed_steps: 0,
        total_steps: rule.actions.len() as u32,
        steps: Vec::new(),
        cooldown_until_ms: None,
        error_code: Some(envelope.code),
        reason,
    };
    warn!(
        rule_id = rule.id,
        result_status = ?result_status,
        summary = %result.summary,
        "Reactive orchestration condition probe failed"
    );
    state
        .record_orchestration_outcome_with_fallback(rule, None, result)
        .await;
}

#[cfg(test)]
mod tests {
    use super::{
        OrchestrationNetworkProgress, OrchestrationWorkerEntry, orchestration_evidence_key,
        reconcile_worker_state, skip_latched_orchestration_evidence,
    };
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
}
