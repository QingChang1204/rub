use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::locator::LiveLocator;
use rub_core::model::{
    NetworkRequestRecord, ReadinessInfo, RouteStability, TabInfo, TriggerActionExecutionInfo,
    TriggerActionKind, TriggerConditionKind, TriggerEvidenceInfo, TriggerInfo, TriggerResultInfo,
    TriggerStatus,
};
use rub_core::storage::{StorageArea, StorageSnapshot};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::{DaemonRouter, RouterTransactionGuard};
use crate::runtime_refresh::{
    refresh_live_frame_runtime, refresh_live_runtime_state, refresh_live_trigger_runtime,
};
use crate::session::SessionState;
use crate::trigger_workflow_bridge::{
    resolve_trigger_workflow_parameterization, trigger_workflow_source_var_keys,
};
use crate::workflow_assets::{load_named_workflow_spec, resolve_named_workflow_path};

const TRIGGER_WORKER_INTERVAL: Duration = Duration::from_millis(500);
const TRIGGER_ACTION_BASE_TIMEOUT_MS: u64 = 30_000;
const TRIGGER_AUTOMATION_TRANSACTION_TIMEOUT_MS: u64 = 100;

#[derive(Debug, Clone, Copy)]
struct TriggerWorkerEntry {
    last_status: TriggerStatus,
    network_cursor: u64,
    observatory_drop_count: u64,
}

#[derive(Debug, Clone, Copy)]
struct TriggerNetworkProgress {
    next_cursor: u64,
    observed_drop_count: u64,
}

struct TriggerConditionEvaluation {
    evidence: Option<TriggerEvidenceInfo>,
    network_progress: Option<TriggerNetworkProgress>,
}

enum TriggerConditionState {
    NotTriggered {
        network_progress: Option<TriggerNetworkProgress>,
    },
    Triggered(TriggeredTriggerCondition),
}

struct TriggeredTriggerCondition {
    evidence: TriggerEvidenceInfo,
    evidence_fingerprint: String,
    network_progress: Option<TriggerNetworkProgress>,
}

struct ReservedTriggerExecution<'a> {
    trigger: TriggerInfo,
    tabs: Vec<TabInfo>,
    evidence: TriggerEvidenceInfo,
    evidence_fingerprint: String,
    network_progress: Option<TriggerNetworkProgress>,
    _transaction: RouterTransactionGuard<'a>,
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
                    run_trigger_cycle(&router, &state, &mut worker_state).await;
                }
            }
        }
    })
}

async fn run_trigger_cycle(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
) {
    if state.in_flight_count.load(Ordering::SeqCst) > 0 {
        return;
    }

    let triggers = state.triggers().await;
    if triggers.is_empty() {
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

    for trigger in triggers {
        if !matches!(trigger.status, TriggerStatus::Armed) || trigger.unavailable_reason.is_some() {
            continue;
        }

        process_trigger_rule(router, state, &browser, &tabs, trigger, worker_state).await;
    }
}

async fn process_trigger_rule(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    tabs: &[TabInfo],
    trigger: TriggerInfo,
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
) {
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
            return;
        }
        TriggerConditionState::Triggered(triggered) => triggered,
    };

    let reserved = match reserve_trigger_execution(
        router,
        state,
        browser,
        &trigger,
        worker_state
            .get_mut(&trigger.id)
            .expect("worker_state entry should exist"),
        triggered,
    )
    .await
    {
        Ok(Some(reserved)) => reserved,
        Ok(None) => return,
        Err(envelope) => {
            record_trigger_failure(state, &trigger, envelope, None, None).await;
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
            if let Some(worker) = worker_state.get_mut(&trigger.id) {
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
            if let Some(worker) = worker_state.get_mut(&trigger.id) {
                commit_trigger_network_progress(worker, reserved.network_progress);
            }
        }
    }
}

async fn load_trigger_condition_state(
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    state: &Arc<SessionState>,
    tabs: &[TabInfo],
    trigger: &TriggerInfo,
    worker: &mut TriggerWorkerEntry,
) -> Result<TriggerConditionState, RubError> {
    let evaluation = evaluate_trigger_condition(browser, state, tabs, trigger, worker).await?;
    Ok(match evaluation.evidence {
        Some(evidence) => TriggerConditionState::Triggered(TriggeredTriggerCondition {
            evidence_fingerprint: trigger_evidence_consumption_key(&evidence),
            evidence,
            network_progress: evaluation.network_progress,
        }),
        None => TriggerConditionState::NotTriggered {
            network_progress: evaluation.network_progress,
        },
    })
}

