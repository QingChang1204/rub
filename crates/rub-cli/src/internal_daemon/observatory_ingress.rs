use rub_core::model::{
    ConsoleErrorEvent, NetworkFailureEvent, ObservedNetworkRequestRecord, PageErrorEvent,
    RequestSummaryEvent,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) enum ObservatoryMutation {
    ConsoleError(ConsoleErrorEvent),
    PageError(PageErrorEvent),
    NetworkFailure(NetworkFailureEvent),
    RequestSummary(RequestSummaryEvent),
}

pub(super) const OBSERVATORY_INGRESS_LIMIT: usize = 1_024;
pub(super) const NETWORK_REQUEST_INGRESS_LIMIT: usize = 1_024;

pub(super) fn enqueue_observatory_mutation(
    tx: &tokio::sync::mpsc::Sender<ObservatoryMutation>,
    mutation: ObservatoryMutation,
    state: &Arc<rub_daemon::session::SessionState>,
    overflowed: &Arc<AtomicBool>,
) {
    if tx.try_send(mutation).is_ok() {
        return;
    }

    let _ = state.record_observatory_ingress_overflow();

    if !overflowed.swap(true, Ordering::SeqCst) {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .mark_observatory_degraded("observatory_ingress_overflow")
                .await;
        });
    }
}

pub(super) fn enqueue_network_request_record(
    tx: &tokio::sync::mpsc::Sender<Box<ObservedNetworkRequestRecord>>,
    record: ObservedNetworkRequestRecord,
    state: &Arc<rub_daemon::session::SessionState>,
    overflowed: &Arc<AtomicBool>,
) {
    if tx.try_send(Box::new(record)).is_ok() {
        return;
    }

    let _ = state.record_network_request_ingress_overflow();
    state.network_request_notifier().notify_waiters();

    if !overflowed.swap(true, Ordering::SeqCst) {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .mark_observatory_degraded("network_request_ingress_overflow")
                .await;
        });
    }
}
