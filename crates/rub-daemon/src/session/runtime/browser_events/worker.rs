use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

pub(super) async fn run_browser_event_worker(
    state: Arc<SessionState>,
    mut critical_rx: tokio::sync::mpsc::UnboundedReceiver<BrowserSessionEvent>,
    mut progress_rx: tokio::sync::mpsc::Receiver<BrowserSessionEvent>,
    progress_overflow_coordination: Arc<std::sync::Mutex<()>>,
    progress_overflow_latched: Arc<std::sync::atomic::AtomicBool>,
    progress_overflow_latched_generation: Arc<std::sync::atomic::AtomicU64>,
    progress_overflow_latched_sequence: Arc<std::sync::atomic::AtomicU64>,
) {
    let mut pending = BTreeMap::<u64, BrowserSessionEvent>::new();
    let mut next_sequence = state.committed_browser_event_cursor().saturating_add(1);
    let mut critical_open = true;
    let mut progress_open = true;

    loop {
        drain_ready_browser_events(
            &state,
            &mut pending,
            &mut next_sequence,
            &progress_overflow_coordination,
            &progress_overflow_latched,
            &progress_overflow_latched_generation,
            &progress_overflow_latched_sequence,
        )
        .await;
        if !critical_open && !progress_open && pending.is_empty() {
            return;
        }

        tokio::select! {
            event = async {
                if critical_open {
                    critical_rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => match event {
                Some(event) => {
                    state.record_critical_browser_event_dequeued();
                    insert_pending_browser_event(&state, &mut pending, event)
                }
                None => critical_open = false,
            },
            event = async {
                if progress_open {
                    progress_rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => match event {
                Some(event) => insert_pending_browser_event(&state, &mut pending, event),
                None => progress_open = false,
            }
        }
    }
}

fn insert_pending_browser_event(
    state: &Arc<SessionState>,
    pending: &mut BTreeMap<u64, BrowserSessionEvent>,
    event: BrowserSessionEvent,
) {
    let browser_sequence = event.browser_sequence();
    if browser_sequence <= state.committed_browser_event_cursor() {
        return;
    }
    pending.insert(browser_sequence, event);
}

pub(super) async fn drain_ready_browser_events(
    state: &Arc<SessionState>,
    pending: &mut BTreeMap<u64, BrowserSessionEvent>,
    next_sequence: &mut u64,
    progress_overflow_coordination: &Arc<std::sync::Mutex<()>>,
    progress_overflow_latched: &Arc<std::sync::atomic::AtomicBool>,
    progress_overflow_latched_generation: &Arc<std::sync::atomic::AtomicU64>,
    progress_overflow_latched_sequence: &Arc<std::sync::atomic::AtomicU64>,
) {
    loop {
        let committed = state.committed_browser_event_cursor();
        let expected = committed.saturating_add(1);
        // Committed holes are authoritative. If enqueue-time overflow handling already marked a
        // sequence committed, the worker must advance to the next uncommitted sequence instead of
        // waiting forever for an event that was intentionally dropped before it reached pending.
        if expected > *next_sequence {
            *next_sequence = expected;
        }
        let Some(event) = pending.remove(next_sequence) else {
            return;
        };
        let outcome = state.apply_browser_session_event(event).await;
        if let Some((clear_generation, clear_sequence)) = outcome.clear_progress_overflow_latch {
            let _coordination = progress_overflow_coordination
                .lock()
                .expect("progress overflow coordination lock poisoned");
            let latched_generation = progress_overflow_latched_generation.load(Ordering::SeqCst);
            let latched_sequence = progress_overflow_latched_sequence.load(Ordering::SeqCst);
            if progress_overflow_latched.load(Ordering::SeqCst)
                && latched_generation == clear_generation
                && clear_sequence > latched_sequence
            {
                progress_overflow_latched.store(false, Ordering::SeqCst);
                progress_overflow_latched_generation.store(0, Ordering::SeqCst);
                progress_overflow_latched_sequence.store(0, Ordering::SeqCst);
            }
        }
        state.record_browser_event_commit(outcome.browser_sequence);
        *next_sequence = next_sequence.saturating_add(1);
    }
}
