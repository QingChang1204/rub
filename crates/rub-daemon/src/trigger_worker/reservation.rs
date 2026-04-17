use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{TabInfo, TriggerEvidenceInfo, TriggerInfo, TriggerStatus};
use tokio::task::JoinHandle;

use crate::router::{DaemonRouter, OwnedRouterTransactionGuard};
use crate::runtime_refresh::refresh_live_trigger_runtime;
use crate::session::SessionState;

use super::TriggerWorkerEntry;
use super::condition::{
    TriggerConditionState, TriggerNetworkProgress, TriggeredTriggerCondition,
    commit_trigger_network_progress, load_trigger_condition_state,
};

pub(super) struct ReservedTriggerExecution {
    pub(super) trigger: TriggerInfo,
    pub(super) tabs: Vec<TabInfo>,
    pub(super) evidence: TriggerEvidenceInfo,
    pub(super) evidence_fingerprint: String,
    pub(super) network_progress: Option<TriggerNetworkProgress>,
    pub(super) _transaction: OwnedRouterTransactionGuard,
}

pub(super) struct PendingTriggerConditionPolicy {
    pub(super) preserved_triggered: Option<TriggeredTriggerCondition>,
    pub(super) requires_revalidation_after_queue: bool,
    pub(super) rule_semantics_fingerprint: String,
}

pub(super) struct PendingTriggerReservation {
    pub(super) attempt_id: u64,
    pub(super) fallback_network_progress: Option<TriggerNetworkProgress>,
    pub(super) condition_policy: PendingTriggerConditionPolicy,
    pub(super) task: JoinHandle<()>,
}

pub(super) struct TriggerReservationCompletion {
    pub(super) trigger_id: u32,
    pub(super) attempt_id: u64,
    pub(super) result: Result<OwnedRouterTransactionGuard, ErrorEnvelope>,
}

pub(super) fn spawn_trigger_reservation(
    router: Arc<DaemonRouter>,
    state: Arc<SessionState>,
    trigger_id: u32,
    attempt_id: u64,
    fallback_network_progress: Option<TriggerNetworkProgress>,
    condition_policy: PendingTriggerConditionPolicy,
    completions: tokio::sync::mpsc::UnboundedSender<TriggerReservationCompletion>,
) -> PendingTriggerReservation {
    let task = tokio::spawn(async move {
        let result = router
            .begin_automation_reservation_transaction_owned(&state, "trigger_worker")
            .await;
        let _ = completions.send(TriggerReservationCompletion {
            trigger_id,
            attempt_id,
            result,
        });
    });
    PendingTriggerReservation {
        attempt_id,
        fallback_network_progress,
        condition_policy,
        task,
    }
}

pub(super) async fn complete_trigger_reservation(
    state: &Arc<SessionState>,
    browser: &Arc<dyn rub_core::port::BrowserPort>,
    trigger_id: u32,
    worker: &mut TriggerWorkerEntry,
    transaction: OwnedRouterTransactionGuard,
    fallback_network_progress: Option<TriggerNetworkProgress>,
    condition_policy: PendingTriggerConditionPolicy,
) -> Result<Option<ReservedTriggerExecution>, ErrorEnvelope> {
    let live_trigger = match state.trigger_rule(trigger_id).await {
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

    let live_requires_revalidation =
        super::trigger_condition_requires_revalidation_after_queue(&live_trigger);
    if live_requires_revalidation != condition_policy.requires_revalidation_after_queue {
        drop(transaction);
        return Ok(None);
    }
    if !live_requires_revalidation
        && super::trigger_rule_semantics_fingerprint(&live_trigger)
            != condition_policy.rule_semantics_fingerprint
    {
        drop(transaction);
        return Ok(None);
    }

    let triggered_after_queue = if live_requires_revalidation {
        let live_condition =
            match load_trigger_condition_state(browser, state, &live_tabs, &live_trigger, worker)
                .await
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
            commit_trigger_network_progress(worker, fallback_network_progress);
            drop(transaction);
            return Ok(None);
        };
        triggered_after_queue
    } else {
        let Some(preserved_triggered) = condition_policy.preserved_triggered else {
            drop(transaction);
            return Err(ErrorEnvelope::new(
                ErrorCode::InternalError,
                "trigger reservation lost preserved network_request evidence before queue completion",
            ));
        };
        preserved_triggered
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
                .or(fallback_network_progress),
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
            .or(fallback_network_progress),
        _transaction: transaction,
    }))
}
