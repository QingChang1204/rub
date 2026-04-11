use std::time::Duration;

/// Automation workers poll twice per second for fresh trigger/orchestration
/// state.
pub(crate) const AUTOMATION_WORKER_POLL_INTERVAL_MS: u64 = 500;
pub(crate) const AUTOMATION_WORKER_POLL_INTERVAL: Duration =
    Duration::from_millis(AUTOMATION_WORKER_POLL_INTERVAL_MS);

/// Worker-owned automation reservations may wait long enough to span a normal
/// foreground command, but must still yield within one worker cycle so a single
/// queued rule does not stall all later evaluations.
#[cfg(test)]
pub(crate) const AUTOMATION_QUEUE_WAIT_BUDGET_MS: u64 = AUTOMATION_WORKER_POLL_INTERVAL_MS;
#[cfg(test)]
pub(crate) const AUTOMATION_QUEUE_WAIT_BUDGET: Duration =
    Duration::from_millis(AUTOMATION_QUEUE_WAIT_BUDGET_MS);

/// While an automation reservation is waiting on the shared FIFO, poll shutdown
/// frequently so draining still wins promptly.
pub(crate) const AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS: u64 = 50;
pub(crate) const AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL: Duration =
    Duration::from_millis(AUTOMATION_QUEUE_SHUTDOWN_POLL_INTERVAL_MS);
