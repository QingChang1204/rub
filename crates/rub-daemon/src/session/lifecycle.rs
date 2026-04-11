use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Notify;

use crate::rub_paths::RubPaths;

use super::SessionState;

impl SessionState {
    /// Get Arc reference to dom_epoch for sharing with adapter.
    pub fn epoch_ref(&self) -> Arc<AtomicU64> {
        self.dom_epoch.clone()
    }

    /// Increment dom_epoch and return the new value (INV-001).
    pub fn increment_epoch(&self) -> u64 {
        self.dom_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Get current dom_epoch.
    pub fn current_epoch(&self) -> u64 {
        self.dom_epoch.load(Ordering::SeqCst)
    }

    pub fn allocate_browser_event_sequence(&self) -> u64 {
        self.next_browser_event_sequence
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1)
    }

    pub fn browser_event_cursor(&self) -> u64 {
        self.next_browser_event_sequence.load(Ordering::SeqCst)
    }

    pub fn committed_browser_event_cursor(&self) -> u64 {
        self.committed_browser_event_sequence.load(Ordering::SeqCst)
    }

    pub fn record_browser_event_commit(&self, sequence: u64) {
        let mut backlog = self
            .committed_browser_event_backlog
            .lock()
            .expect("browser event commit backlog mutex should not be poisoned");
        let current = self.committed_browser_event_sequence.load(Ordering::SeqCst);
        if sequence <= current {
            self.browser_event_notify.notify_waiters();
            return;
        }
        backlog.insert(sequence);
        let mut next = current;
        while backlog.remove(&next.saturating_add(1)) {
            next = next.saturating_add(1);
        }
        if next > current {
            self.committed_browser_event_sequence
                .store(next, Ordering::SeqCst);
        }
        self.browser_event_notify.notify_waiters();
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn uptime_millis(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// External CDP events only advance the epoch when no command transaction owns the commit.
    pub fn observe_external_dom_change(&self) -> Option<u64> {
        if self.in_flight_count.load(Ordering::SeqCst) == 0 {
            Some(self.increment_epoch())
        } else {
            self.pending_external_dom_change
                .store(true, Ordering::SeqCst);
            None
        }
    }

    /// Drain the pending external DOM-change marker captured during an in-flight transaction.
    pub fn take_pending_external_dom_change(&self) -> bool {
        self.pending_external_dom_change
            .swap(false, Ordering::SeqCst)
    }

    /// Check whether an in-flight transaction observed an external DOM change.
    pub fn has_pending_external_dom_change(&self) -> bool {
        self.pending_external_dom_change.load(Ordering::SeqCst)
    }

    /// Restore the pending external DOM-change marker when a transaction cannot publish a stable result.
    pub fn mark_pending_external_dom_change(&self) {
        self.pending_external_dom_change
            .store(true, Ordering::SeqCst);
    }

    /// Clear any pending external DOM-change marker after a command-owned epoch commit.
    pub fn clear_pending_external_dom_change(&self) {
        self.pending_external_dom_change
            .store(false, Ordering::SeqCst);
    }

    /// Mark the session as shutting down so new command transactions are rejected.
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::SeqCst);
    }

    /// Whether the session is currently draining for shutdown.
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Check if session is idle for auto-upgrade (INV-007).
    pub fn is_base_idle_for_upgrade(&self) -> bool {
        self.in_flight_count.load(Ordering::SeqCst) == 0
            && self.connected_client_count.load(Ordering::SeqCst) <= 1
    }

    /// Socket path for this session.
    pub fn socket_path(&self) -> PathBuf {
        RubPaths::new(&self.rub_home)
            .session_runtime(&self.session_name, &self.session_id)
            .socket_path()
    }

    /// PID file path for this session.
    pub fn pid_path(&self) -> PathBuf {
        RubPaths::new(&self.rub_home)
            .session_runtime(&self.session_name, &self.session_id)
            .pid_path()
    }

    /// Lock file path for this session.
    pub fn lock_path(&self) -> PathBuf {
        RubPaths::new(&self.rub_home)
            .session_runtime(&self.session_name, &self.session_id)
            .lock_path()
    }

    pub(crate) fn browser_event_notifier(&self) -> Arc<Notify> {
        self.browser_event_notify.clone()
    }
}
