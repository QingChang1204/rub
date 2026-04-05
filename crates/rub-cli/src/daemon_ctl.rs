//! Daemon control — auto-start, stale detection, socket connect, version upgrade.
//!
//! # Logical Structure
//!
//! This file contains six responsibility domains, all sharing a dense private
//! type graph (`ReplayReconnectStrategy`, `ReplaySendLifecycle`, etc.) that
//! makes physical file splitting non-trivial. Future refactoring should extract
//! these into a `daemon_ctl/` sub-module tree using `pub(super)` boundaries.
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

use crate::connection_hardening::{
    AttemptError, ConnectionFailureClass, RetryAttribution, RetryFailure, RetryPolicy,
    attach_connection_diagnostics, classify_error_code, classify_io_transient,
    classify_transport_message, run_with_bounded_retry,
};
use crate::timeout_budget::helpers::mutating_request;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use rub_core::process::is_process_alive;
use rub_ipc::client::IpcClient;
use rub_ipc::protocol::IpcRequest;
use uuid::Uuid;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::LaunchPolicyInfo;
use rub_daemon::rub_paths::RubPaths;

const READY_FILE_ENV: &str = "RUB_DAEMON_READY_FILE";
const ERROR_FILE_ENV: &str = "RUB_DAEMON_ERROR_FILE";
const SESSION_ID_ENV: &str = "RUB_SESSION_ID";

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

pub struct BootstrapClient {
    pub client: IpcClient,
    pub connected_to_existing_daemon: bool,
    pub daemon_session_id: Option<String>,
}

struct BootstrapResolution {
    client: IpcClient,
    connected_to_existing_daemon: bool,
    daemon_session_id: Option<String>,
}

impl BootstrapResolution {
    fn connected(client: IpcClient, daemon_session_id: Option<String>) -> Self {
        Self {
            client,
            connected_to_existing_daemon: true,
            daemon_session_id,
        }
    }

    fn started(client: IpcClient, daemon_session_id: String) -> Self {
        Self {
            client,
            connected_to_existing_daemon: false,
            daemon_session_id: Some(daemon_session_id),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct HandshakePayload {
    daemon_session_id: String,
    launch_policy: LaunchPolicyInfo,
}

pub struct StartupSignalFiles {
    pub ready_file: std::path::PathBuf,
    pub error_file: std::path::PathBuf,
    pub daemon_pid: u32,
    pub session_id: String,
}

struct StartupSignalCleanup<'a> {
    signals: &'a StartupSignalFiles,
}

impl Drop for StartupSignalCleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.signals.ready_file);
        let _ = std::fs::remove_file(&self.signals.error_file);
    }
}

struct StartupReadyMonitor {
    socket_path: std::path::PathBuf,
    committed_path: std::path::PathBuf,
}

enum StartupReadinessObservation {
    Pending,
    ReadyToHandshake,
    Error(RubError),
    DaemonExitedBeforeCommit {
        ready_written: bool,
        committed_session_id: Option<String>,
    },
}

#[cfg(unix)]
fn detach_daemon_session() -> std::io::Result<()> {
    #[cfg(test)]
    if FORCE_SETSID_FAILURE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other("forced setsid failure"));
    }

    if unsafe { libc::setsid() } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Debug)]
pub struct StartupLockGuard {
    files: Vec<std::fs::File>,
}

fn replay_recoverable_transport_reason(
    error: &(dyn std::error::Error + 'static),
) -> Option<&'static str> {
    error
        .downcast_ref::<std::io::Error>()
        .and_then(classify_io_transient)
        .or_else(|| classify_transport_message(&error.to_string()))
}

fn ipc_transport_error(
    error: impl std::fmt::Display,
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    ipc_classified_error(
        ErrorCode::IpcProtocolError,
        "IPC error",
        error,
        command_id,
        extra_context,
    )
}

fn ipc_timeout_error(
    error: impl std::fmt::Display,
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    ipc_classified_error(
        ErrorCode::IpcTimeout,
        "IPC timeout",
        error,
        command_id,
        extra_context,
    )
}

fn ipc_budget_exhausted_error(
    command_id: Option<&str>,
    original_timeout_ms: u64,
    phase: &str,
) -> RubError {
    ipc_classified_error(
        ErrorCode::IpcTimeout,
        "IPC timeout",
        format!("IPC request exhausted the declared timeout budget of {original_timeout_ms}ms"),
        command_id,
        Some(serde_json::json!({
            "reason": "ipc_replay_budget_exhausted",
            "phase": phase,
            "original_timeout_ms": original_timeout_ms,
        })),
    )
}

pub(crate) fn project_request_onto_deadline(
    request: &IpcRequest,
    deadline: Instant,
) -> Option<IpcRequest> {
    let remaining_timeout_ms = remaining_budget_ms(deadline);
    if remaining_timeout_ms == 0 {
        return None;
    }

    let mut projected = request.clone();
    projected.timeout_ms = projected.timeout_ms.min(remaining_timeout_ms);
    crate::timeout_budget::align_embedded_timeout_authority(&mut projected);
    Some(projected)
}

fn ipc_classified_error(
    code: ErrorCode,
    prefix: &str,
    error: impl std::fmt::Display,
    command_id: Option<&str>,
    extra_context: Option<serde_json::Value>,
) -> RubError {
    let mut context = serde_json::Map::new();
    if let Some(command_id) = command_id {
        context.insert("command_id".to_string(), serde_json::json!(command_id));
    }
    if let Some(extra) = extra_context
        && let Some(extra_object) = extra.as_object()
    {
        context.extend(extra_object.clone());
    }
    RubError::domain_with_context(
        code,
        format!("{prefix}: {error}"),
        serde_json::Value::Object(context),
    )
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchCloseResult {
    pub closed: Vec<String>,
    pub cleaned_stale: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
}

pub fn project_batch_close_result(rub_home: &Path, result: &BatchCloseResult) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "session_batch_close",
            "rub_home": rub_home.display().to_string(),
        },
        "result": {
            "closed": result.closed,
            "cleaned_stale": result.cleaned_stale,
            "failed": result.failed,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseAllDisposition {
    Closed,
    CleanedStale,
    Failed,
}

enum TransientSocketPolicy {
    NeedStartBeforeLock,
    FailAfterLock,
}

#[derive(Debug)]
pub enum ExistingCloseOutcome {
    Closed(Box<rub_ipc::protocol::IpcResponse>),
    Noop,
}

#[derive(Debug, Clone)]
struct CloseAllSessionTarget {
    session_name: String,
    authority_entry: Option<rub_daemon::session::RegistryEntry>,
    stale_entries: Vec<rub_daemon::session::RegistryEntry>,
    has_uncertain_entries: bool,
}

pub async fn bootstrap_client(
    rub_home: &Path,
    session_name: &str,
    command_deadline: Instant,
    extra_args: &[String],
    attachment_identity: Option<&str>,
) -> Result<BootstrapClient, RubError> {
    let resolution = match detect_or_connect_hardened(
        rub_home,
        session_name,
        TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => BootstrapResolution::connected(client, daemon_session_id),
        DaemonConnection::NeedStart => {
            resolve_bootstrap_after_lock(
                rub_home,
                session_name,
                command_deadline,
                extra_args,
                attachment_identity,
            )
            .await?
        }
    };

    Ok(BootstrapClient {
        client: resolution.client,
        connected_to_existing_daemon: resolution.connected_to_existing_daemon,
        daemon_session_id: resolution.daemon_session_id,
    })
}

async fn resolve_bootstrap_after_lock(
    rub_home: &Path,
    session_name: &str,
    command_deadline: Instant,
    extra_args: &[String],
    attachment_identity: Option<&str>,
) -> Result<BootstrapResolution, RubError> {
    let startup_session_id = rub_daemon::session::new_session_id();
    let startup_lock = acquire_startup_lock_until(
        rub_home,
        session_name,
        attachment_identity,
        command_deadline,
    )
    .await?;

    let resolution = match detect_or_connect_hardened(
        rub_home,
        session_name,
        TransientSocketPolicy::FailAfterLock,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => Ok(BootstrapResolution::connected(client, daemon_session_id)),
        DaemonConnection::NeedStart => {
            start_new_daemon_bootstrap(
                rub_home,
                session_name,
                &startup_session_id,
                extra_args,
                command_deadline,
            )
            .await
        }
    };

    drop(startup_lock);
    resolution
}

async fn start_new_daemon_bootstrap(
    rub_home: &Path,
    session_name: &str,
    startup_session_id: &str,
    extra_args: &[String],
    command_deadline: Instant,
) -> Result<BootstrapResolution, RubError> {
    let signals = start_daemon(rub_home, session_name, startup_session_id, extra_args)?;
    let ready = wait_for_ready_until(rub_home, session_name, &signals, command_deadline).await;
    if ready.is_err() {
        cleanup_failed_startup(rub_home, session_name, &signals).await;
    }
    ready.map(|(client, daemon_session_id)| BootstrapResolution::started(client, daemon_session_id))
}

pub async fn close_existing_session(
    rub_home: &Path,
    session_name: &str,
    timeout_ms: u64,
) -> Result<ExistingCloseOutcome, RubError> {
    if !rub_home.exists() {
        return Ok(ExistingCloseOutcome::Noop);
    }

    let (mut client, daemon_session_id) = match detect_or_connect_hardened(
        rub_home,
        session_name,
        TransientSocketPolicy::FailAfterLock,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => (client, daemon_session_id),
        DaemonConnection::NeedStart => return Ok(ExistingCloseOutcome::Noop),
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let request = mutating_request("close", serde_json::json!({}), timeout_ms.max(1));
    let response = send_existing_request_with_replay_recovery(
        &mut client,
        &request,
        deadline,
        rub_home,
        session_name,
        daemon_session_id.as_deref(),
    )
    .await
    .map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!("Failed to close existing session '{session_name}': {error}"),
            serde_json::json!({
                "session": session_name,
                "daemon_session_id": daemon_session_id,
                "command_id": request.command_id,
            }),
        )
    })?;
    Ok(ExistingCloseOutcome::Closed(Box::new(response)))
}