async fn reserve_trigger_execution<'a>(
    router: &'a Arc<DaemonRouter>,
    state: &'a Arc<SessionState>,
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    trigger: &TriggerInfo,
    worker: &mut TriggerWorkerEntry,
    triggered: TriggeredTriggerCondition,
) -> Result<Option<ReservedTriggerExecution<'a>>, ErrorEnvelope> {
    let transaction = match router
        .begin_automation_transaction(
            state,
            TRIGGER_AUTOMATION_TRANSACTION_TIMEOUT_MS,
            "trigger_worker",
        )
        .await
    {
        Ok(transaction) => transaction,
        Err(_) => return Ok(None),
    };

    let live_trigger = match state.trigger_rule(trigger.id).await {
        Some(trigger)
            if matches!(trigger.status, TriggerStatus::Armed)
                && trigger.unavailable_reason.is_none() =>
        {
            trigger
        }
        _ => {
            drop(transaction);
            return Ok(None);
        }
    };
    let live_tabs = match refresh_live_trigger_runtime(browser, state).await {
        Ok(tabs) => tabs,
        Err(_) => {
            drop(transaction);
            return Ok(None);
        }
    };
    let live_condition =
        match load_trigger_condition_state(browser, state, &live_tabs, &live_trigger, worker).await
        {
            Ok(condition) => condition,
            Err(error) => {
                drop(transaction);
                return Err(ErrorEnvelope::new(
                    ErrorCode::BrowserCrashed,
                    format!("trigger condition evaluation failed after queue: {error}"),
                ));
            }
        };
    let TriggerConditionState::Triggered(triggered_after_queue) = live_condition else {
        let _ = state
            .set_trigger_condition_evidence(live_trigger.id, None)
            .await;
        commit_trigger_network_progress(worker, triggered.network_progress);
        drop(transaction);
        return Ok(None);
    };

    if live_trigger.consumed_evidence_fingerprint.as_deref()
        == Some(&triggered_after_queue.evidence_fingerprint)
    {
        let _ = state
            .set_trigger_condition_evidence(
                live_trigger.id,
                Some(triggered_after_queue.evidence.clone()),
            )
            .await;
        commit_trigger_network_progress(
            worker,
            triggered_after_queue
                .network_progress
                .or(triggered.network_progress),
        );
        drop(transaction);
        return Ok(None);
    }

    Ok(Some(ReservedTriggerExecution {
        trigger: live_trigger,
        tabs: live_tabs,
        evidence: triggered_after_queue.evidence,
        evidence_fingerprint: triggered_after_queue.evidence_fingerprint,
        network_progress: triggered_after_queue
            .network_progress
            .or(triggered.network_progress),
        _transaction: transaction,
    }))
}

fn reconcile_worker_state(
    worker_state: &mut HashMap<u32, TriggerWorkerEntry>,
    triggers: &[TriggerInfo],
    active_request_cursor: u64,
    observatory_drop_count: u64,
) {
    let live_ids = triggers
        .iter()
        .map(|trigger| trigger.id)
        .collect::<HashSet<_>>();
    worker_state.retain(|id, _| live_ids.contains(id));

    for trigger in triggers {
        let entry = worker_state
            .entry(trigger.id)
            .or_insert(TriggerWorkerEntry {
                last_status: trigger.status,
                network_cursor: active_request_cursor,
                observatory_drop_count,
            });
        if !matches!(entry.last_status, TriggerStatus::Armed)
            && matches!(trigger.status, TriggerStatus::Armed)
        {
            entry.network_cursor = active_request_cursor;
            entry.observatory_drop_count = observatory_drop_count;
        }
        entry.last_status = trigger.status;
    }
}

