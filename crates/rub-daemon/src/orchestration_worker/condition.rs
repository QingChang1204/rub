use super::*;

pub(super) fn reconcile_worker_state(
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
                latched_evidence_key: persisted_latched_orchestration_evidence_key(rule),
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
            entry.latched_evidence_key = persisted_latched_orchestration_evidence_key(rule);
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
    let preserved_latch = matches!(result.status, OrchestrationRuleStatus::Fired)
        || (matches!(result.status, OrchestrationRuleStatus::Blocked)
            && result.reason.as_deref() == Some("orchestration_cooldown_active"));
    if !preserved_latch || !matches!(result.next_status, OrchestrationRuleStatus::Armed) {
        return None;
    }
    rule.last_condition_evidence
        .as_ref()
        .map(orchestration_evidence_key)
}

pub(super) async fn record_orchestration_probe_failure(
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