pub(crate) async fn send_existing_request_with_replay_recovery(
    client: &mut IpcClient,
    request: &IpcRequest,
    deadline: Instant,
    rub_home: &Path,
    session: &str,
    original_daemon_session_id: Option<&str>,
) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
    send_request_with_replay_strategy(
        client,
        request,
        deadline,
        original_daemon_session_id,
        ReplayReconnectStrategy::Existing { rub_home, session },
    )
    .await
}

pub async fn send_request_with_replay_recovery(
    client: &mut IpcClient,
    request: &IpcRequest,
    deadline: Instant,
    recovery: ReplayRecoveryContext<'_>,
) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
    send_request_with_replay_strategy(
        client,
        request,
        deadline,
        recovery.original_daemon_session_id,
        ReplayReconnectStrategy::Bootstrap(recovery),
    )
    .await
}

#[derive(Clone, Copy)]
pub struct ReplayRecoveryContext<'a> {
    pub rub_home: &'a Path,
    pub session: &'a str,
    pub daemon_args: &'a [String],
    pub attachment_identity: Option<&'a str>,
    pub original_daemon_session_id: Option<&'a str>,
}

struct ReplayAttempt<'a> {
    started: std::time::Instant,
    command_id: &'a str,
    retry_reason: &'static str,
    original_timeout_ms: u64,
    original_daemon_session_id: Option<&'a str>,
}

struct ReplayReconnectResult {
    client: IpcClient,
    daemon_session_id: Option<String>,
}

#[derive(Clone, Copy)]
enum ReplayReconnectStrategy<'a> {
    Existing {
        rub_home: &'a Path,
        session: &'a str,
    },
    Bootstrap(ReplayRecoveryContext<'a>),
}

#[derive(Clone, Copy)]
struct ReplaySendLifecycle<'a> {
    deadline: Instant,
    original_daemon_session_id: Option<&'a str>,
    strategy: ReplayReconnectStrategy<'a>,
}

impl ReplayAttempt<'_> {
    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

fn bind_request_to_daemon_authority(
    request: &IpcRequest,
    daemon_session_id: Option<&str>,
) -> IpcRequest {
    if request.daemon_session_id.is_none()
        && let Some(daemon_session_id) = daemon_session_id
    {
        return request
            .clone()
            .with_daemon_session_id(daemon_session_id.to_string())
            .expect("validated daemon session id must remain protocol-valid");
    }
    request.clone()
}

impl<'a> ReplaySendLifecycle<'a> {
    async fn send(
        self,
        client: &mut IpcClient,
        request: &IpcRequest,
    ) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
        let started = std::time::Instant::now();
        let request = bind_request_to_daemon_authority(request, self.original_daemon_session_id);
        let original_timeout_ms = request.timeout_ms;
        let request = self.project_initial_request(&request)?;
        match client.send(&request).await {
            Ok(response) => Ok(response),
            Err(error) => {
                self.retry_after_transport(&*error, &request, started, original_timeout_ms)
                    .await
            }
        }
    }

    fn project_initial_request(&self, request: &IpcRequest) -> Result<IpcRequest, RubError> {
        project_request_onto_deadline(request, self.deadline).ok_or_else(|| {
            ipc_budget_exhausted_error(
                request.command_id.as_deref(),
                request.timeout_ms,
                "initial_send",
            )
        })
    }

    async fn retry_after_transport(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        request: &IpcRequest,
        started: Instant,
        original_timeout_ms: u64,
    ) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
        let Some(command_id) = request.command_id.as_deref() else {
            return Err(ipc_transport_error(transport_error, None, None));
        };
        let Some(retry_reason) = replay_recoverable_transport_reason(transport_error) else {
            return Err(ipc_transport_error(transport_error, Some(command_id), None));
        };
        let attempt = ReplayAttempt {
            started,
            command_id,
            retry_reason,
            original_timeout_ms,
            original_daemon_session_id: self.original_daemon_session_id,
        };
        let (mut replay_client, replay_request) = self
            .reconnect_for_replay(transport_error, request, &attempt)
            .await?;
        let replay_timeout_ms = replay_request.timeout_ms;

        replay_client
            .send(&replay_request)
            .await
            .map_err(|replay_error| {
                ipc_transport_error(
                    replay_error,
                    Some(command_id),
                    Some(serde_json::json!({
                        "reason": "ipc_replay_retry_failed",
                        "retry_reason": retry_reason,
                        "daemon_session_id": self.original_daemon_session_id,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "remaining_timeout_ms": replay_timeout_ms,
                    })),
                )
            })
    }

    fn budget_exhausted_after_transport(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        attempt: &ReplayAttempt<'_>,
        phase: Option<&str>,
    ) -> RubError {
        ipc_timeout_error(
            transport_error,
            Some(attempt.command_id),
            Some(serde_json::json!({
                "reason": "ipc_replay_budget_exhausted",
                "retry_reason": attempt.retry_reason,
                "elapsed_ms": attempt.elapsed_ms(),
                "original_timeout_ms": attempt.original_timeout_ms,
                "phase": phase,
            })),
        )
    }

    fn identity_changed_error(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        attempt: &ReplayAttempt<'_>,
        reconnected_daemon_session_id: Option<&str>,
    ) -> RubError {
        ipc_transport_error(
            transport_error,
            Some(attempt.command_id),
            Some(serde_json::json!({
                "reason": "ipc_replay_daemon_identity_changed",
                "retry_reason": attempt.retry_reason,
                "original_daemon_session_id": attempt.original_daemon_session_id,
                "reconnected_daemon_session_id": reconnected_daemon_session_id,
            })),
        )
    }

    async fn reconnect_client(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        attempt: &ReplayAttempt<'_>,
    ) -> Result<ReplayReconnectResult, RubError> {
        if remaining_budget_ms(self.deadline) == 0 {
            return Err(self.budget_exhausted_after_transport(transport_error, attempt, None));
        }

        match self.strategy {
            ReplayReconnectStrategy::Existing { rub_home, session } => {
                match detect_or_connect_hardened(
                    rub_home,
                    session,
                    TransientSocketPolicy::FailAfterLock,
                )
                .await
                {
                    Ok(DaemonConnection::Connected {
                        client,
                        daemon_session_id,
                    }) => Ok(ReplayReconnectResult {
                        client,
                        daemon_session_id,
                    }),
                    Ok(DaemonConnection::NeedStart) => Err(ipc_transport_error(
                        transport_error,
                        Some(attempt.command_id),
                        Some(serde_json::json!({
                            "reason": "ipc_replay_existing_daemon_unavailable",
                            "retry_reason": attempt.retry_reason,
                            "original_daemon_session_id": attempt.original_daemon_session_id,
                            "elapsed_ms": attempt.elapsed_ms(),
                        })),
                    )),
                    Err(reconnect_error) => Err(ipc_transport_error(
                        transport_error,
                        Some(attempt.command_id),
                        Some(serde_json::json!({
                            "reason": "ipc_replay_reconnect_failed",
                            "retry_reason": attempt.retry_reason,
                            "original_daemon_session_id": attempt.original_daemon_session_id,
                            "elapsed_ms": attempt.elapsed_ms(),
                            "reconnect_error": reconnect_error.into_envelope(),
                        })),
                    )),
                }
            }
            ReplayReconnectStrategy::Bootstrap(recovery) => bootstrap_client(
                recovery.rub_home,
                recovery.session,
                self.deadline,
                recovery.daemon_args,
                recovery.attachment_identity,
            )
            .await
            .map(|bootstrap| ReplayReconnectResult {
                client: bootstrap.client,
                daemon_session_id: bootstrap.daemon_session_id,
            })
            .map_err(|reconnect_error| {
                ipc_transport_error(
                    transport_error,
                    Some(attempt.command_id),
                    Some(serde_json::json!({
                        "reason": "ipc_replay_reconnect_failed",
                        "retry_reason": attempt.retry_reason,
                        "original_daemon_session_id": attempt.original_daemon_session_id,
                        "elapsed_ms": attempt.elapsed_ms(),
                        "reconnect_error": reconnect_error.into_envelope(),
                    })),
                )
            }),
        }
    }

    fn project_retry_request(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        request: &IpcRequest,
        attempt: &ReplayAttempt<'_>,
    ) -> Result<IpcRequest, RubError> {
        let replay_request =
            project_request_onto_deadline(request, self.deadline).ok_or_else(|| {
                self.budget_exhausted_after_transport(transport_error, attempt, Some("replay_send"))
            })?;
        if replay_request.timeout_ms == 0 {
            return Err(self.budget_exhausted_after_transport(
                transport_error,
                attempt,
                Some("replay_send"),
            ));
        }
        Ok(replay_request)
    }

    async fn reconnect_for_replay(
        self,
        transport_error: &(dyn std::error::Error + 'static),
        request: &IpcRequest,
        attempt: &ReplayAttempt<'_>,
    ) -> Result<(IpcClient, IpcRequest), RubError> {
        let reconnect = self.reconnect_client(transport_error, attempt).await?;
        if !replay_retry_matches_daemon_authority(
            attempt.original_daemon_session_id,
            reconnect.daemon_session_id.as_deref(),
        ) {
            return Err(self.identity_changed_error(
                transport_error,
                attempt,
                reconnect.daemon_session_id.as_deref(),
            ));
        }

        let replay_request = self.project_retry_request(transport_error, request, attempt)?;
        Ok((reconnect.client, replay_request))
    }
}