async fn evaluate_trigger_condition(
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    state: &Arc<SessionState>,
    tabs: &[TabInfo],
    trigger: &TriggerInfo,
    worker: &mut TriggerWorkerEntry,
) -> Result<TriggerConditionEvaluation, RubError> {
    match trigger.condition.kind {
        TriggerConditionKind::UrlMatch => {
            let pattern = trigger
                .condition
                .url_pattern
                .as_deref()
                .unwrap_or_default()
                .trim();
            let source_tab = resolve_bound_tab(tabs, &trigger.source_tab.target_id)?;
            if !source_tab.url.contains(pattern) {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_url_matched:{pattern}"),
                    fingerprint: Some(source_tab.url.clone()),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::TextPresent => {
            let text = trigger.condition.text.as_deref().unwrap_or_default();
            if !browser
                .tab_has_text(&trigger.source_tab.target_id, None, text)
                .await?
            {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_text_present:{text}"),
                    fingerprint: Some(text.to_string()),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::LocatorPresent => {
            let locator = trigger.condition.locator.as_ref().ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    "trigger locator_present condition is missing a locator",
                )
            })?;
            let locator = LiveLocator::try_from(locator.clone()).map_err(|invalid| {
                RubError::domain_with_context_and_suggestion(
                    ErrorCode::InvalidInput,
                    "trigger locator_present condition requires a live DOM locator",
                    serde_json::json!({
                        "locator": invalid,
                    }),
                    "Use selector, target_text, role, label, or testid addressing for live content probes",
                )
            })?;
            let matches = browser
                .find_content_matches_in_tab(&trigger.source_tab.target_id, None, &locator)
                .await?;
            if matches.is_empty() {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            let first = &matches[0];
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!(
                        "source_tab_locator_present:{}:{}",
                        first.tag_name,
                        matches.len()
                    ),
                    fingerprint: Some(format!(
                        "{}:{}:{}",
                        first.tag_name,
                        first.role,
                        matches.len()
                    )),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::Readiness => {
            let readiness = browser
                .probe_runtime_state_for_tab(&trigger.source_tab.target_id, None)
                .await?
                .readiness_state;
            let requested = trigger
                .condition
                .readiness_state
                .as_deref()
                .unwrap_or_default();
            if !readiness_matches(&readiness, requested) {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_readiness_matched:{requested}"),
                    fingerprint: readiness.document_ready_state.clone().or_else(|| {
                        Some(format!("{:?}", readiness.route_stability).to_lowercase())
                    }),
                }),
                network_progress: None,
            })
        }
        TriggerConditionKind::NetworkRequest => {
            let window = state
                .network_request_window_after(worker.network_cursor, worker.observatory_drop_count)
                .await;
            let observed_drop_count = state.network_request_drop_count().await;
            if !window.authoritative {
                worker.network_cursor = window.next_cursor;
                worker.observatory_drop_count = observed_drop_count;
                return Err(RubError::domain_with_context(
                    ErrorCode::BrowserCrashed,
                    "trigger network_request evaluation is not authoritative because observatory evidence was dropped",
                    serde_json::json!({
                        "reason": "runtime_observatory_not_authoritative",
                        "degraded_reason": window.degraded_reason,
                        "next_network_cursor": worker.network_cursor,
                        "dropped_event_count": worker.observatory_drop_count,
                    }),
                ));
            }
            let network_progress = Some(TriggerNetworkProgress {
                next_cursor: window.next_cursor,
                observed_drop_count,
            });

            let Some(record) = window
                .records
                .into_iter()
                .find(|record| network_request_matches(record, trigger))
            else {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress,
                });
            };
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("network_request_matched:{}", record.request_id),
                    fingerprint: Some(record.request_id),
                }),
                network_progress,
            })
        }
        TriggerConditionKind::StorageValue => {
            let snapshot = browser
                .storage_snapshot_for_tab(&trigger.source_tab.target_id, None)
                .await?;
            if !storage_snapshot_matches(&snapshot, trigger)? {
                return Ok(TriggerConditionEvaluation {
                    evidence: None,
                    network_progress: None,
                });
            }
            let key = trigger.condition.key.as_deref().unwrap_or_default();
            Ok(TriggerConditionEvaluation {
                evidence: Some(TriggerEvidenceInfo {
                    summary: format!("source_tab_storage_matched:{key}"),
                    fingerprint: Some(format!("{}:{key}", snapshot.origin)),
                }),
                network_progress: None,
            })
        }
    }
}

fn commit_trigger_network_progress(
    worker: &mut TriggerWorkerEntry,
    progress: Option<TriggerNetworkProgress>,
) {
    if let Some(progress) = progress {
        worker.network_cursor = progress.next_cursor;
        worker.observatory_drop_count = progress.observed_drop_count;
    }
}

async fn fire_trigger(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    tabs: &[TabInfo],
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    command_id: &str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    let target_tab = resolve_bound_tab(tabs, &trigger.target_tab.target_id)
        .map_err(|error| ErrorEnvelope::new(ErrorCode::TabNotFound, error.to_string()))?;

    if !target_tab.active {
        let switch_request = IpcRequest::new(
            "switch",
            serde_json::json!({
                "index": target_tab.index,
                "_trigger": trigger_request_meta(trigger, evidence, "target_switch"),
            }),
            TRIGGER_ACTION_BASE_TIMEOUT_MS,
        );
        let response = router
            .dispatch_within_active_transaction(switch_request, state)
            .await;
        // We only care whether the tab switch succeeded; the Ok payload is empty.
        ensure_trigger_response_success(response)?;
    }

    ensure_trigger_target_continuity(router, state, &trigger.target_tab.target_id).await?;

    match trigger.action.kind {
        TriggerActionKind::BrowserCommand => {
            fire_browser_command_trigger(router, state, trigger, evidence, command_id).await
        }
        TriggerActionKind::Workflow => {
            fire_workflow_trigger(router, state, trigger, evidence, command_id).await
        }
        TriggerActionKind::Provider | TriggerActionKind::Script | TriggerActionKind::Webhook => {
            Err(ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                "trigger action.kind is not yet executable in this runtime slice",
            ))
        }
    }
}

async fn fire_browser_command_trigger(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    command_id: &str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    let command = trigger.action.command.as_deref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger browser_command action is missing action.command",
        )
    })?;
    let mut payload = trigger
        .action
        .payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let object = payload.as_object_mut().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger browser_command action payload must be a JSON object",
        )
    })?;
    object.insert(
        "_trigger".to_string(),
        trigger_request_meta(trigger, evidence, "action"),
    );

    let response = router
        .dispatch_within_active_transaction(
            {
                let args = serde_json::Value::Object(object.clone());
                IpcRequest::new(
                    command,
                    args.clone(),
                    trigger_action_timeout_ms(command, &args),
                )
                .with_command_id(command_id)
                .expect("trigger action command_id must remain protocol-valid")
            },
            state,
        )
        .await;
    let data = ensure_trigger_response_success(response)?;
    ensure_committed_automation_result(command, data.as_ref())?;
    Ok(data)
}

