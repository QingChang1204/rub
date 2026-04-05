//! Session management — SessionManager owns session lifecycle (AUTH.SessionManager).
//! RegistryFile owns session discoverability (AUTH.RegistryFile).

mod automation;
mod cache;
mod integration;
mod registry;
mod runtime;

use crate::dialogs::DialogRuntimeState;
use crate::downloads::DownloadRuntimeState;
use crate::frame_runtime::FrameRuntimeState;
use crate::handoff::HumanVerificationHandoffState;
use crate::history::{CommandHistoryProjection, CommandHistoryState};
use crate::interference::{InterferenceRecoveryContext, InterferenceRuntimeState};
use crate::locator_memo::{LocatorMemoRegistry, LocatorMemoTarget};
use crate::observatory::RuntimeObservatoryState;
use crate::orchestration_runtime::OrchestrationRuntimeState;
use crate::rub_paths::RubPaths;
use crate::runtime_state_projection::RuntimeStateProjectionState;
use crate::storage_runtime::StorageRuntimeState;
use crate::takeover::TakeoverRuntimeState;
use crate::trigger::TriggerRuntimeState;
use crate::workflow_capture::{WorkflowCaptureProjection, WorkflowCaptureState};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use tokio::sync::Notify;
use tokio::sync::{Mutex, RwLock};

use self::integration::{derive_integration_runtime_status, derive_integration_runtime_surfaces};
use rub_core::model::{
    ConnectionTarget, ConsoleErrorEvent, DialogKind, DialogRuntimeInfo, DialogRuntimeStatus,
    DownloadEntry, DownloadEvent, DownloadMode, DownloadRuntimeInfo, DownloadRuntimeStatus,
    DownloadState, FrameInventoryEntry, FrameRuntimeInfo, HumanVerificationHandoffInfo,
    IntegrationRuntimeInfo, IntegrationRuntimeStatus, InterferenceRecoveryAction,
    InterferenceRecoveryResult, InterferenceRuntimeInfo, NetworkFailureEvent,
    NetworkRequestLifecycle, NetworkRequestRecord, NetworkRule, NetworkRuleSpec, NetworkRuleStatus,
    OrchestrationResultInfo, OrchestrationRuleInfo, OrchestrationRuleStatus,
    OrchestrationTraceProjection, PageErrorEvent, ReadinessInfo, ReadinessStatus,
    RequestSummaryEvent, RuntimeObservatoryEvent, RuntimeObservatoryInfo, RuntimeObservatoryStatus,
    RuntimeStateSnapshot, Snapshot, StateInspectorInfo, StateInspectorStatus, TabInfo,
    TakeoverRuntimeInfo, TakeoverTransitionKind, TakeoverTransitionResult,
};
use rub_core::storage::{StorageArea, StorageMutationKind, StorageRuntimeInfo, StorageSnapshot};
use rub_ipc::codec::MAX_FRAME_BYTES;

/// Cross-crate re-exports: types needed by rub-cli callers and public registry operations.
pub use self::registry::{
    RegistryAuthoritySnapshot, RegistryData, RegistryEntry, RegistryEntryLiveness,
    RegistryEntrySnapshot, RegistrySessionSnapshot, active_registry_entries,
    authoritative_entry_by_session_name, check_profile_in_use, cleanup_projections,
    deregister_session, latest_entry_by_session_name, new_session_id, promote_session_authority,
    read_registry, register_pending_session, register_session, register_session_with_displaced,
    registry_authority_snapshot, registry_entry_is_live_for_home,
    registry_entry_is_pending_startup_for_home, write_registry,
};

/// Crate-internal re-exports: not part of the public API surface.
pub(crate) use self::registry::{ensure_rub_home, rfc3339_now};

const REPLAY_CACHE_LIMIT: usize = 1000;
const REPLAY_CACHE_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 8;
const SNAPSHOT_CACHE_LIMIT: usize = 128;
const POST_COMMIT_PROJECTION_LIMIT: usize = 256;
const POST_COMMIT_PROJECTION_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 4;