async fn send_request_with_replay_strategy(
    client: &mut IpcClient,
    request: &IpcRequest,
    deadline: Instant,
    original_daemon_session_id: Option<&str>,
    strategy: ReplayReconnectStrategy<'_>,
) -> Result<rub_ipc::protocol::IpcResponse, RubError> {
    ReplaySendLifecycle {
        deadline,
        original_daemon_session_id,
        strategy,
    }
    .send(client, request)
    .await
}

fn replay_retry_matches_daemon_authority(
    original_daemon_session_id: Option<&str>,
    reconnected_daemon_session_id: Option<&str>,
) -> bool {
    match (original_daemon_session_id, reconnected_daemon_session_id) {
        (Some(original), Some(reconnected)) => original == reconnected,
        _ => false,
    }
}

/// Start a daemon process for the given session.
pub fn start_daemon(
    rub_home: &Path,
    session_name: &str,
    session_id: &str,
    extra_args: &[String],
) -> Result<StartupSignalFiles, RubError> {
    let exe = std::env::current_exe().map_err(|e| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Cannot find rub binary: {e}"),
        )
    })?;
    std::fs::create_dir_all(rub_home)?;

    let startup_id = Uuid::now_v7().to_string();
    let session_paths = RubPaths::new(rub_home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(session_paths.session_dir())?;
    let ready_file = session_paths.startup_ready_path(&startup_id);
    let error_file = session_paths.startup_error_path(&startup_id);
    let _ = std::fs::remove_file(&ready_file);
    let _ = std::fs::remove_file(&error_file);

    let mut cmd = Command::new(exe);
    cmd.arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(rub_home.to_string_lossy().as_ref())
        .env(READY_FILE_ENV, &ready_file)
        .env(ERROR_FILE_ENV, &error_file);
    cmd.env(SESSION_ID_ENV, session_id);

    for arg in extra_args {
        cmd.arg(arg);
    }

    // Detach from parent
    #[cfg(unix)]
    {
        use std::process::Stdio;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        // Use pre_exec for setsid on Unix
        unsafe {
            cmd.pre_exec(detach_daemon_session);
        }
    }

    let child = cmd.spawn().map_err(|e| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Failed to spawn daemon: {e}"),
        )
    })?;

    Ok(StartupSignalFiles {
        ready_file,
        error_file,
        daemon_pid: child.id(),
        session_id: session_id.to_string(),
    })
}

#[cfg(test)]
pub async fn acquire_startup_lock(
    rub_home: &Path,
    session_name: &str,
    attachment_identity: Option<&str>,
    timeout_ms: u64,
) -> Result<StartupLockGuard, RubError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    acquire_startup_lock_until(rub_home, session_name, attachment_identity, deadline).await
}

pub async fn acquire_startup_lock_until(
    rub_home: &Path,
    session_name: &str,
    attachment_identity: Option<&str>,
    deadline: Instant,
) -> Result<StartupLockGuard, RubError> {
    std::fs::create_dir_all(rub_home)?;
    let paths = RubPaths::new(rub_home);
    std::fs::create_dir_all(paths.startup_locks_dir())?;
    let mut files = Vec::new();
    for scope_key in startup_lock_scope_keys(session_name, attachment_identity) {
        let lock_path = paths.startup_lock_path(&scope_key);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                RubError::domain(
                    ErrorCode::DaemonStartFailed,
                    format!("Failed to open startup lock {}: {e}", lock_path.display()),
                )
            })?;

        loop {
            match try_lock_exclusive(&file) {
                Ok(()) => {
                    files.push(file);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(RubError::domain(
                            ErrorCode::DaemonStartFailed,
                            "Timed out waiting for daemon startup lock before the command deadline",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(err) => {
                    return Err(RubError::domain(
                        ErrorCode::DaemonStartFailed,
                        format!("Failed to acquire daemon startup lock: {err}"),
                    ));
                }
            }
        }
    }

    Ok(StartupLockGuard { files })
}

/// Wait for daemon startup to commit by observing the ready marker and then
/// confirming an explicit handshake against the session socket.
#[cfg(test)]
pub async fn wait_for_ready(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    timeout_ms: u64,
) -> Result<(IpcClient, String), RubError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    wait_for_ready_until(rub_home, session_name, signals, deadline).await
}