async fn fire_workflow_trigger(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    command_id: &str,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    let payload = trigger.action.payload.as_ref().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger workflow action is missing action.payload",
        )
    })?;
    let object = payload.as_object().ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            "trigger workflow payload must be a JSON object",
        )
    })?;

    let (raw_spec, mut spec_source) = resolve_trigger_workflow_spec(object, &state.rub_home)
        .map_err(|error| error.into_envelope())?;
    let parameterized = resolve_trigger_workflow_parameterization(
        &router.browser_port(),
        &trigger.source_tab.target_id,
        None,
        object,
        &raw_spec,
    )
    .await
    .map_err(|error| error.into_envelope())?;
    if let Some(spec_source_object) = spec_source.as_object_mut() {
        spec_source_object.insert(
            "vars".to_string(),
            serde_json::json!(parameterized.parameter_keys),
        );
    }

    let args = serde_json::json!({
        "spec": parameterized.resolved_spec,
        "spec_source": spec_source,
        "_trigger": trigger_request_meta(trigger, evidence, "action"),
    });
    let response = router
        .dispatch_within_active_transaction(
            IpcRequest::new(
                "pipe",
                args.clone(),
                trigger_action_timeout_ms("pipe", &args),
            )
            .with_command_id(command_id)
            .expect("trigger action command_id must remain protocol-valid"),
            state,
        )
        .await;
    ensure_trigger_response_success(response)
}

fn resolve_trigger_workflow_spec(
    payload: &serde_json::Map<String, serde_json::Value>,
    rub_home: &std::path::Path,
) -> Result<(String, serde_json::Value), RubError> {
    match (
        payload
            .get("workflow_name")
            .and_then(|value| value.as_str()),
        payload.get("steps"),
    ) {
        (Some(name), None) => {
            let (normalized, contents, path) = load_named_workflow_spec(rub_home, name)?;
            Ok((
                contents,
                serde_json::json!({
                    "kind": "workflow",
                    "name": normalized,
                    "path": path.display().to_string(),
                }),
            ))
        }
        (None, Some(steps)) if steps.is_array() => {
            let raw_steps = serde_json::to_string(steps).map_err(RubError::from)?;
            Ok((
                raw_steps,
                serde_json::json!({
                    "kind": "trigger_inline_workflow",
                    "step_count": steps.as_array().map(|steps| steps.len()).unwrap_or(0),
                }),
            ))
        }
        (Some(_), Some(_)) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload must provide exactly one of payload.workflow_name or payload.steps",
        )),
        _ => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "trigger workflow payload requires non-empty payload.workflow_name or payload.steps",
        )),
    }
}

async fn ensure_trigger_target_continuity(
    router: &Arc<DaemonRouter>,
    state: &Arc<SessionState>,
    target_id: &str,
) -> Result<(), ErrorEnvelope> {
    let browser = router.browser_port();
    let tabs = refresh_live_trigger_runtime(&browser, state)
        .await
        .map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::BrowserCrashed,
                format!("trigger target continuity refresh failed: {error}"),
            )
            .with_context(serde_json::json!({
                "reason": "continuity_tab_refresh_failed",
                "target_tab_target_id": target_id,
            }))
        })?;
    let target_tab = resolve_bound_tab(&tabs, target_id).map_err(|error| {
        ErrorEnvelope::new(ErrorCode::TabNotFound, error.to_string()).with_context(
            serde_json::json!({
                "reason": "continuity_target_tab_missing",
                "target_tab_target_id": target_id,
            }),
        )
    })?;
    if !target_tab.active {
        return Err(ErrorEnvelope::new(
            ErrorCode::BrowserCrashed,
            "Trigger target continuity fence failed: target tab is not active after switch",
        )
        .with_context(serde_json::json!({
            "reason": "continuity_target_not_active",
            "target_tab_target_id": target_id,
            "target_tab_index": target_tab.index,
        })));
    }

    refresh_live_runtime_state(&browser, state).await;
    refresh_live_frame_runtime(&browser, state).await;
    let frame_runtime = state.frame_runtime().await;
    let readiness = state.readiness_state().await;
    if let Some((reason, message)) =
        trigger_target_continuity_failure(target_id, &frame_runtime, &readiness)
    {
        return Err(
            ErrorEnvelope::new(ErrorCode::BrowserCrashed, message).with_context(
                serde_json::json!({
                    "reason": reason,
                    "target_tab_target_id": target_id,
                    "frame_runtime": frame_runtime,
                    "readiness_state": readiness,
                }),
            ),
        );
    }

    Ok(())
}

fn ensure_trigger_response_success(
    response: rub_ipc::protocol::IpcResponse,
) -> Result<Option<serde_json::Value>, ErrorEnvelope> {
    match response.status {
        ResponseStatus::Success => Ok(response.data),
        ResponseStatus::Error => Err(response.error.unwrap_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                "trigger action returned an error response without an error envelope",
            )
        })),
    }
}

fn resolve_bound_tab<'a>(tabs: &'a [TabInfo], target_id: &str) -> Result<&'a TabInfo, RubError> {
    tabs.iter()
        .find(|tab| tab.target_id == target_id)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::TabNotFound,
                format!("Tab target '{target_id}' is not present in the current session"),
            )
        })
}