#[derive(Debug, Clone)]
pub(crate) struct ReplayCacheEntry {
    pub(crate) fingerprint: String,
    pub(crate) response: rub_ipc::protocol::IpcResponse,
    pub(crate) approx_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PostCommitProjection {
    pub(crate) request: rub_ipc::protocol::IpcRequest,
    pub(crate) response: rub_ipc::protocol::IpcResponse,
    pub(crate) approx_bytes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PostCommitProjectionQueue {
    pub(crate) entries: VecDeque<PostCommitProjection>,
    pub(crate) total_bytes: usize,
    pub(crate) dropped_before_projection: u64,
}

/// Combined snapshot cache: LRU map + insertion order in one lock.
/// Replaces the previous split of `snapshot_cache: RwLock<HashMap>` +
/// `snapshot_order: Mutex<VecDeque>` which had a TOCTOU window between the two.
#[derive(Debug, Default)]
pub(crate) struct SnapshotCache {
    pub(crate) map: HashMap<String, Arc<Snapshot>>,
    pub(crate) order: VecDeque<String>,
}

/// Atomic launch-time identity snapshot: always read/written as a unit to
/// prevent the TOCTOU window that existed when the two fields had separate locks.
#[derive(Debug, Clone, Default)]
pub(crate) struct LaunchIdentity {
    pub(crate) attachment_identity: Option<String>,
    pub(crate) connection_target: Option<ConnectionTarget>,
}

#[derive(Debug, Default)]
struct ReplayProtocolState {
    cache: HashMap<String, ReplayCacheEntry>,
    order: VecDeque<String>,
    in_flight: HashMap<String, ReplayInFlightEntry>,
    total_bytes: usize,
}

#[derive(Debug, Clone)]
struct ReplayInFlightEntry {
    fingerprint: String,
    sender: tokio::sync::watch::Sender<ReplayFenceState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayFenceState {
    InFlight,
    Released,
}

pub enum BrowserSessionEvent {
    DialogRuntime {
        browser_sequence: u64,
        generation: u64,
        status: DialogRuntimeStatus,
        degraded_reason: Option<String>,
    },
    DialogOpened {
        browser_sequence: u64,
        generation: u64,
        kind: DialogKind,
        message: String,
        url: String,
        tab_target_id: Option<String>,
        frame_id: Option<String>,
        default_prompt: Option<String>,
        has_browser_handler: bool,
    },
    DialogClosed {
        browser_sequence: u64,
        generation: u64,
        accepted: bool,
        user_input: String,
    },
    DownloadRuntime {
        browser_sequence: u64,
        generation: u64,
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
        degraded_reason: Option<String>,
    },
    DownloadStarted {
        browser_sequence: u64,
        generation: u64,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    },
    DownloadProgress {
        browser_sequence: u64,
        generation: u64,
        guid: String,
        state: DownloadState,
        received_bytes: u64,
        total_bytes: Option<u64>,
        final_path: Option<String>,
    },
}

impl BrowserSessionEvent {
    pub fn browser_sequence(&self) -> u64 {
        match self {
            Self::DialogRuntime {
                browser_sequence, ..
            }
            | Self::DialogOpened {
                browser_sequence, ..
            }
            | Self::DialogClosed {
                browser_sequence, ..
            }
            | Self::DownloadRuntime {
                browser_sequence, ..
            }
            | Self::DownloadStarted {
                browser_sequence, ..
            }
            | Self::DownloadProgress {
                browser_sequence, ..
            } => *browser_sequence,
        }
    }
}

#[derive(Clone)]
pub struct BrowserSessionEventSink {
    state: Arc<SessionState>,
    tx: tokio::sync::mpsc::UnboundedSender<BrowserSessionEvent>,
}

/// Per-session in-memory state. Authority for session lifecycle.
pub struct SessionState {
    pub session_id: String,
    pub session_name: String,
    pub dom_epoch: Arc<AtomicU64>,
    pending_external_dom_change: AtomicBool,
    shutdown_requested: AtomicBool,
    pub in_flight_count: AtomicU32,
    pub connected_client_count: AtomicU32,
    replay: StdMutex<ReplayProtocolState>,
    post_commit_projections: StdMutex<PostCommitProjectionQueue>,
    post_commit_projection_drain: Mutex<()>,
    post_commit_projection_drain_scheduled: AtomicBool,
    #[cfg(test)]
    post_commit_projection_drain_spawn_count: AtomicU64,
    history: RwLock<CommandHistoryState>,
    workflow_capture: RwLock<WorkflowCaptureState>,
    locator_memo: RwLock<LocatorMemoRegistry>,
    snapshot_cache: RwLock<SnapshotCache>,
    integration_runtime: RwLock<IntegrationRuntimeInfo>,
    dialogs: RwLock<DialogRuntimeState>,
    next_dialog_event_sequence: AtomicU64,
    downloads: RwLock<DownloadRuntimeState>,
    next_download_event_sequence: AtomicU64,
    frame_runtime: RwLock<FrameRuntimeState>,
    storage_runtime: RwLock<StorageRuntimeState>,
    observatory: RwLock<RuntimeObservatoryState>,
    network_request_notify: Arc<Notify>,
    browser_event_notify: Arc<Notify>,
    observatory_drop_count: AtomicU64,
    network_request_ingress_drop_count: AtomicU64,
    next_browser_event_sequence: AtomicU64,
    committed_browser_event_sequence: AtomicU64,
    committed_browser_event_backlog: StdMutex<BTreeSet<u64>>,
    orchestration_runtime: RwLock<OrchestrationRuntimeState>,
    next_orchestration_runtime_sequence: AtomicU64,
    runtime_state: RwLock<RuntimeStateProjectionState>,
    next_runtime_state_sequence: AtomicU64,
    handoff: RwLock<HumanVerificationHandoffState>,
    takeover: RwLock<TakeoverRuntimeState>,
    trigger_runtime: RwLock<TriggerRuntimeState>,
    interference: RwLock<InterferenceRuntimeState>,
    next_network_rule_id: AtomicU32,
    next_trigger_id: AtomicU32,
    next_orchestration_id: AtomicU32,
    pub rub_home: PathBuf,
    pub user_data_dir: Option<String>,
    /// Launch-time identity — single RwLock prevents TOCTOU between the two fields.
    launch_identity: RwLock<LaunchIdentity>,
    started_at: std::time::Instant,
}

impl SessionState {
    pub fn new(name: impl Into<String>, rub_home: PathBuf, user_data_dir: Option<String>) -> Self {
        Self::new_with_id(
            name,
            self::registry::new_session_id(),
            rub_home,
            user_data_dir,
        )
    }

    pub fn new_with_id(
        name: impl Into<String>,
        session_id: impl Into<String>,
        rub_home: PathBuf,
        user_data_dir: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            session_name: name.into(),
            dom_epoch: Arc::new(AtomicU64::new(0)),
            pending_external_dom_change: AtomicBool::new(false),
            shutdown_requested: AtomicBool::new(false),
            in_flight_count: AtomicU32::new(0),
            connected_client_count: AtomicU32::new(0),
            replay: StdMutex::new(ReplayProtocolState::default()),
            post_commit_projections: StdMutex::new(PostCommitProjectionQueue::default()),
            post_commit_projection_drain: Mutex::new(()),
            post_commit_projection_drain_scheduled: AtomicBool::new(false),
            #[cfg(test)]
            post_commit_projection_drain_spawn_count: AtomicU64::new(0),
            history: RwLock::new(CommandHistoryState::default()),
            workflow_capture: RwLock::new(WorkflowCaptureState::default()),
            locator_memo: RwLock::new(LocatorMemoRegistry::default()),
            snapshot_cache: RwLock::new(SnapshotCache::default()),
            integration_runtime: RwLock::new(IntegrationRuntimeInfo::default()),
            dialogs: RwLock::new(DialogRuntimeState::default()),
            next_dialog_event_sequence: AtomicU64::new(0),
            downloads: RwLock::new(DownloadRuntimeState::default()),
            next_download_event_sequence: AtomicU64::new(0),
            frame_runtime: RwLock::new(FrameRuntimeState::default()),
            storage_runtime: RwLock::new(StorageRuntimeState::default()),
            observatory: RwLock::new(RuntimeObservatoryState::default()),
            network_request_notify: Arc::new(Notify::new()),
            browser_event_notify: Arc::new(Notify::new()),
            observatory_drop_count: AtomicU64::new(0),
            network_request_ingress_drop_count: AtomicU64::new(0),
            next_browser_event_sequence: AtomicU64::new(0),
            committed_browser_event_sequence: AtomicU64::new(0),
            committed_browser_event_backlog: StdMutex::new(BTreeSet::new()),
            orchestration_runtime: RwLock::new(OrchestrationRuntimeState::default()),
            next_orchestration_runtime_sequence: AtomicU64::new(1),
            runtime_state: RwLock::new(RuntimeStateProjectionState::default()),
            next_runtime_state_sequence: AtomicU64::new(1),
            handoff: RwLock::new(HumanVerificationHandoffState::default()),
            takeover: RwLock::new(TakeoverRuntimeState::default()),
            trigger_runtime: RwLock::new(TriggerRuntimeState::default()),
            interference: RwLock::new(InterferenceRuntimeState::default()),
            next_network_rule_id: AtomicU32::new(1),
            next_trigger_id: AtomicU32::new(1),
            next_orchestration_id: AtomicU32::new(1),
            rub_home,
            user_data_dir,
            launch_identity: RwLock::new(LaunchIdentity::default()),
            started_at: std::time::Instant::now(),
        }
    }

    /// Return an atomic snapshot of both launch-time identity fields.
    pub(crate) async fn launch_identity(&self) -> LaunchIdentity {
        self.launch_identity.read().await.clone()
    }

    pub async fn set_attachment_identity(&self, identity: Option<String>) {
        self.launch_identity.write().await.attachment_identity = identity;
    }

    pub async fn attachment_identity(&self) -> Option<String> {
        self.launch_identity
            .read()
            .await
            .attachment_identity
            .clone()
    }

    pub async fn set_connection_target(&self, target: Option<ConnectionTarget>) {
        self.launch_identity.write().await.connection_target = target;
    }

    pub async fn connection_target(&self) -> Option<ConnectionTarget> {
        self.launch_identity.read().await.connection_target.clone()
    }

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

#[derive(Debug, Clone)]
pub enum ReplayCommandClaim {
    Owner,
    Cached(Box<rub_ipc::protocol::IpcResponse>),
    Wait(tokio::sync::watch::Receiver<ReplayFenceState>),
    Conflict,
}

#[cfg(test)]
mod tests;
