use super::*;
use crate::orchestration_executor::orchestration_non_authoritative_evidence_error;
use crate::session::NetworkRequestBaseline;

pub(super) fn reconcile_worker_state(
    worker_state: &mut HashMap<u32, OrchestrationWorkerEntry>,
    rules: &[OrchestrationRuleInfo],
    committed_baselines: &HashMap<u32, NetworkRequestBaseline>,
) {
    let live_ids = rules
        .iter()
        .map(|rule| rule.id)
        .collect::<std::collections::HashSet<_>>();
    worker_state.retain(|id, _| live_ids.contains(id));

    for rule in rules {
        let baseline_required = matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
            && matches!(rule.status, OrchestrationRuleStatus::Armed);
        let committed_baseline = committed_baselines.get(&rule.id).copied();
        let network_cursor_primed = committed_baseline
            .map(|baseline| baseline.primed)
            .unwrap_or(!baseline_required);
        let entry = worker_state
            .entry(rule.id)
            .or_insert(OrchestrationWorkerEntry {
                last_status: rule.status,
                network_cursor: committed_baseline
                    .map(|baseline| baseline.cursor)
                    .unwrap_or(0),
                network_cursor_primed,
                observatory_drop_count: committed_baseline
                    .map(|baseline| baseline.observed_ingress_drop_count)
                    .unwrap_or(0),
                latched_evidence_key: persisted_latched_orchestration_evidence_key(rule),
            });
        if !matches!(entry.last_status, OrchestrationRuleStatus::Armed)
            && matches!(rule.status, OrchestrationRuleStatus::Armed)
        {
            if let Some(committed_baseline) = committed_baseline {
                entry.network_cursor = committed_baseline.cursor;
                entry.network_cursor_primed = committed_baseline.primed;
                entry.observatory_drop_count = committed_baseline.observed_ingress_drop_count;
            } else if baseline_required {
                entry.network_cursor = 0;
                entry.network_cursor_primed = false;
                entry.observatory_drop_count = 0;
            } else {
                entry.network_cursor_primed = true;
            }
            entry.latched_evidence_key = persisted_latched_orchestration_evidence_key(rule);
        }
        if baseline_required && committed_baseline.is_none() {
            entry.network_cursor_primed = false;
        }
        entry.last_status = rule.status;
        if entry.latched_evidence_key.is_none() {
            entry.latched_evidence_key = persisted_latched_orchestration_evidence_key(rule);
        }
    }
}

pub(crate) fn orchestration_evidence_key(evidence: &TriggerEvidenceInfo) -> String {
    match evidence.fingerprint.as_deref() {
        Some(fingerprint) if !fingerprint.is_empty() => {
            format!("{}::{fingerprint}", evidence.summary)
        }
        _ => evidence.summary.clone(),
    }
}