fn trigger_target_continuity_failure(
    target_tab_id: &str,
    frame_runtime: &rub_core::model::FrameRuntimeInfo,
    readiness: &ReadinessInfo,
) -> Option<(&'static str, &'static str)> {
    if matches!(
        frame_runtime.status,
        rub_core::model::FrameContextStatus::Unknown
            | rub_core::model::FrameContextStatus::Stale
            | rub_core::model::FrameContextStatus::Degraded
    ) || frame_runtime.current_frame.is_none()
    {
        return Some((
            "continuity_frame_unavailable",
            "Trigger target continuity fence failed: frame context became unavailable",
        ));
    }
    if frame_runtime
        .current_frame
        .as_ref()
        .and_then(|frame| frame.target_id.as_deref())
        != Some(target_tab_id)
    {
        return Some((
            "continuity_frame_target_mismatch",
            "Trigger target continuity fence failed: frame context no longer matches the target tab authority",
        ));
    }
    if matches!(readiness.status, rub_core::model::ReadinessStatus::Degraded) {
        return Some((
            "continuity_readiness_degraded",
            "Trigger target continuity fence failed: readiness surface degraded",
        ));
    }
    None
}

fn readiness_matches(readiness: &ReadinessInfo, requested: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() {
        return false;
    }

    if requested == "ready" {
        return matches!(readiness.route_stability, RouteStability::Stable)
            && readiness.degraded_reason.is_none();
    }

    requested == format!("{:?}", readiness.status).to_ascii_lowercase()
        || requested == format!("{:?}", readiness.route_stability).to_ascii_lowercase()
        || readiness
            .document_ready_state
            .as_deref()
            .map(|state| state.eq_ignore_ascii_case(&requested))
            .unwrap_or(false)
}

fn trigger_action_summary(trigger: &TriggerInfo) -> String {
    match trigger.action.kind {
        TriggerActionKind::BrowserCommand => format!(
            "'{}'",
            trigger
                .action
                .command
                .as_deref()
                .unwrap_or("browser_command")
        ),
        TriggerActionKind::Workflow => trigger
            .action
            .payload
            .as_ref()
            .and_then(|payload| payload.get("workflow_name"))
            .and_then(|value| value.as_str())
            .map(|name| format!("workflow '{name}'"))
            .unwrap_or_else(|| "inline workflow".to_string()),
        TriggerActionKind::Provider => "provider action".to_string(),
        TriggerActionKind::Script => "script action".to_string(),
        TriggerActionKind::Webhook => "webhook action".to_string(),
    }
}

fn network_request_matches(record: &NetworkRequestRecord, trigger: &TriggerInfo) -> bool {
    if record.tab_target_id.as_deref() != Some(trigger.source_tab.target_id.as_str()) {
        return false;
    }

    let url_pattern = trigger.condition.url_pattern.as_deref().unwrap_or_default();
    if !record.url.contains(url_pattern) {
        return false;
    }

    if let Some(method) = trigger.condition.method.as_deref()
        && !record.method.eq_ignore_ascii_case(method)
    {
        return false;
    }

    if let Some(status_code) = trigger.condition.status_code
        && record.status != Some(status_code)
    {
        return false;
    }

    true
}

fn storage_snapshot_matches(
    snapshot: &StorageSnapshot,
    trigger: &TriggerInfo,
) -> Result<bool, RubError> {
    let key = trigger.condition.key.as_deref().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "trigger storage_value condition is missing condition.key",
        )
    })?;
    let expected_value = trigger.condition.value.as_deref();
    let area = parse_storage_area(trigger.condition.storage_area.as_deref())?;

    let areas = match area {
        Some(StorageArea::Local) => [&snapshot.local_storage, &std::collections::BTreeMap::new()],
        Some(StorageArea::Session) => [
            &std::collections::BTreeMap::new(),
            &snapshot.session_storage,
        ],
        None => [&snapshot.local_storage, &snapshot.session_storage],
    };

    for entries in areas {
        if let Some(value) = entries.get(key)
            && expected_value
                .map(|expected| expected == value)
                .unwrap_or(true)
        {
            return Ok(true);
        }
    }

    Ok(false)
}

fn parse_storage_area(area: Option<&str>) -> Result<Option<StorageArea>, RubError> {
    match area.map(|value| value.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(value) if value == "local" => Ok(Some(StorageArea::Local)),
        Some(value) if value == "session" => Ok(Some(StorageArea::Session)),
        Some(other) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unsupported trigger storage area '{other}'; use 'local' or 'session'"),
        )),
    }
}

async fn record_trigger_failure(
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    envelope: ErrorEnvelope,
    evidence: Option<TriggerEvidenceInfo>,
    command_id: Option<String>,
) {
    let result_status = classify_trigger_error_status(envelope.code);
    let consumed_evidence_fingerprint = evidence.as_ref().and_then(|evidence| {
        matches!(result_status, TriggerStatus::Blocked)
            .then(|| trigger_evidence_consumption_key(evidence))
    });
    let summary = format!(
        "trigger action failed: {}: {}",
        envelope.code, envelope.message
    );
    let reason = envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    warn!(
        trigger_id = trigger.id,
        result_status = ?result_status,
        summary = %summary,
        "Trigger failure"
    );
    let _ = state
        .record_trigger_outcome(
            trigger.id,
            trigger.status,
            evidence,
            TriggerResultInfo {
                trigger_id: trigger.id,
                status: result_status,
                summary,
                command_id,
                action: Some(trigger_action_execution_info(trigger, &state.rub_home)),
                result: None,
                error_code: Some(envelope.code),
                reason,
                consumed_evidence_fingerprint,
            },
        )
        .await;
}

