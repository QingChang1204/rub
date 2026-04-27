//! Session management — SessionManager owns session lifecycle (AUTH.SessionManager).
//! RegistryFile owns session discoverability (AUTH.RegistryFile).

mod automation;
mod cache;
mod identity;
mod integration;
mod journal;
mod lifecycle;
mod protocol;
mod registry;
mod runtime;
mod scheduler;

use crate::dialogs::DialogRuntimeState;
use crate::downloads::DownloadRuntimeState;
use crate::frame_runtime::FrameRuntimeState;
use crate::handoff::HumanVerificationHandoffState;
use crate::history::{CommandHistoryProjection, CommandHistoryState};
use crate::interference::{InterferenceRecoveryContext, InterferenceRuntimeState};
use crate::locator_memo::{LocatorMemoRegistry, LocatorMemoTarget};
use crate::observatory::RuntimeObservatoryState;
use crate::orchestration_runtime::OrchestrationRuntimeState;
use crate::runtime_state_projection::RuntimeStateProjectionState;
use crate::storage_runtime::{StorageMutationRuntimeContext, StorageRuntimeState};
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

use self::identity::LaunchIdentity;
use self::integration::{derive_integration_runtime_status, derive_integration_runtime_surfaces};
pub use self::protocol::{
    BrowserSessionEvent, BrowserSessionEventSink, ReplayCommandClaim, ReplayFenceState,
};
pub(crate) use self::registry::registry_authority_snapshot_async;
/// Cross-crate re-exports: types needed by rub-cli callers and public registry operations.
pub use self::registry::{
    HardCutReleasePendingProof, RegistryAuthoritySnapshot, RegistryData, RegistryEntry,
    RegistryEntryLiveness, RegistryEntrySnapshot, RegistrySessionSnapshot, active_registry_entries,
    active_registry_entry_snapshots, authoritative_entry_by_session_name, check_profile_in_use,
    cleanup_projections, clear_hard_cut_release_pending_proof, deregister_session,
    hard_cut_release_pending_blocks_entry, latest_entry_by_session_name, new_session_id,
    promote_session_authority, read_hard_cut_release_pending_proof, read_registry,
    register_pending_session, register_session, register_session_with_displaced,
    registry_authority_snapshot, registry_entry_is_live_for_home,
    registry_entry_is_pending_startup_for_home, write_hard_cut_release_pending_proof,
    write_registry,
};
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
use rub_core::storage::{StorageMutationKind, StorageRuntimeInfo, StorageSnapshot};

/// Crate-internal re-exports: not part of the public API surface.
pub(crate) use self::registry::{
    ensure_rub_home, load_registry_for_home, registry_entry_has_runtime_authority_for_home,
    rfc3339_now, store_registry_for_home, validate_registry_entry_for_home, with_registry_lock,
};
#[cfg(test)]
pub(crate) use self::registry::{
    force_busy_registry_socket_probe_once_for_test, force_live_registry_socket_probe_once_for_test,
    force_probe_contract_failure_registry_socket_probe_once_for_test,
    force_protocol_incompatible_registry_socket_probe_once_for_test,
};
const SNAPSHOT_CACHE_LIMIT: usize = 128;
pub(crate) use self::protocol::{
    BROWSER_EVENT_PROGRESS_INGRESS_LIMIT, POST_COMMIT_PROJECTION_LIMIT,
    POST_COMMIT_PROJECTION_LIMIT_BYTES, PostCommitProjection, PostCommitProjectionQueue,
    REPLAY_CACHE_LIMIT, REPLAY_CACHE_LIMIT_BYTES, ReplayCacheEntry, ReplayInFlightEntry,
    ReplayProtocolState, ReplaySpentEntry,
};