pub async fn wait_for_ready_until(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
    deadline: Instant,
) -> Result<(IpcClient, String), RubError> {
    let monitor = StartupReadyMonitor::new(rub_home, session_name, &signals.session_id);
    let _signal_cleanup = StartupSignalCleanup { signals };
    let timeout_ms = remaining_budget_ms(deadline).max(1);
    let mut last_transport_retry: Option<RetryAttribution> = None;

    loop {
        match monitor.observe(signals)? {
            StartupReadinessObservation::Error(error) => {
                let envelope = error.into_envelope();
                let classified = classify_error_code(envelope.code);
                let error = RubError::Domain(envelope);
                return Err(if let Some(attribution) = last_transport_retry.as_ref() {
                    attach_connection_diagnostics(error, attribution, classified)
                } else {
                    error
                });
            }
            StartupReadinessObservation::DaemonExitedBeforeCommit {
                ready_written,
                committed_session_id,
            } => {
                return Err(RubError::domain_with_context(
                    ErrorCode::DaemonStartFailed,
                    format!(
                        "Daemon for session '{session_name}' exited before startup authority committed"
                    ),
                    serde_json::json!({
                        "reason": "daemon_exited_before_startup_commit",
                        "session": session_name,
                        "session_id": signals.session_id,
                        "daemon_pid": signals.daemon_pid,
                        "ready_marker_present": ready_written,
                        "commit_marker_value": committed_session_id,
                    }),
                ));
            }
            StartupReadinessObservation::ReadyToHandshake => {
                match connect_ready_client(&monitor.socket_path, &signals.session_id, deadline)
                    .await
                {
                    Ok((client, daemon_session_id, _attribution)) => {
                        return Ok((client, daemon_session_id));
                    }
                    Err(failure)
                        if matches!(
                            failure.final_failure_class,
                            ConnectionFailureClass::TransportTransient
                        ) =>
                    {
                        last_transport_retry = Some(failure.attribution);
                    }
                    Err(failure) => {
                        return Err(failure.into_error());
                    }
                }
            }
            StartupReadinessObservation::Pending => {}
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let sleep_for = std::cmp::min(
            std::time::Duration::from_millis(100),
            deadline.saturating_duration_since(now),
        );
        tokio::time::sleep(sleep_for).await;
    }

    let error = RubError::domain(
        ErrorCode::DaemonStartFailed,
        format!("Daemon did not become ready within {}ms", timeout_ms),
    );
    Err(if let Some(attribution) = last_transport_retry.as_ref() {
        attach_connection_diagnostics(
            error,
            attribution,
            ConnectionFailureClass::TransportTransient,
        )
    } else {
        error
    })
}

impl StartupReadyMonitor {
    fn new(rub_home: &Path, session_name: &str, session_id: &str) -> Self {
        let session_runtime = RubPaths::new(rub_home).session_runtime(session_name, session_id);
        Self {
            socket_path: session_runtime.socket_path(),
            committed_path: session_runtime.startup_committed_path(),
        }
    }

    fn observe(
        &self,
        signals: &StartupSignalFiles,
    ) -> Result<StartupReadinessObservation, RubError> {
        if signals.error_file.exists() {
            let envelope = read_startup_error(&signals.error_file)?;
            return Ok(StartupReadinessObservation::Error(RubError::Domain(
                envelope,
            )));
        }

        let ready_written = signals.ready_file.exists();
        let committed_session_id = std::fs::read_to_string(&self.committed_path).ok();
        let committed = committed_session_id.as_deref() == Some(signals.session_id.as_str());
        if !committed && !is_process_alive(signals.daemon_pid) {
            return Ok(StartupReadinessObservation::DaemonExitedBeforeCommit {
                ready_written,
                committed_session_id,
            });
        }

        if ready_written && committed {
            Ok(StartupReadinessObservation::ReadyToHandshake)
        } else {
            Ok(StartupReadinessObservation::Pending)
        }
    }
}

async fn connect_ready_client(
    socket_path: &Path,
    expected_session_id: &str,
    deadline: Instant,
) -> Result<(IpcClient, String, RetryAttribution), RetryFailure> {
    let socket_path = socket_path.to_path_buf();
    let expected_session_id = expected_session_id.to_string();
    let policy = RetryPolicy::default();
    let mut attribution = RetryAttribution::default();

    loop {
        let Some(remaining) = remaining_budget_duration(deadline) else {
            return Err(startup_ready_retry_timeout_failure(attribution));
        };
        let attempt = tokio::time::timeout(remaining, async {
            let mut handshake_client = connect_ipc_once(
                &socket_path,
                ErrorCode::DaemonStartFailed,
                "Failed to connect to the daemon socket while confirming startup readiness",
            )
            .await?;

            let handshake = fetch_handshake_info_with_timeout(
                &mut handshake_client,
                remaining_budget_ms(deadline).max(1),
            )
            .await
            .map_err(handshake_attempt_error)?;
            if handshake.daemon_session_id != expected_session_id {
                return Err(AttemptError::terminal(
                    RubError::domain_with_context(
                        ErrorCode::DaemonStartFailed,
                        "Daemon startup handshake resolved a different daemon authority than the committed session",
                        serde_json::json!({
                            "reason": "startup_handshake_authority_mismatch",
                            "expected_session_id": expected_session_id,
                            "handshake_session_id": handshake.daemon_session_id,
                            "socket_path": socket_path.display().to_string(),
                        }),
                    ),
                    ConnectionFailureClass::ProtocolMismatch,
                ));
            }

            let client = authority_bound_deferred_client(&socket_path, &handshake.daemon_session_id)
                .map_err(|error| {
                    AttemptError::terminal(
                        RubError::domain(
                            ErrorCode::DaemonStartFailed,
                            format!(
                                "Failed to bind startup daemon authority after handshake: {error}"
                            ),
                        ),
                        ConnectionFailureClass::ProtocolMismatch,
                    )
                })?;

            Ok((client, handshake.daemon_session_id))
        })
        .await;

        match attempt {
            Ok(Ok((client, daemon_session_id))) => {
                return Ok((client, daemon_session_id, attribution));
            }
            Ok(Err(attempt_error)) => {
                if let Some(reason) = attempt_error.transient_reason.clone()
                    && attribution.retry_count < policy.max_retries
                    && remaining_budget_duration(deadline).is_some()
                {
                    attribution.retry_count += 1;
                    attribution.retry_reason = Some(reason);
                    if let Some(delay) = remaining_budget_duration(deadline) {
                        tokio::time::sleep(delay.min(policy.delay)).await;
                        continue;
                    }
                }

                return Err(RetryFailure {
                    error: attempt_error.error,
                    attribution,
                    final_failure_class: if attempt_error.transient_reason.is_some() {
                        ConnectionFailureClass::TransportTransient
                    } else {
                        attempt_error.final_failure_class
                    },
                });
            }
            Err(_) => return Err(startup_ready_retry_timeout_failure(attribution)),
        }
    }
}

pub async fn fetch_launch_policy(client: &mut IpcClient) -> Result<LaunchPolicyInfo, RubError> {
    Ok(fetch_handshake_info(client).await?.launch_policy)
}

async fn fetch_handshake_info(client: &mut IpcClient) -> Result<HandshakePayload, RubError> {
    fetch_handshake_info_with_timeout(client, 3_000).await
}

async fn fetch_handshake_info_with_timeout(
    client: &mut IpcClient,
    timeout_ms: u64,
) -> Result<HandshakePayload, RubError> {
    let request = IpcRequest::new("_handshake", serde_json::json!({}), timeout_ms.max(1));
    let response = client
        .send(&request)
        .await
        .map_err(|e| RubError::domain(ErrorCode::IpcProtocolError, e.to_string()))?;

    if response.status == rub_ipc::protocol::ResponseStatus::Error {
        let envelope = response.error.unwrap_or_else(|| {
            rub_core::error::ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                "Handshake returned an empty error envelope",
            )
        });
        return Err(RubError::Domain(envelope));
    }

    let data = response.data.unwrap_or_default();
    serde_json::from_value(data).map_err(|e| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            format!("Invalid handshake payload: {e}"),
        )
    })
}

fn startup_ready_retry_timeout_failure(attribution: RetryAttribution) -> RetryFailure {
    RetryFailure {
        error: RubError::domain_with_context(
            ErrorCode::DaemonStartFailed,
            "Daemon readiness handshake exceeded the declared startup timeout",
            serde_json::json!({
                "reason": "startup_handshake_timeout",
            }),
        ),
        attribution,
        final_failure_class: ConnectionFailureClass::TransportTransient,
    }
}

pub async fn fetch_launch_policy_for_session(
    rub_home: &Path,
    session: &str,
) -> Result<LaunchPolicyInfo, RubError> {
    let socket_path = preferred_socket_path_for_session(rub_home, session)?;
    let (launch_policy, _attribution) = run_with_bounded_retry(RetryPolicy::default(), || async {
        let (mut client, _connect_attr) = connect_ipc_with_retry(
            &socket_path,
            ErrorCode::IpcProtocolError,
            format!("Failed to connect to session '{session}' for launch policy check"),
        )
        .await
        .map_err(RetryFailure::into_attempt_error)?;
        fetch_launch_policy(&mut client)
            .await
            .map_err(handshake_attempt_error)
    })
    .await
    .map_err(RetryFailure::into_error)?;
    Ok(launch_policy)
}

pub async fn close_all_sessions(
    rub_home: &Path,
    timeout: u64,
) -> Result<BatchCloseResult, RubError> {
    let command_deadline = Instant::now() + Duration::from_millis(timeout.max(1));
    if !rub_home.exists() {
        return Ok(BatchCloseResult {
            closed: Vec::new(),
            cleaned_stale: Vec::new(),
            failed: Vec::new(),
        });
    }

    let snapshot = registry_authority_snapshot(rub_home)?;
    if snapshot.sessions.is_empty() {
        return Ok(BatchCloseResult {
            closed: Vec::new(),
            cleaned_stale: Vec::new(),
            failed: Vec::new(),
        });
    }

    let mut closed = Vec::new();
    let mut cleaned_stale = Vec::new();
    let mut failed = Vec::new();

    for target in close_all_session_targets(&snapshot) {
        if remaining_budget_ms(command_deadline) == 0 {
            failed.push(target.session_name);
            continue;
        }
        let mut session_cleaned_stale = false;
        for entry in &target.stale_entries {
            cleanup_stale(rub_home, entry);
            let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
            session_cleaned_stale = true;
        }

        let Some(entry) = target.authority_entry.as_ref() else {
            if target.has_uncertain_entries {
                failed.push(target.session_name);
            } else if session_cleaned_stale {
                cleaned_stale.push(target.session_name);
            }
            continue;
        };

        let session_paths =
            RubPaths::new(rub_home).session_runtime(&entry.session_name, &entry.session_id);
        let mut graceful_close = false;

        if let Some(socket_path) = session_paths.existing_socket_paths().into_iter().next()
            && let Ok(mut client) = IpcClient::connect(&socket_path).await
        {
            let request = mutating_request(
                "close",
                serde_json::json!({}),
                remaining_budget_ms(command_deadline).max(1),
            );
            graceful_close = matches!(
                send_existing_request_with_replay_recovery(
                    &mut client,
                    &request,
                    command_deadline,
                    rub_home,
                    &target.session_name,
                    Some(entry.session_id.as_str()),
                )
                .await,
                Ok(response) if response.status == rub_ipc::protocol::ResponseStatus::Success
            );
        }

        let termination_requested = terminate_registry_entry_process(rub_home, entry).is_ok();
        let shutdown = wait_for_shutdown_until(rub_home, entry, command_deadline).await;

        let still_running =
            is_process_alive(entry.pid) || !session_paths.existing_socket_paths().is_empty();
        match classify_close_all_result(
            graceful_close,
            termination_requested,
            shutdown,
            still_running,
        ) {
            CloseAllDisposition::Closed => {
                cleanup_stale(rub_home, entry);
                let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
                closed.push(target.session_name);
            }
            CloseAllDisposition::CleanedStale => {
                cleanup_stale(rub_home, entry);
                let _ = rub_daemon::session::deregister_session(rub_home, &entry.session_id);
                cleaned_stale.push(target.session_name);
            }
            CloseAllDisposition::Failed => {
                failed.push(target.session_name);
            }
        }
    }

    Ok(BatchCloseResult {
        closed,
        cleaned_stale,
        failed,
    })
}

