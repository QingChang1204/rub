use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{TabInfo, TriggerEvidenceInfo, TriggerInfo, TriggerStatus};

use crate::router::{DaemonRouter, RouterTransactionGuard};
use crate::runtime_refresh::refresh_live_trigger_runtime;
use crate::session::SessionState;

use super::condition::{
    TriggerConditionState, TriggerNetworkProgress, TriggeredTriggerCondition,
    commit_trigger_network_progress, load_trigger_condition_state,
};
use super::{TRIGGER_AUTOMATION_TRANSACTION_TIMEOUT_MS, TriggerWorkerEntry};

pub(super) struct ReservedTriggerExecution<'a> {
    pub(super) trigger: TriggerInfo,
    pub(super) tabs: Vec<TabInfo>,
    pub(super) evidence: TriggerEvidenceInfo,
    pub(super) evidence_fingerprint: String,
    pub(super) network_progress: Option<TriggerNetworkProgress>,
    pub(super) _transaction: RouterTransactionGuard<'a>,
}

pub(super) async fn reserve_trigger_execution<'a>(
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
