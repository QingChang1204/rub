//! Daemon control — auto-start, stale detection, socket connect, version upgrade.
//!
//! # Logical Structure
//!
//! This file contains six responsibility domains. The startup lifecycle slice
//! (signal files, startup lock, ready wait) has already been extracted into
//! `daemon_ctl/startup.rs`; the remaining dense private type graph
//! (`ReplayReconnectStrategy`, `ReplaySendLifecycle`, etc.) still makes further
//! physical splitting non-trivial.
//!
//! | Domain | Lines | Key Public APIs |
//! |--------|-------|-----------------|
//! | **Bootstrap** | ~273–767 | `bootstrap_client`, `close_existing_session` |
//! | **Request transport + replay** | ~434–762 | `send_request_with_replay_recovery`, `send_existing_request_with_replay_recovery` |
//! | **Daemon launch** | ~764–897 | `start_daemon`, `acquire_startup_lock`, `startup_signal_paths` |
//! | **Ready wait** | ~898–1202 | `wait_for_ready`, `wait_for_ready_until`, `connect_ready_client` |
//! | **Session management** | ~1203–1401 | `close_all_sessions`, `fetch_launch_policy_for_session` |
//! | **Version upgrade + hardened connect** | ~1332–1725 | `maybe_upgrade_if_needed`, `detect_or_connect_hardened` |
//! | **Process lifecycle** | ~1726–end | `terminate_spawned_daemon`, `startup_signal_paths` |
//!
//! # Authority Notes
//!
//! - `bootstrap_client` is the single entry point for CLI→daemon connection. It owns
//!   the lock → start → wait-ready → handshake sequence (INV-007).
//! - `send_request_with_replay_recovery` is the single authority for IPC retry and
//!   replay recovery. It must never be bypassed for non-idempotent commands.
//! - Registry files are projections (INV-005). Live status probing MUST use socket or
//!   health check, not PID files.

#[cfg(test)]
use crate::connection_hardening::RetryAttribution;
use rub_ipc::client::IpcClient;

mod bootstrap;
mod close_all;
mod close_existing;
mod connect;
mod handshake;
mod ipc;
mod process_identity;
mod process_lifecycle;
mod projection;
mod registry;
mod replay;
mod startup;

#[cfg(test)]
pub(crate) use self::bootstrap::cleanup_precommit_browser_authority_for_test;
pub use self::bootstrap::{BootstrapClient, bootstrap_client};
#[cfg(test)]
pub(crate) use self::close_all::{
    CloseAllDisposition, classify_close_all_result, close_all_session_targets,
    should_escalate_close_all_to_kill_fallback,
};
pub(crate) use self::close_all::{close_all_sessions, close_all_sessions_until};
pub use self::close_existing::close_existing_session;
pub(crate) use self::close_existing::{
    close_existing_session_targeted, resolve_existing_close_target_by_attachment_identity,
};
pub(crate) use self::connect::{
    ShutdownFenceStatus, TransientSocketPolicy, authority_bound_deferred_client, connect_ipc_once,
    connect_ipc_with_retry, detect_or_connect_hardened_until, remaining_budget_duration,
    remaining_budget_ms, wait_for_shutdown_until,
};
#[cfg(test)]
pub(crate) use self::connect::{
    apply_hard_cut_shutdown_outcome, detect_or_connect_hardened, maybe_upgrade_if_needed,
    socket_candidates_for_session,
};
use self::handshake::handshake_attempt_error;
pub(crate) use self::handshake::{
    HandshakePayload, fetch_handshake_info, fetch_handshake_info_until,
    fetch_handshake_info_with_timeout,
};
use self::ipc::{
    ipc_budget_exhausted_error, ipc_transport_error, replay_recoverable_transport_reason,
};
pub(crate) use self::ipc::{ipc_timeout_error, project_request_onto_deadline};
#[cfg(test)]
pub(crate) use self::process_identity::command_matches_daemon_identity;
pub(crate) use self::process_lifecycle::terminate_spawned_daemon_force;
pub(crate) use self::projection::{
    DaemonCtlPathContext, daemon_ctl_path_error, daemon_ctl_path_state, daemon_ctl_socket_error,
    project_batch_close_result,
};
pub(crate) use self::registry::{
    cleanup_stale, latest_definitely_stale_entry_by_name, latest_registry_entry_by_name,
    registry_authority_snapshot, registry_entry_by_name, terminate_registry_entry_process,
};
#[cfg(test)]
pub(crate) use self::replay::replay_retry_matches_daemon_authority;
pub(crate) use self::replay::{
    ReplayRecoveryContext, send_existing_request_with_replay_recovery,
    send_request_with_replay_recovery,
};
#[cfg(all(test, unix))]
use self::startup::detach_daemon_session;
pub use self::startup::startup_signal_paths;
pub(crate) use self::startup::{
    StartupCleanupAuthorityKind, StartupCleanupProof, clear_startup_cleanup_proof,
    startup_cleanup_signal_path, write_startup_cleanup_proof_at,
};
#[cfg(test)]
use self::startup::{
    StartupSignalFiles, acquire_startup_lock, read_startup_cleanup_proof, read_startup_error,
    startup_lock_scope_keys, startup_ready_retry_timeout_failure, try_lock_exclusive, unlock,
    upgrade_startup_lock_to_canonical_attachment_until, wait_for_ready,
};

#[cfg(test)]
static FORCE_SETSID_FAILURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Result of attempting to connect to a daemon.
pub enum DaemonConnection {
    /// Connected to an existing daemon.
    Connected {
        client: IpcClient,
        daemon_session_id: Option<String>,
    },
    /// Need to start a new daemon.
    NeedStart,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchCloseResult {
    pub closed: Vec<String>,
    pub cleaned_stale: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
}

#[derive(Debug)]
pub enum ExistingCloseOutcome {
    Closed(Box<rub_ipc::protocol::IpcResponse>),
    Noop,
}

#[cfg(test)]
mod tests;