fn close_all_session_targets(
    snapshot: &rub_daemon::session::RegistryAuthoritySnapshot,
) -> Vec<CloseAllSessionTarget> {
    snapshot
        .sessions
        .iter()
        .map(|session| CloseAllSessionTarget {
            session_name: session.session_name.clone(),
            authority_entry: session
                .authoritative_entry()
                .map(|entry| entry.entry.clone()),
            stale_entries: session.stale_entries(),
            has_uncertain_entries: session.has_uncertain_entries(),
        })
        .collect()
}

/// Clean up stale projection files.
fn cleanup_stale(rub_home: &Path, entry: &rub_daemon::session::RegistryEntry) {
    rub_daemon::session::cleanup_projections(rub_home, entry);
}

async fn maybe_upgrade_if_needed(
    _rub_home: &Path,
    session_name: &str,
    authority_entry: Option<&rub_daemon::session::RegistryEntry>,
    handshake: &HandshakePayload,
    socket_path: &Path,
) -> Result<DaemonConnection, RubError> {
    if let Some(entry) = authority_entry
        && handshake.daemon_session_id != entry.session_id
    {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Connected daemon authority for session '{session_name}' did not match the authoritative registry entry"
            ),
            serde_json::json!({
                "reason": "daemon_authority_mismatch",
                "session": session_name,
                "registry_session_id": entry.session_id,
                "handshake_session_id": handshake.daemon_session_id,
                "socket_path": entry.socket_path,
            }),
        ));
    }

    Ok(DaemonConnection::Connected {
        client: authority_bound_deferred_client(socket_path, &handshake.daemon_session_id)
            .map_err(|error| {
                RubError::domain(
                    ErrorCode::IpcProtocolError,
                    format!("Failed to bind connected daemon authority: {error}"),
                )
            })?,
        daemon_session_id: Some(handshake.daemon_session_id.clone()),
    })
}

async fn hard_cut_outdated_daemon(
    rub_home: &Path,
    session_name: &str,
    entry: &rub_daemon::session::RegistryEntry,
) -> Result<(), RubError> {
    let _ = terminate_registry_entry_process(rub_home, entry);
    let shutdown = wait_for_shutdown(rub_home, entry).await;
    if shutdown.lifecycle_committed() {
        cleanup_stale(rub_home, entry);
    }
    if !shutdown.fully_released() {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcVersionMismatch,
            format!(
                "Session '{session_name}' is still owned by an outdated daemon or browser profile after hard-cut upgrade fencing"
            ),
            serde_json::json!({
                "session": session_name,
                "daemon_protocol_version": entry.ipc_protocol_version,
                "cli_protocol_version": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
                "reason": "hard_cut_upgrade_fence_incomplete",
                "user_data_dir": entry.user_data_dir,
            }),
        ));
    }
    Ok(())
}

async fn detect_or_connect_hardened(
    rub_home: &Path,
    session_name: &str,
    transient_socket_policy: TransientSocketPolicy,
) -> Result<DaemonConnection, RubError> {
    let authority_entry = registry_entry_by_name(rub_home, session_name)?;
    if let Some(entry) = authority_entry.as_ref()
        && entry.ipc_protocol_version != rub_ipc::protocol::IPC_PROTOCOL_VERSION
    {
        hard_cut_outdated_daemon(rub_home, session_name, entry).await?;
        return Ok(DaemonConnection::NeedStart);
    }

    let socket_paths = socket_candidates_for_session(authority_entry.as_ref())?;

    if socket_paths.is_empty() {
        return Ok(DaemonConnection::NeedStart);
    }

    let mut last_failure = None;
    for socket_path in socket_paths {
        match connect_ipc_with_retry(
            &socket_path,
            ErrorCode::IpcProtocolError,
            "Failed to connect to an existing daemon socket",
        )
        .await
        {
            Ok((mut handshake_client, _attribution)) => {
                let handshake = fetch_handshake_info(&mut handshake_client).await?;
                return maybe_upgrade_if_needed(
                    rub_home,
                    session_name,
                    authority_entry.as_ref(),
                    &handshake,
                    &socket_path,
                )
                .await;
            }
            Err(failure) => {
                last_failure = Some(failure);
            }
        }
    }

    let pid_paths = authority_entry
        .as_ref()
        .map(|entry| {
            RubPaths::new(rub_home)
                .session_runtime(&entry.session_name, &entry.session_id)
                .existing_pid_paths()
        })
        .unwrap_or_else(|| {
            RubPaths::new(rub_home)
                .session(session_name)
                .existing_pid_paths()
        });
    if let Some(dead_pid) = pid_paths.into_iter().find_map(|pid_path| {
        std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|pid_str| pid_str.trim().parse::<u32>().ok())
            .filter(|pid| !is_process_alive(*pid))
    }) {
        tracing::warn!(
            session = session_name,
            pid = dead_pid,
            "Detected stale daemon after connect retry failure, cleaning up"
        );
        if let Some(entry) = latest_registry_entry_by_name(rub_home, session_name)? {
            cleanup_stale(rub_home, &entry);
        }
        return Ok(DaemonConnection::NeedStart);
    }

    let Some(failure) = last_failure else {
        return Ok(DaemonConnection::NeedStart);
    };
    if matches!(
        transient_socket_policy,
        TransientSocketPolicy::NeedStartBeforeLock
    ) && matches!(
        failure.final_failure_class,
        ConnectionFailureClass::TransportTransient
    ) {
        return Ok(DaemonConnection::NeedStart);
    }

    Err(failure.into_error())
}

fn authority_bound_deferred_client(
    socket_path: &Path,
    daemon_session_id: &str,
) -> Result<IpcClient, String> {
    IpcClient::deferred(socket_path.to_path_buf()).bind_daemon_session_id(daemon_session_id)
}

async fn connect_ipc_with_retry(
    socket_path: &Path,
    error_code: ErrorCode,
    message_prefix: impl AsRef<str>,
) -> Result<(IpcClient, RetryAttribution), RetryFailure> {
    let socket_path = socket_path.to_path_buf();
    let message_prefix = message_prefix.as_ref().to_string();
    run_with_bounded_retry(RetryPolicy::default(), move || {
        let socket_path = socket_path.clone();
        let message_prefix = message_prefix.clone();
        async move {
            IpcClient::connect(&socket_path).await.map_err(|error| {
                let message = format!("{} {}: {error}", message_prefix, socket_path.display());
                if let Some(reason) = classify_io_transient(&error) {
                    AttemptError::retryable(RubError::domain(error_code, message), reason)
                } else {
                    AttemptError::terminal(
                        RubError::domain(error_code, message),
                        classify_error_code(error_code),
                    )
                }
            })
        }
    })
    .await
}

async fn connect_ipc_once(
    socket_path: &Path,
    error_code: ErrorCode,
    message_prefix: impl AsRef<str>,
) -> Result<IpcClient, AttemptError> {
    IpcClient::connect(socket_path).await.map_err(|error| {
        let message = format!(
            "{} {}: {error}",
            message_prefix.as_ref(),
            socket_path.display()
        );
        if let Some(reason) = classify_io_transient(&error) {
            AttemptError::retryable(RubError::domain(error_code, message), reason)
        } else {
            AttemptError::terminal(
                RubError::domain(error_code, message),
                classify_error_code(error_code),
            )
        }
    })
}