/// Combined snapshot cache: LRU map + insertion order in one lock.
/// Replaces the previous split of `snapshot_cache: RwLock<HashMap>` +
/// `snapshot_order: Mutex<VecDeque>` which had a TOCTOU window between the two.
#[derive(Debug, Default)]
pub(crate) struct SnapshotCache {
    pub(crate) map: HashMap<String, Arc<Snapshot>>,
    pub(crate) order: VecDeque<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NetworkRequestBaseline {
    pub(crate) cursor: u64,
    /// Ingress-only drop count used by authoritative request-window fences.
    pub(crate) observed_ingress_drop_count: u64,
    pub(crate) primed: bool,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct PendingExternalDomChangeState {
    pub(crate) unknown_global: bool,
    pub(crate) target_ids: BTreeSet<String>,
}

impl PendingExternalDomChangeState {
    pub(crate) fn is_empty(&self) -> bool {
        !self.unknown_global && self.target_ids.is_empty()
    }

    pub(crate) fn mark(&mut self, target_id: Option<&str>) {
        if let Some(target_id) = target_id.filter(|target_id| !target_id.is_empty()) {
            self.target_ids.insert(target_id.to_string());
        } else {
            self.unknown_global = true;
        }
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.unknown_global |= other.unknown_global;
        self.target_ids.extend(other.target_ids);
    }

    pub(crate) fn affects_target(&self, target_id: Option<&str>) -> bool {
        if self.unknown_global {
            return true;
        }
        match target_id {
            Some(target_id) if !target_id.is_empty() => self.target_ids.contains(target_id),
            _ => !self.target_ids.is_empty(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct AutomationWorkerTelemetry {
    pub(crate) cycle_count: AtomicU64,
    pub(crate) last_cycle_uptime_ms: AtomicU64,
}

#[derive(Debug, Default)]
pub(crate) struct QueuePressureTelemetry {
    pub(crate) queue_timeout_count: AtomicU64,
    pub(crate) last_queue_timeout_uptime_ms: AtomicU64,
    pub(crate) max_in_flight_count: AtomicU32,
}

#[derive(Debug, Default)]
pub(crate) struct ShutdownDrainTelemetry {
    pub(crate) wait_loop_count: AtomicU64,
    pub(crate) soft_timeout_count: AtomicU64,
    pub(crate) connected_only_soft_release_count: AtomicU64,
    pub(crate) last_wait_uptime_ms: AtomicU64,
    pub(crate) last_soft_timeout_uptime_ms: AtomicU64,
    pub(crate) last_connected_only_soft_release_uptime_ms: AtomicU64,
    pub(crate) max_observed_in_flight_count: AtomicU32,
    pub(crate) max_observed_connected_client_count: AtomicU32,
    pub(crate) max_observed_pre_request_response_fence_count: AtomicU32,
}

#[derive(Debug, Default)]
pub(crate) struct BrowserEventIngressTelemetry {
    pub(crate) critical_pending_count: AtomicU32,
    pub(crate) critical_max_pending_count: AtomicU32,
    pub(crate) critical_soft_limit_cross_count: AtomicU64,
    pub(crate) critical_pressure_active: AtomicBool,
    pub(crate) last_critical_soft_limit_cross_uptime_ms: AtomicU64,
}

/// Per-session in-memory state. Authority for session lifecycle.
pub struct SessionState {
    pub session_id: String,
    pub session_name: String,
    pub dom_epoch: Arc<AtomicU64>,
    pending_external_dom_change: StdMutex<PendingExternalDomChangeState>,
    shutdown_requested: AtomicBool,
    shutdown_notify: Arc<Notify>,
    pub in_flight_count: AtomicU32,
    pub connected_client_count: AtomicU32,
    pub(crate) pre_request_response_fence_count: AtomicU32,
    trigger_worker_telemetry: AutomationWorkerTelemetry,
    orchestration_worker_telemetry: AutomationWorkerTelemetry,
    queue_pressure_telemetry: QueuePressureTelemetry,
    shutdown_drain_telemetry: ShutdownDrainTelemetry,
    browser_event_ingress_telemetry: BrowserEventIngressTelemetry,
    replay: StdMutex<ReplayProtocolState>,
    post_commit_projections: StdMutex<PostCommitProjectionQueue>,
    pub(crate) post_commit_followup_count: AtomicU32,
    post_commit_projection_drain: Mutex<()>,
    post_commit_projection_drain_scheduled: AtomicBool,
    post_commit_journal_append: Mutex<()>,
    post_commit_journal_failures: AtomicU64,
    #[cfg(test)]
    post_commit_journal_force_failure_once: AtomicBool,
    #[cfg(test)]
    post_commit_journal_blocked: AtomicBool,
    #[cfg(test)]
    post_commit_journal_block_notify: Arc<Notify>,
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
    browser_event_ingress_drop_count: AtomicU64,
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
            pending_external_dom_change: StdMutex::new(PendingExternalDomChangeState::default()),
            shutdown_requested: AtomicBool::new(false),
            shutdown_notify: Arc::new(Notify::new()),
            in_flight_count: AtomicU32::new(0),
            connected_client_count: AtomicU32::new(0),
            pre_request_response_fence_count: AtomicU32::new(0),
            trigger_worker_telemetry: AutomationWorkerTelemetry::default(),
            orchestration_worker_telemetry: AutomationWorkerTelemetry::default(),
            queue_pressure_telemetry: QueuePressureTelemetry::default(),
            shutdown_drain_telemetry: ShutdownDrainTelemetry::default(),
            browser_event_ingress_telemetry: BrowserEventIngressTelemetry::default(),
            replay: StdMutex::new(ReplayProtocolState::default()),
            post_commit_projections: StdMutex::new(PostCommitProjectionQueue::default()),
            post_commit_followup_count: AtomicU32::new(0),
            post_commit_projection_drain: Mutex::new(()),
            post_commit_projection_drain_scheduled: AtomicBool::new(false),
            post_commit_journal_append: Mutex::new(()),
            post_commit_journal_failures: AtomicU64::new(0),
            #[cfg(test)]
            post_commit_journal_force_failure_once: AtomicBool::new(false),
            #[cfg(test)]
            post_commit_journal_blocked: AtomicBool::new(false),
            #[cfg(test)]
            post_commit_journal_block_notify: Arc::new(Notify::new()),
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
            browser_event_ingress_drop_count: AtomicU64::new(0),
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
}

#[cfg(test)]
mod tests;