fn trigger_action_timeout_ms(command: &str, args: &serde_json::Value) -> u64 {
    TRIGGER_ACTION_BASE_TIMEOUT_MS.saturating_add(
        rub_core::automation_timeout::command_additional_timeout_ms(command, args),
    )
}

fn trigger_evidence_consumption_key(evidence: &TriggerEvidenceInfo) -> String {
    evidence
        .fingerprint
        .clone()
        .unwrap_or_else(|| evidence.summary.clone())
}

fn trigger_action_command_id(trigger: &TriggerInfo, evidence: &TriggerEvidenceInfo) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    trigger.id.hash(&mut hasher);
    trigger_evidence_consumption_key(evidence).hash(&mut hasher);
    format!("trigger:{}:{:016x}", trigger.id, hasher.finish())
}

fn trigger_action_execution_info(
    trigger: &TriggerInfo,
    rub_home: &std::path::Path,
) -> TriggerActionExecutionInfo {
    let mut vars = trigger
        .action
        .payload
        .as_ref()
        .and_then(|payload| payload.get("vars"))
        .and_then(|vars| vars.as_object())
        .map(|vars| vars.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    vars.sort();
    let source_vars = trigger
        .action
        .payload
        .as_ref()
        .and_then(|payload| payload.as_object())
        .and_then(|payload| trigger_workflow_source_var_keys(payload).ok())
        .unwrap_or_default();

    match trigger.action.kind {
        TriggerActionKind::BrowserCommand => TriggerActionExecutionInfo {
            kind: TriggerActionKind::BrowserCommand,
            command: trigger.action.command.clone(),
            workflow_name: None,
            workflow_path: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
        TriggerActionKind::Workflow => {
            let workflow_name = trigger
                .action
                .payload
                .as_ref()
                .and_then(|payload| payload.get("workflow_name"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let workflow_path = workflow_name
                .as_deref()
                .and_then(|name| resolve_named_workflow_path(rub_home, name).ok())
                .map(|path| path.display().to_string());
            let inline_step_count = trigger
                .action
                .payload
                .as_ref()
                .and_then(|payload| payload.get("steps"))
                .and_then(|steps| steps.as_array())
                .map(|steps| steps.len() as u32);
            TriggerActionExecutionInfo {
                kind: TriggerActionKind::Workflow,
                command: None,
                workflow_name,
                workflow_path,
                inline_step_count,
                vars,
                source_vars,
            }
        }
        TriggerActionKind::Provider => TriggerActionExecutionInfo {
            kind: TriggerActionKind::Provider,
            command: None,
            workflow_name: None,
            workflow_path: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
        TriggerActionKind::Script => TriggerActionExecutionInfo {
            kind: TriggerActionKind::Script,
            command: None,
            workflow_name: None,
            workflow_path: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
        TriggerActionKind::Webhook => TriggerActionExecutionInfo {
            kind: TriggerActionKind::Webhook,
            command: None,
            workflow_name: None,
            workflow_path: None,
            inline_step_count: None,
            vars,
            source_vars,
        },
    }
}

fn classify_trigger_error_status(code: ErrorCode) -> TriggerStatus {
    match code {
        ErrorCode::InvalidInput
        | ErrorCode::ElementNotFound
        | ErrorCode::ElementNotInteractable
        | ErrorCode::StaleSnapshot
        | ErrorCode::StaleIndex
        | ErrorCode::WaitTimeout
        | ErrorCode::TabNotFound
        | ErrorCode::NoMatchingOption
        | ErrorCode::FileNotFound
        | ErrorCode::AutomationPaused => TriggerStatus::Blocked,
        _ => TriggerStatus::Degraded,
    }
}

fn trigger_request_meta(
    trigger: &TriggerInfo,
    evidence: &TriggerEvidenceInfo,
    phase: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": trigger.id,
        "phase": phase,
        "source_tab_target_id": trigger.source_tab.target_id,
        "target_tab_target_id": trigger.target_tab.target_id,
        "condition_kind": trigger.condition.kind,
        "evidence": evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        classify_trigger_error_status, network_request_matches, parse_storage_area,
        readiness_matches, resolve_trigger_workflow_spec, storage_snapshot_matches,
        trigger_action_execution_info, trigger_action_summary, trigger_target_continuity_failure,
    };
    use rub_core::error::ErrorCode;
    use rub_core::locator::CanonicalLocator;
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameRuntimeInfo, NetworkRequestLifecycle,
        NetworkRequestRecord, OverlayState, ReadinessInfo, ReadinessStatus, RouteStability,
        TriggerActionKind, TriggerActionSpec, TriggerConditionKind, TriggerConditionSpec,
        TriggerInfo, TriggerMode, TriggerStatus, TriggerTabBindingInfo,
    };
    use rub_core::storage::{StorageArea, StorageSnapshot};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fs;

    fn trigger(kind: TriggerConditionKind) -> TriggerInfo {
        TriggerInfo {
            id: 1,
            status: TriggerStatus::Armed,
            mode: TriggerMode::Once,
            source_tab: TriggerTabBindingInfo {
                index: 0,
                target_id: "source".to_string(),
                url: "https://source.example".to_string(),
                title: "Source".to_string(),
            },
            target_tab: TriggerTabBindingInfo {
                index: 1,
                target_id: "target".to_string(),
                url: "https://target.example".to_string(),
                title: "Target".to_string(),
            },
            condition: TriggerConditionSpec {
                kind,
                locator: Some(CanonicalLocator::Selector {
                    css: "#ready".to_string(),
                    selection: None,
                }),
                text: Some("Approved".to_string()),
                url_pattern: Some("/events".to_string()),
                readiness_state: Some("stable".to_string()),
                method: Some("POST".to_string()),
                status_code: Some(200),
                storage_area: Some("local".to_string()),
                key: Some("token".to_string()),
                value: Some("abc".to_string()),
            },
            action: TriggerActionSpec {
                kind: TriggerActionKind::BrowserCommand,
                command: Some("click".to_string()),
                payload: Some(json!({ "selector": "#continue" })),
            },
            last_condition_evidence: None,
            consumed_evidence_fingerprint: None,
            last_action_result: None,
            unavailable_reason: None,
        }
    }

    #[test]
    fn readiness_matches_accepts_ready_alias_and_document_state() {
        let readiness = ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("complete".to_string()),
            blocking_signals: Vec::new(),
            degraded_reason: None,
        };

        assert!(readiness_matches(&readiness, "ready"));
        assert!(readiness_matches(&readiness, "stable"));
        assert!(readiness_matches(&readiness, "complete"));
        assert!(!readiness_matches(&readiness, "loading"));
    }

    #[test]
    fn network_request_matcher_respects_url_method_and_status() {
        let trigger = trigger(TriggerConditionKind::NetworkRequest);
        let record = NetworkRequestRecord {
            request_id: "req-1".to_string(),
            sequence: 2,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.test/api/events".to_string(),
            method: "POST".to_string(),
            tab_target_id: Some(trigger.source_tab.target_id.clone()),
            status: Some(200),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: None,
        };

        assert!(network_request_matches(&record, &trigger));
    }

    #[test]
    fn network_request_matcher_rejects_other_tabs() {
        let trigger = trigger(TriggerConditionKind::NetworkRequest);
        let record = NetworkRequestRecord {
            request_id: "req-2".to_string(),
            sequence: 3,
            lifecycle: NetworkRequestLifecycle::Completed,
            url: "https://example.test/api/events".to_string(),
            method: "POST".to_string(),
            tab_target_id: Some("background".to_string()),
            status: Some(200),
            request_headers: BTreeMap::new(),
            response_headers: BTreeMap::new(),
            request_body: None,
            response_body: None,
            original_url: None,
            rewritten_url: None,
            applied_rule_effects: Vec::new(),
            error_text: None,
            frame_id: None,
            resource_type: None,
            mime_type: None,
        };

        assert!(!network_request_matches(&record, &trigger));
    }

    #[test]
    fn storage_snapshot_matcher_respects_area_and_value() {
        let trigger = trigger(TriggerConditionKind::StorageValue);
        let snapshot = StorageSnapshot {
            origin: "https://example.test".to_string(),
            local_storage: BTreeMap::from([("token".to_string(), "abc".to_string())]),
            session_storage: BTreeMap::from([("token".to_string(), "session".to_string())]),
        };

        assert!(storage_snapshot_matches(&snapshot, &trigger).expect("storage match"));
    }

    #[test]
    fn parse_storage_area_accepts_local_and_session() {
        assert_eq!(
            parse_storage_area(Some("local")).expect("local"),
            Some(StorageArea::Local)
        );
        assert_eq!(
            parse_storage_area(Some("session")).expect("session"),
            Some(StorageArea::Session)
        );
        assert_eq!(parse_storage_area(None).expect("none"), None);
    }

    #[test]
    fn resolve_trigger_workflow_spec_loads_named_asset() {
        let home = std::env::temp_dir().join(format!(
            "rub-trigger-worker-workflow-{}",
            std::process::id()
        ));
        let workflows_dir = home.join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        let asset_path = workflows_dir.join("reply_flow.json");
        fs::write(&asset_path, r#"{"steps":[{"command":"doctor","args":{}}]}"#).unwrap();

        let payload =
            serde_json::Map::from_iter([("workflow_name".to_string(), json!("reply_flow"))]);
        let (spec, source) = resolve_trigger_workflow_spec(&payload, &home).unwrap();
        assert_eq!(spec, r#"{"steps":[{"command":"doctor","args":{}}]}"#);
        assert_eq!(source["kind"], "workflow");
        assert_eq!(source["name"], "reply_flow");
        assert_eq!(source["path"], asset_path.display().to_string());
        assert!(source.get("vars").is_none());

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn resolve_trigger_workflow_spec_projects_inline_steps() {
        let payload = serde_json::Map::from_iter([(
            "steps".to_string(),
            json!([
                { "command": "click", "args": { "selector": "#continue" } },
                { "command": "type", "args": { "selector": "#name", "text": "Ada" } }
            ]),
        )]);
        let (spec, source) =
            resolve_trigger_workflow_spec(&payload, std::path::Path::new("/tmp")).unwrap();
        let spec_json: serde_json::Value = serde_json::from_str(&spec).unwrap();
        assert_eq!(spec_json.as_array().unwrap().len(), 2);
        assert_eq!(source["kind"], "trigger_inline_workflow");
        assert_eq!(source["step_count"], 2);
        assert!(source.get("vars").is_none());
    }

    #[test]
    fn trigger_action_summary_prefers_named_workflow_projection() {
        let mut trigger = trigger(TriggerConditionKind::TextPresent);
        trigger.action.kind = TriggerActionKind::Workflow;
        trigger.action.command = None;
        trigger.action.payload = Some(json!({
            "workflow_name": "reply_flow"
        }));

        assert_eq!(trigger_action_summary(&trigger), "workflow 'reply_flow'");

        trigger.action.payload = Some(json!({
            "steps": [{ "command": "doctor", "args": {} }]
        }));
        assert_eq!(trigger_action_summary(&trigger), "inline workflow");
    }

    #[test]
    fn trigger_action_execution_info_projects_workflow_metadata() {
        let mut trigger = trigger(TriggerConditionKind::TextPresent);
        trigger.action.kind = TriggerActionKind::Workflow;
        trigger.action.command = None;
        trigger.action.payload = Some(json!({
            "workflow_name": "reply_flow",
            "vars": {
                "reply_name": "Ada",
                "target_url": "https://example.com"
            },
            "source_vars": {
                "prompt_text": {
                    "kind": "text",
                    "selector": "#prompt"
                }
            }
        }));
        let info = trigger_action_execution_info(&trigger, std::path::Path::new("/tmp/rub-home"));
        assert_eq!(info.kind, TriggerActionKind::Workflow);
        assert_eq!(info.workflow_name.as_deref(), Some("reply_flow"));
        assert_eq!(
            info.workflow_path.as_deref(),
            Some("/tmp/rub-home/workflows/reply_flow.json")
        );
        assert_eq!(info.inline_step_count, None);
        assert_eq!(
            info.vars,
            vec!["reply_name".to_string(), "target_url".to_string()]
        );
        assert_eq!(info.source_vars, vec!["prompt_text".to_string()]);
    }

    #[test]
    fn classify_trigger_error_status_preserves_blocked_vs_degraded_boundary() {
        assert_eq!(
            classify_trigger_error_status(ErrorCode::AutomationPaused),
            TriggerStatus::Blocked
        );
        assert_eq!(
            classify_trigger_error_status(ErrorCode::BrowserCrashed),
            TriggerStatus::Degraded
        );
    }

    #[test]
    fn target_continuity_fails_when_frame_runtime_is_stale() {
        let frame_runtime = FrameRuntimeInfo {
            status: FrameContextStatus::Stale,
            current_frame: Some(FrameContextInfo {
                frame_id: "missing-frame".to_string(),
                name: None,
                parent_frame_id: None,
                target_id: None,
                url: None,
                depth: 0,
                same_origin_accessible: None,
            }),
            primary_frame: None,
            frame_lineage: vec!["missing-frame".to_string()],
            degraded_reason: Some("selected_frame_not_found".to_string()),
        };
        let readiness = ReadinessInfo {
            status: ReadinessStatus::Active,
            route_stability: RouteStability::Stable,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("complete".to_string()),
            blocking_signals: Vec::new(),
            degraded_reason: None,
        };

        assert_eq!(
            trigger_target_continuity_failure("tab-target", &frame_runtime, &readiness),
            Some((
                "continuity_frame_unavailable",
                "Trigger target continuity fence failed: frame context became unavailable",
            ))
        );
    }

    #[test]
    fn target_continuity_fails_when_readiness_is_degraded() {
        let frame_runtime = FrameRuntimeInfo {
            status: FrameContextStatus::Top,
            current_frame: Some(FrameContextInfo {
                frame_id: "main-frame".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("tab-target".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            }),
            primary_frame: Some(FrameContextInfo {
                frame_id: "main-frame".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("tab-target".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            }),
            frame_lineage: vec!["main-frame".to_string()],
            degraded_reason: None,
        };
        let readiness = ReadinessInfo {
            status: ReadinessStatus::Degraded,
            route_stability: RouteStability::Transitioning,
            loading_present: true,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: Some("interactive".to_string()),
            blocking_signals: vec!["probe_timeout".to_string()],
            degraded_reason: Some("probe_timeout".to_string()),
        };

        assert_eq!(
            trigger_target_continuity_failure("tab-target", &frame_runtime, &readiness),
            Some((
                "continuity_readiness_degraded",
                "Trigger target continuity fence failed: readiness surface degraded",
            ))
        );
    }
}