fn handshake_attempt_error(error: RubError) -> AttemptError {
    let envelope = error.into_envelope();
    if let Some(reason) = classify_transport_message(&envelope.message) {
        AttemptError::retryable(RubError::Domain(envelope), reason)
    } else {
        AttemptError::terminal(
            RubError::Domain(envelope.clone()),
            classify_error_code(envelope.code),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShutdownFenceStatus {
    daemon_stopped: bool,
    profile_released: bool,
}

impl ShutdownFenceStatus {
    fn lifecycle_committed(self) -> bool {
        self.daemon_stopped
    }

    fn fully_released(self) -> bool {
        self.daemon_stopped && self.profile_released
    }
}

fn classify_close_all_result(
    graceful_close: bool,
    termination_requested: bool,
    shutdown: ShutdownFenceStatus,
    still_running: bool,
) -> CloseAllDisposition {
    if shutdown.fully_released() {
        if graceful_close {
            CloseAllDisposition::Closed
        } else {
            CloseAllDisposition::CleanedStale
        }
    } else if graceful_close
        || termination_requested
        || still_running
        || shutdown.lifecycle_committed()
    {
        CloseAllDisposition::Failed
    } else {
        CloseAllDisposition::CleanedStale
    }
}

async fn wait_for_shutdown_until(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
    deadline: Instant,
) -> ShutdownFenceStatus {
    let session_paths =
        RubPaths::new(rub_home).session_runtime(&entry.session_name, &entry.session_id);
    while Instant::now() < deadline {
        let profile_released = profile_released(entry);
        let daemon_stopped =
            session_paths.existing_socket_paths().is_empty() && !is_process_alive(entry.pid);
        if daemon_stopped {
            return ShutdownFenceStatus {
                daemon_stopped: true,
                profile_released,
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    ShutdownFenceStatus {
        daemon_stopped: false,
        profile_released: profile_released(entry),
    }
}

async fn wait_for_shutdown(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> ShutdownFenceStatus {
    wait_for_shutdown_until(rub_home, entry, Instant::now() + Duration::from_secs(5)).await
}

fn profile_released(entry: &rub_daemon::session::RegistryEntry) -> bool {
    entry.user_data_dir.as_deref().is_none_or(
        |user_data_dir| match rub_cdp::managed_profile_in_use(Path::new(user_data_dir)) {
            Ok(in_use) => !in_use,
            Err(_) => false,
        },
    )
}

fn socket_candidates_for_session(
    authority_entry: Option<&rub_daemon::session::RegistryEntry>,
) -> Result<Vec<std::path::PathBuf>, RubError> {
    let mut candidates = Vec::new();
    if let Some(entry) = authority_entry {
        let path = std::path::PathBuf::from(&entry.socket_path);
        if path.exists() {
            candidates.push(path);
        }
    }
    Ok(candidates)
}

pub(crate) fn remaining_budget_ms(deadline: Instant) -> u64 {
    deadline
        .checked_duration_since(Instant::now())
        .map(|remaining| remaining.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn remaining_budget_duration(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn preferred_socket_path_for_session(
    rub_home: &Path,
    session_name: &str,
) -> Result<std::path::PathBuf, RubError> {
    let authority_entry = registry_entry_by_name(rub_home, session_name)?;
    if let Some(path) = socket_candidates_for_session(authority_entry.as_ref())?
        .into_iter()
        .next()
    {
        return Ok(path);
    }

    Ok(authority_entry
        .as_ref()
        .map(|entry| {
            RubPaths::new(rub_home)
                .session_runtime(&entry.session_name, &entry.session_id)
                .socket_path()
        })
        .unwrap_or_else(|| RubPaths::new(rub_home).session(session_name).socket_path()))
}

fn registry_entry_by_name(
    rub_home: &Path,
    session_name: &str,
) -> Result<Option<rub_daemon::session::RegistryEntry>, RubError> {
    Ok(registry_authority_snapshot(rub_home)?
        .session(session_name)
        .and_then(|session| {
            session
                .authoritative_entry()
                .map(|entry| entry.entry.clone())
        }))
}

fn latest_registry_entry_by_name(
    rub_home: &Path,
    session_name: &str,
) -> Result<Option<rub_daemon::session::RegistryEntry>, RubError> {
    Ok(registry_authority_snapshot(rub_home)?
        .session(session_name)
        .and_then(|session| session.latest_entry().map(|entry| entry.entry.clone())))
}

fn terminate_registry_entry_process(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<()> {
    if !process_matches_registry_entry(rub_home, entry)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Refused to kill pid {} because it no longer matches daemon authority for session '{}' under {}",
                entry.pid,
                entry.session_name,
                rub_home.display()
            ),
        ));
    }
    let result = unsafe { libc::kill(entry.pid as i32, libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn terminate_spawned_daemon(pid: u32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub async fn terminate_spawned_daemon_force(pid: u32) -> std::io::Result<()> {
    let _ = terminate_spawned_daemon(pid);
    for _ in 0..20 {
        if !is_process_alive(pid) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if result == 0 || !is_process_alive(pid) {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

async fn cleanup_failed_startup(rub_home: &Path, session_name: &str, signals: &StartupSignalFiles) {
    let _ = terminate_failed_startup_process(rub_home, session_name, signals).await;

    let runtime_paths = RubPaths::new(rub_home).session_runtime(session_name, &signals.session_id);
    for _ in 0..20 {
        if !is_process_alive(signals.daemon_pid)
            && runtime_paths
                .actual_socket_paths()
                .into_iter()
                .all(|path| !path.exists())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if !is_process_alive(signals.daemon_pid) {
        let cleanup_entry = rub_daemon::session::RegistryEntry {
            session_id: signals.session_id.clone(),
            session_name: session_name.to_string(),
            pid: signals.daemon_pid,
            socket_path: runtime_paths.socket_path().display().to_string(),
            created_at: String::new(),
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let _ = rub_daemon::session::deregister_session(rub_home, &signals.session_id);
        cleanup_stale(rub_home, &cleanup_entry);
    }

    let _ = std::fs::remove_file(&signals.ready_file);
    let _ = std::fs::remove_file(&signals.error_file);
}

async fn terminate_failed_startup_process(
    rub_home: &Path,
    session_name: &str,
    signals: &StartupSignalFiles,
) -> std::io::Result<()> {
    if !process_matches_daemon_identity(
        rub_home,
        session_name,
        Some(signals.session_id.as_str()),
        signals.daemon_pid,
    )? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Refused to kill pid {} because it no longer matches failed-startup daemon authority for session '{}' under {}",
                signals.daemon_pid,
                session_name,
                rub_home.display()
            ),
        ));
    }
    terminate_spawned_daemon_force(signals.daemon_pid).await
}

fn process_matches_registry_entry(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> std::io::Result<bool> {
    process_matches_daemon_identity(
        rub_home,
        &entry.session_name,
        Some(entry.session_id.as_str()),
        entry.pid,
    )
}

fn process_matches_daemon_identity(
    rub_home: &Path,
    session_name: &str,
    session_id: Option<&str>,
    pid: u32,
) -> std::io::Result<bool> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if command.is_empty() {
        return Ok(false);
    }
    Ok(command_matches_daemon_identity(
        &command,
        rub_home,
        session_name,
        session_id,
    ))
}

fn command_matches_daemon_identity(
    command: &str,
    rub_home: &Path,
    session_name: &str,
    session_id: Option<&str>,
) -> bool {
    if !command.contains("__daemon")
        || extract_flag_value(command, "--session").as_deref() != Some(session_name)
        || extract_flag_value(command, "--rub-home").as_deref()
            != Some(rub_home.to_string_lossy().as_ref())
    {
        return false;
    }
    match session_id {
        Some(session_id) => {
            extract_flag_value(command, "--session-id").as_deref() == Some(session_id)
        }
        None => true,
    }
}

fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
    let inline_prefix = format!("{flag}=");
    let mut parts = tokenize_command(command).into_iter();
    while let Some(part) = parts.next() {
        if part == flag {
            return parts.next();
        }
        if let Some(value) = part.strip_prefix(&inline_prefix) {
            return Some(value.to_string());
        }
    }
    None
}

fn tokenize_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single_quotes = false;
    let mut in_double_quotes = false;
    let mut escaping = false;

    for ch in command.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }

        match ch {
            '\\' if !in_single_quotes => escaping = true,
            '\'' if !in_double_quotes => in_single_quotes = !in_single_quotes,
            '"' if !in_single_quotes => in_double_quotes = !in_double_quotes,
            ch if ch.is_whitespace() && !in_single_quotes && !in_double_quotes => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaping {
        current.push('\\');
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn registry_authority_snapshot(
    rub_home: &Path,
) -> Result<rub_daemon::session::RegistryAuthoritySnapshot, RubError> {
    rub_daemon::session::registry_authority_snapshot(rub_home).map_err(|e| {
        RubError::domain(
            ErrorCode::DaemonStartFailed,
            format!("Failed to resolve registry authority: {e}"),
        )
    })
}

fn read_startup_error(path: &Path) -> Result<rub_core::error::ErrorEnvelope, RubError> {
    let contents = std::fs::read_to_string(path)?;
    let envelope = serde_json::from_str(&contents).unwrap_or_else(|_| {
        rub_core::error::ErrorEnvelope::new(ErrorCode::DaemonStartFailed, contents)
    });
    Ok(envelope)
}

pub fn startup_signal_paths() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    (
        std::env::var_os(READY_FILE_ENV).map(std::path::PathBuf::from),
        std::env::var_os(ERROR_FILE_ENV).map(std::path::PathBuf::from),
    )
}

impl Drop for StartupLockGuard {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = unlock(file);
        }
    }
}

fn startup_lock_scope_keys(session_name: &str, attachment_identity: Option<&str>) -> Vec<String> {
    let mut keys = vec![format!("session-{session_name}")];
    if let Some(identity) = attachment_identity {
        keys.push(format!("attachment-{identity}"));
    }
    keys
}

fn try_lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::{FORCE_SETSID_FAILURE, detach_daemon_session};
    use super::{
        ShutdownFenceStatus, StartupSignalFiles, acquire_startup_lock, classify_close_all_result,
        close_all_session_targets, close_all_sessions, command_matches_daemon_identity,
        ipc_timeout_error, project_request_onto_deadline, read_startup_error,
        registry_authority_snapshot, replay_retry_matches_daemon_authority,
        socket_candidates_for_session, startup_lock_scope_keys, startup_signal_paths,
        try_lock_exclusive, unlock, wait_for_ready,
    };
    use crate::timeout_budget::WAIT_IPC_BUFFER_MS;
    use rub_core::error::ErrorCode;
    use rub_core::model::LaunchPolicyInfo;
    use rub_daemon::rub_paths::RubPaths;
    use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
    use rub_ipc::codec::NdJsonCodec;
    use rub_ipc::protocol::{IpcRequest, IpcResponse, ResponseStatus};
    use std::path::Path;
    use std::time::{Duration, Instant};
    use tokio::io::BufReader;
    use tokio::net::UnixListener;
    use uuid::Uuid;

    #[cfg(unix)]
    use std::sync::atomic::Ordering;
    #[cfg(unix)]
    use std::{
        io::{BufRead as _, BufReader as StdBufReader, Write as _},
        os::unix::net::UnixListener as StdUnixListener,
    };

    fn temp_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rub-daemon-ctl-test-{}", Uuid::now_v7()))
    }

    #[test]
    fn project_request_onto_deadline_shrinks_request_and_embedded_wait_budget_together() {
        let request = IpcRequest::new(
            "wait",
            serde_json::json!({
                "selector": "#ready",
                "timeout_ms": 30_000,
            }),
            30_000 + WAIT_IPC_BUFFER_MS,
        );
        let deadline = Instant::now() + Duration::from_millis(2_000 + WAIT_IPC_BUFFER_MS);

        let projected =
            project_request_onto_deadline(&request, deadline).expect("deadline should remain");
        let embedded_timeout_ms = projected
            .args
            .get("timeout_ms")
            .and_then(|value| value.as_u64())
            .expect("wait payload should keep embedded timeout");

        assert!(projected.timeout_ms <= 2_000 + WAIT_IPC_BUFFER_MS);
        assert_eq!(
            embedded_timeout_ms,
            projected.timeout_ms.saturating_sub(WAIT_IPC_BUFFER_MS)
        );
    }

    #[test]
    fn project_request_onto_deadline_returns_none_when_deadline_is_exhausted() {
        let request = IpcRequest::new("doctor", serde_json::json!({}), 5_000);
        let deadline = Instant::now() - Duration::from_millis(1);

        assert!(project_request_onto_deadline(&request, deadline).is_none());
    }

    #[tokio::test]
    async fn close_all_reports_stale_cleanup_in_cleaned_stale_list() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let default_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
        let work_paths = RubPaths::new(&home).session_runtime("work", "sess-work");
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![
                    RegistryEntry {
                        session_id: "sess-default".to_string(),
                        session_name: "default".to_string(),
                        pid: 424242,
                        socket_path: default_paths.socket_path().display().to_string(),
                        created_at: "2026-03-28T00:00:00Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "sess-work".to_string(),
                        session_name: "work".to_string(),
                        pid: 434343,
                        socket_path: work_paths.socket_path().display().to_string(),
                        created_at: "2026-03-28T00:00:01Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: None,
                        connection_target: None,
                    },
                ],
            },
        )
        .unwrap();

        let result = close_all_sessions(&home, 1_000).await.unwrap();
        assert!(result.closed.is_empty());
        assert_eq!(result.cleaned_stale.len(), 2);

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn close_all_preserves_registry_when_shutdown_fence_is_not_confirmed() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit marker parent"),
        )
        .unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-default").unwrap();
        let stale_socket = runtime.socket_path();
        std::fs::write(&stale_socket, b"").unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-default".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: stale_socket.display().to_string(),
                    created_at: "2026-04-01T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let result = close_all_sessions(&home, 100).await.unwrap();
        assert!(result.closed.is_empty());
        assert!(result.cleaned_stale.is_empty());
        assert_eq!(result.failed, vec!["default".to_string()]);

        let registry = rub_daemon::session::read_registry(&home).unwrap();
        assert_eq!(registry.sessions.len(), 1);
        assert_eq!(registry.sessions[0].session_id, "sess-default");

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn close_existing_session_noops_without_creating_rub_home() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);

        let outcome = super::close_existing_session(&home, "default", 1_000)
            .await
            .unwrap();
        assert!(matches!(outcome, super::ExistingCloseOutcome::Noop));
        assert!(
            !home.exists(),
            "close must not bootstrap or create RUB_HOME"
        );
    }

    #[tokio::test]
    async fn close_existing_session_replays_with_stable_command_id() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit marker parent"),
        )
        .unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-default").unwrap();

        let socket_path = runtime.socket_path();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-default".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: socket_path.display().to_string(),
                    created_at: "2026-04-03T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let server = tokio::spawn(async move {
            let mut first_close_command_id = None;
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let request: IpcRequest = NdJsonCodec::read(&mut reader)
                    .await
                    .expect("read request")
                    .expect("request");
                match request.command.as_str() {
                    "_handshake" => {
                        let response = IpcResponse::success(
                            "handshake",
                            serde_json::json!({
                                "daemon_session_id": "sess-default",
                                "launch_policy": {
                                    "headless": true,
                                    "ignore_cert_errors": false,
                                    "hide_infobars": false
                                }
                            }),
                        );
                        let _ = NdJsonCodec::write(&mut writer, &response).await;
                    }
                    "close" if first_close_command_id.is_none() => {
                        assert!(
                            request.command_id.is_some(),
                            "close replay fence must carry command_id"
                        );
                        first_close_command_id = request.command_id.clone();
                        // Drop the connection without replying so replay recovery
                        // must reconnect and retry against the same authority.
                    }
                    "close" => {
                        assert_eq!(request.command_id, first_close_command_id);
                        let response = IpcResponse::success(
                            "close-replayed",
                            serde_json::json!({
                                "closed": true
                            }),
                        )
                        .with_command_id(
                            request
                                .command_id
                                .clone()
                                .expect("replayed close command_id"),
                        )
                        .unwrap();
                        NdJsonCodec::write(&mut writer, &response)
                            .await
                            .expect("write close response");
                        break;
                    }
                    other => panic!("unexpected request command during close replay test: {other}"),
                }
            }
        });

        let outcome = super::close_existing_session(&home, "default", 2_000)
            .await
            .expect("close existing session");
        let super::ExistingCloseOutcome::Closed(response) = outcome else {
            panic!("existing session close must issue a close request");
        };
        assert_eq!(response.status, ResponseStatus::Success);

        server.await.expect("server task");
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn close_all_sessions_noops_without_creating_rub_home() {
        let home = temp_home();
        let _ = std::fs::remove_dir_all(&home);

        let result = close_all_sessions(&home, 1_000).await.unwrap();
        assert!(result.closed.is_empty());
        assert!(result.cleaned_stale.is_empty());
        assert!(result.failed.is_empty());
        assert!(!home.exists(), "close --all must not create RUB_HOME");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn close_all_targets_ignore_pending_replacement_for_same_session() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let live_runtime = RubPaths::new(&home).session_runtime("default", "sess-live");
        let pending_runtime = RubPaths::new(&home).session_runtime("default", "sess-pending");
        let projection = RubPaths::new(&home).session("default");
        std::fs::create_dir_all(live_runtime.session_dir()).unwrap();
        std::fs::create_dir_all(pending_runtime.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(live_runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(pending_runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(projection.startup_committed_path(), "sess-live").unwrap();
        std::fs::write(pending_runtime.socket_path(), b"pending").unwrap();

        let listener = StdUnixListener::bind(live_runtime.socket_path()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            StdBufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let decoded: rub_ipc::protocol::IpcRequest =
                serde_json::from_str(request.trim_end()).unwrap();
            assert_eq!(decoded.command, "_handshake");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": "sess-live",
                }),
            );
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        let registry = RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: "sess-live".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: live_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-03T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "sess-pending".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: pending_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-03T00:00:01Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        };
        write_registry(&home, &registry).unwrap();

        let snapshot = registry_authority_snapshot(&home).unwrap();
        let targets = close_all_session_targets(&snapshot);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].session_name, "default");
        assert_eq!(
            targets[0]
                .authority_entry
                .as_ref()
                .map(|entry| entry.session_id.as_str()),
            Some("sess-live")
        );
        assert!(targets[0].stale_entries.is_empty());

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn close_all_rejects_committed_shutdown_when_profile_release_lags() {
        let disposition = classify_close_all_result(
            true,
            true,
            ShutdownFenceStatus {
                daemon_stopped: true,
                profile_released: false,
            },
            false,
        );
        assert_eq!(disposition, super::CloseAllDisposition::Failed);
    }

    #[test]
    fn replay_budget_exhaustion_maps_to_ipc_timeout() {
        let error = ipc_timeout_error(
            "replay budget exhausted",
            None,
            Some(serde_json::json!({
                "reason": "ipc_replay_budget_exhausted",
                "command": "doctor",
            })),
        );
        match error {
            rub_core::error::RubError::Domain(envelope) => {
                assert_eq!(envelope.code, ErrorCode::IpcTimeout);
                assert_eq!(
                    envelope
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("reason"))
                        .and_then(|value| value.as_str()),
                    Some("ipc_replay_budget_exhausted")
                );
            }
            other => panic!("expected domain timeout error, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn detach_daemon_session_surfaces_setsid_failure() {
        FORCE_SETSID_FAILURE.store(true, Ordering::SeqCst);
        let error = detach_daemon_session().expect_err("forced setsid failure");
        assert!(error.to_string().contains("forced setsid failure"));
    }

    #[test]
    fn replay_retry_requires_same_daemon_session_authority() {
        assert!(replay_retry_matches_daemon_authority(
            Some("sess-a"),
            Some("sess-a")
        ));
        assert!(!replay_retry_matches_daemon_authority(
            Some("sess-a"),
            Some("sess-b")
        ));
        assert!(!replay_retry_matches_daemon_authority(Some("sess-a"), None));
        assert!(!replay_retry_matches_daemon_authority(None, Some("sess-a")));
    }

    #[test]
    fn startup_lock_scopes_always_include_session_and_optionally_attachment() {
        assert_eq!(
            startup_lock_scope_keys("default", None),
            vec!["session-default".to_string()]
        );
        assert_eq!(
            startup_lock_scope_keys("default", Some("cdp:http://127.0.0.1:9222")),
            vec![
                "session-default".to_string(),
                "attachment-cdp:http://127.0.0.1:9222".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn acquire_startup_lock_times_out_under_contention() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let paths = RubPaths::new(&home);
        std::fs::create_dir_all(paths.startup_locks_dir()).unwrap();
        let held_path = paths.startup_lock_path("session-default");
        let held_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&held_path)
            .unwrap();
        try_lock_exclusive(&held_file).unwrap();

        let start = tokio::time::Instant::now();
        let error = acquire_startup_lock(&home, "default", None, 75)
            .await
            .expect_err("contention should time out");
        assert_eq!(error.into_envelope().code, ErrorCode::DaemonStartFailed);
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(50),
            "startup lock should wait rather than spin"
        );

        unlock(&held_file).unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn read_startup_error_falls_back_to_plaintext_envelope() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let error_path = home.join("daemon.error");
        std::fs::write(&error_path, "daemon failed before structured envelope")
            .expect("test fixture should be writable");

        let envelope = read_startup_error(&error_path).expect("fallback envelope should parse");
        assert_eq!(envelope.code, ErrorCode::DaemonStartFailed);
        assert_eq!(envelope.message, "daemon failed before structured envelope");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn startup_signal_paths_reflect_environment() {
        let ready = temp_home().join("ready.signal");
        let error = temp_home().join("error.signal");
        unsafe {
            std::env::set_var("RUB_DAEMON_READY_FILE", &ready);
            std::env::set_var("RUB_DAEMON_ERROR_FILE", &error);
        }
        let (ready_path, error_path) = startup_signal_paths();
        assert_eq!(ready_path.as_deref(), Some(ready.as_path()));
        assert_eq!(error_path.as_deref(), Some(error.as_path()));
        unsafe {
            std::env::remove_var("RUB_DAEMON_READY_FILE");
            std::env::remove_var("RUB_DAEMON_ERROR_FILE");
        }
    }

    #[test]
    fn existing_socket_paths_only_returns_actual_runtime_sockets() {
        let home = temp_home();
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        for path in [
            session_paths.socket_path(),
            session_paths.canonical_socket_path(),
        ] {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
        }
        std::fs::write(session_paths.canonical_socket_path(), b"").unwrap();
        std::fs::write(session_paths.socket_path(), b"").unwrap();

        assert_eq!(
            session_paths.existing_socket_paths(),
            vec![session_paths.socket_path()]
        );

        let _ = std::fs::remove_file(session_paths.socket_path());
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn wait_for_ready_requires_ready_commit_marker_and_handshake() {
        let home = temp_home();
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        let ready_file = session_paths.startup_ready_path("startup");
        let error_file = session_paths.startup_error_path("startup");
        let socket_path = session_paths.socket_path();
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("handshake accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: rub_ipc::protocol::IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read handshake")
                .expect("handshake request");
            assert_eq!(request.command, "_handshake");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": "sess-default",
                    "launch_policy": serde_json::to_value(LaunchPolicyInfo {
                        headless: true,
                        ignore_cert_errors: false,
                        hide_infobars: true,
                        user_data_dir: None,
                        connection_target: None,
                        stealth_level: None,
                        stealth_patches: None,
                        stealth_default_enabled: None,
                        humanize_enabled: None,
                        humanize_speed: None,
                        stealth_coverage: None,
                    }).expect("launch policy"),
                }),
            );
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write handshake response");

            let (stream, _) = listener.accept().await.expect("bound client accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: rub_ipc::protocol::IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read bound request")
                .expect("bound request");
            assert_eq!(request.command, "doctor");
            assert_eq!(request.daemon_session_id.as_deref(), Some("sess-default"));
            let response = IpcResponse::success("req-2", serde_json::json!({"ok": true}));
            NdJsonCodec::write(&mut writer, &response)
                .await
                .expect("write bound response");
        });

        let ready_path = ready_file.clone();
        let committed_path = session_paths.startup_committed_path();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            std::fs::write(ready_path, b"ready").expect("ready marker");
            std::fs::create_dir_all(
                committed_path
                    .parent()
                    .expect("commit marker should have a parent directory"),
            )
            .expect("commit marker parent");
            std::fs::write(committed_path, b"sess-default").expect("commit marker");
        });

        let signals = StartupSignalFiles {
            ready_file,
            error_file,
            daemon_pid: std::process::id(),
            session_id: "sess-default".to_string(),
        };

        let (mut client, daemon_session_id) = wait_for_ready(&home, "default", &signals, 3_000)
            .await
            .expect("startup readiness should require marker and handshake");
        assert_eq!(daemon_session_id, "sess-default");
        let response = client
            .send(&rub_ipc::protocol::IpcRequest::new(
                "doctor",
                serde_json::json!({}),
                1_000,
            ))
            .await
            .expect("bound client send");
        assert_eq!(response.status, ResponseStatus::Success);

        server.await.expect("server join");
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn wait_for_ready_fails_fast_when_daemon_dies_before_commit() {
        let home = temp_home();
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-dead");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        let ready_file = session_paths.startup_ready_path("startup");
        let error_file = session_paths.startup_error_path("startup");
        std::fs::write(&ready_file, b"ready").unwrap();

        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn short-lived child");
        let daemon_pid = child.id();
        let _ = child.wait_with_output().expect("wait child");

        let signals = StartupSignalFiles {
            ready_file,
            error_file,
            daemon_pid,
            session_id: "sess-dead".to_string(),
        };

        let error = wait_for_ready(&home, "default", &signals, 3_000)
            .await
            .err()
            .expect("dead child before commit must fail fast");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::DaemonStartFailed);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("daemon_exited_before_startup_commit")
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn wait_for_ready_reports_existing_error_before_tiny_deadline_times_out() {
        let home = temp_home();
        let session_paths = RubPaths::new(&home).session_runtime("default", "sess-error");
        std::fs::create_dir_all(session_paths.session_dir()).unwrap();
        let ready_file = session_paths.startup_ready_path("startup");
        let error_file = session_paths.startup_error_path("startup");
        std::fs::write(
            &error_file,
            serde_json::to_vec(&rub_core::error::ErrorEnvelope::new(
                ErrorCode::DaemonStartFailed,
                "structured startup failure",
            ))
            .unwrap(),
        )
        .unwrap();

        let signals = StartupSignalFiles {
            ready_file,
            error_file,
            daemon_pid: std::process::id(),
            session_id: "sess-error".to_string(),
        };

        let error = wait_for_ready(&home, "default", &signals, 1)
            .await
            .err()
            .expect("existing startup error must win over tiny timeout");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::DaemonStartFailed);
        assert_eq!(envelope.message, "structured startup failure");

        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn socket_candidates_require_authority_entry() {
        assert!(socket_candidates_for_session(None).unwrap().is_empty());

        let entry = RegistryEntry {
            session_id: "sess-default".to_string(),
            session_name: "default".to_string(),
            pid: std::process::id(),
            socket_path: std::env::temp_dir()
                .join(format!("rub-daemon-candidate-{}.sock", Uuid::now_v7()))
                .display()
                .to_string(),
            created_at: "2026-04-03T00:00:00Z".to_string(),
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        let path = std::path::PathBuf::from(&entry.socket_path);
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            socket_candidates_for_session(Some(&entry)).unwrap(),
            vec![path.clone()]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn command_match_requires_session_id_when_present() {
        let rub_home = Path::new("/tmp/rub-e2e-home");
        let command =
            "rub __daemon --session default --session-id sess-live --rub-home /tmp/rub-e2e-home";
        assert!(command_matches_daemon_identity(
            command,
            rub_home,
            "default",
            Some("sess-live"),
        ));
        assert!(!command_matches_daemon_identity(
            command,
            rub_home,
            "default",
            Some("sess-stale"),
        ));
    }
}
