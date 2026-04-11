use std::sync::Arc;
use std::time::Instant;

use crate::session::SessionState;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TransactionDeadline {
    started_at: Instant,
    pub(super) timeout_ms: u64,
}

impl TransactionDeadline {
    pub(super) fn new(timeout_ms: u64) -> Self {
        Self {
            started_at: Instant::now(),
            timeout_ms,
        }
    }

    pub(super) fn elapsed_ms(self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    pub(super) fn remaining_ms(self) -> u64 {
        self.timeout_ms.saturating_sub(self.elapsed_ms())
    }

    pub(super) fn remaining_duration(self) -> Option<std::time::Duration> {
        let remaining_ms = self.remaining_ms();
        if remaining_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(remaining_ms))
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum PendingExternalDomCommit {
    #[default]
    Clear,
    Preserve,
}

#[derive(Debug)]
pub(crate) struct CommandDispatchOutcome {
    data: serde_json::Value,
    pending_external_dom_commit: PendingExternalDomCommit,
}

impl CommandDispatchOutcome {
    pub(super) fn new(data: serde_json::Value) -> Self {
        Self {
            data,
            pending_external_dom_commit: PendingExternalDomCommit::Clear,
        }
    }

    pub(super) fn with_pending_external_dom_commit(
        mut self,
        pending_external_dom_commit: PendingExternalDomCommit,
    ) -> Self {
        self.pending_external_dom_commit = pending_external_dom_commit;
        self
    }

    pub(super) fn into_parts(self) -> (serde_json::Value, PendingExternalDomCommit) {
        (self.data, self.pending_external_dom_commit)
    }
}

impl From<serde_json::Value> for CommandDispatchOutcome {
    fn from(data: serde_json::Value) -> Self {
        Self::new(data)
    }
}

pub(crate) struct RouterTransactionGuard<'a> {
    pub(crate) _permit: tokio::sync::SemaphorePermit<'a>,
    pub(crate) state: Arc<SessionState>,
}

impl Drop for RouterTransactionGuard<'_> {
    fn drop(&mut self) {
        self.state
            .in_flight_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) struct OwnedRouterTransactionGuard {
    pub(crate) _permit: tokio::sync::OwnedSemaphorePermit,
    pub(crate) state: Arc<SessionState>,
}

impl Drop for OwnedRouterTransactionGuard {
    fn drop(&mut self) {
        self.state
            .in_flight_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