pub(super) async fn load_orchestration_condition_state(
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

async fn evaluate_orchestration_condition(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    runtime: &OrchestrationRuntimeInfo,
    rule: &OrchestrationRuleInfo,
    worker: &mut OrchestrationWorkerEntry,
) -> Result<OrchestrationConditionEvaluation, ErrorEnvelope> {
    if matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
        && !worker.network_cursor_primed
    {
        return Err(
            ErrorEnvelope::new(
                ErrorCode::SessionBusy,
                "orchestration network_request evaluation is not authoritative because its committed observatory baseline is missing",
            )
            .with_context(serde_json::json!({
                "reason": "orchestration_network_request_baseline_missing",
                "next_network_cursor": worker.network_cursor,
                "dropped_event_count": worker.observatory_drop_count,
            })),
        );
    }

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
        if let Some(reason) =
            crate::orchestration_runtime::orchestration_session_addressability_reason(
                source_session,
            )
        {
            let _ = reason;
            return Err(
                crate::orchestration_runtime::orchestration_session_not_addressable_error(
                    source_session,
                    ErrorCode::SessionBusy,
                    format!(
                        "Source session '{}' is still present but not addressable for orchestration condition evaluation",
                        rule.source.session_name
                    ),
                    "orchestration_source_session_not_addressable",
                    "source_session_id",
                    "source_session_name",
                ),
            );
        }
        dispatch_remote_orchestration_probe(
            source_session,
            tab_target_id,
            rule.source.frame_id.as_deref(),
            &rule.condition,
            worker.network_cursor,
            worker.observatory_drop_count,
            None,
        )
        .await?
    };

    if let Some(reason) = result.degraded_reason {
        worker.network_cursor = result.next_network_cursor;
        worker.network_cursor_primed = true;
        worker.observatory_drop_count = result.observed_drop_count;
        return Err(orchestration_non_authoritative_evidence_error(
            "orchestration network_request evaluation is not authoritative because observatory evidence was dropped",
            Some(reason),
            serde_json::json!({
                "next_network_cursor": worker.network_cursor,
                "dropped_event_count": worker.observatory_drop_count,
            }),
        ));
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

pub(super) fn commit_orchestration_network_progress(
    worker: &mut OrchestrationWorkerEntry,
    progress: Option<OrchestrationNetworkProgress>,
) {
    if let Some(progress) = progress {
        worker.network_cursor = progress.next_cursor;
        worker.network_cursor_primed = true;
        worker.observatory_drop_count = progress.observed_drop_count;
    }
}

pub(super) fn skip_latched_orchestration_evidence(
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

pub(super) fn orchestration_rule_in_cooldown(rule: &OrchestrationRuleInfo) -> bool {
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

pub(super) fn should_persist_orchestration_evidence_latch(rule: &OrchestrationRuleInfo) -> bool {
    matches!(rule.mode, OrchestrationMode::Repeat)
        && matches!(rule.status, OrchestrationRuleStatus::Armed)
        && !matches!(rule.condition.kind, TriggerConditionKind::NetworkRequest)
}

pub(super) fn persisted_latched_orchestration_evidence_key(
    rule: &OrchestrationRuleInfo,
) -> Option<String> {
    if !should_persist_orchestration_evidence_latch(rule) {
        return None;
    }
    let result = rule.last_result.as_ref()?;
    if !should_retain_orchestration_evidence_latch(rule, result) {
        return None;
    }
    rule.last_condition_evidence
        .as_ref()
        .map(orchestration_evidence_key)
}

pub(super) fn should_retain_orchestration_evidence_latch(
    rule: &OrchestrationRuleInfo,
    result: &rub_core::model::OrchestrationResultInfo,
) -> bool {
    should_persist_orchestration_evidence_latch(rule)
        && matches!(result.next_status, OrchestrationRuleStatus::Armed)
        && (matches!(result.status, OrchestrationRuleStatus::Fired)
            || (matches!(result.status, OrchestrationRuleStatus::Blocked)
                && result.reason.as_deref() == Some("orchestration_cooldown_active")))
}

pub(super) async fn record_orchestration_probe_failure(
    state: &Arc<SessionState>,
    rule: &OrchestrationRuleInfo,
    envelope: ErrorEnvelope,
) {
    record_orchestration_failure_with_fallback(state, rule, envelope, None).await;
}

pub(super) async fn record_orchestration_failure_with_fallback(
    state: &Arc<SessionState>,
    rule: &OrchestrationRuleInfo,
    envelope: ErrorEnvelope,
    evidence: Option<TriggerEvidenceInfo>,
) {
    let result_status = classify_orchestration_error_status(envelope.code);
    let reason = envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let error_context = envelope.context.clone();
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
        error_context,
    };
    warn!(
        rule_id = rule.id,
        result_status = ?result_status,
        summary = %result.summary,
        "Reactive orchestration condition probe failed"
    );
    state
        .record_orchestration_outcome_with_fallback(
            rule,
            Some(rule.lifecycle_generation),
            evidence,
            result,
        )
        .await;
}
