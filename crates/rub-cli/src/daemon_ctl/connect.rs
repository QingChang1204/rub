use crate::connection_hardening::{
    AttemptError, ConnectionFailureClass, RetryAttribution, RetryFailure, RetryPolicy,
    classify_error_code, classify_io_transient, run_with_bounded_retry,
};
use crate::main_support::command_timeout_error;
use rub_core::error::{ErrorCode, RubError};
use rub_core::process::is_process_alive;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::{IpcClient, IpcClientError};
use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    DaemonConnection, DaemonCtlPathContext, HandshakePayload, cleanup_stale, daemon_ctl_path_error,
    daemon_ctl_path_state, daemon_ctl_socket_error, fetch_handshake_info,
    fetch_handshake_info_until, latest_definitely_stale_entry_by_name,
    latest_registry_entry_by_name, registry_entry_by_name, terminate_registry_entry_process,
};

pub(crate) enum TransientSocketPolicy {
    NeedStartBeforeLock,
    FailAfterLock,
}

#[derive(Debug, Clone, Copy)]
struct AttachBudget {
    deadline: Instant,
    timeout_ms: u64,
}

pub(crate) async fn maybe_upgrade_if_needed(
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
                "socket_path_state": daemon_ctl_path_state(
                    "daemon_ctl.upgrade.registry_entry.socket_path",
                    "registry_authority_entry",
                    "session_socket",
                ),
            }),
        ));
    }

    Ok(DaemonConnection::Connected {
        client: authority_bound_deferred_client(socket_path, &handshake.daemon_session_id)
            .map_err(|error| {
                daemon_ctl_socket_error(
                    ErrorCode::IpcProtocolError,
                    format!("Failed to bind connected daemon authority: {error}"),
                    socket_path,
                    "daemon_ctl.connect.socket_path",
                    "session_socket_candidates",
                    "connected_daemon_authority_bind_failed",
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
    apply_hard_cut_shutdown_outcome(rub_home, session_name, entry, shutdown)
}

pub(crate) fn apply_hard_cut_shutdown_outcome(
    rub_home: &Path,
    session_name: &str,
    entry: &rub_daemon::session::RegistryEntry,
    shutdown: ShutdownFenceStatus,
) -> Result<(), RubError> {
    if shutdown.fully_released() {
        cleanup_stale(rub_home, entry);
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::IpcVersionMismatch,
        format!(
            "Session '{session_name}' is still owned by an outdated daemon or browser profile after hard-cut upgrade fencing"
        ),
        serde_json::json!({
            "session": session_name,
            "daemon_protocol_version": entry.ipc_protocol_version,
            "cli_protocol_version": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
            "reason": "hard_cut_upgrade_fence_incomplete",
            "shutdown_fence": {
                "daemon_stopped": shutdown.daemon_stopped,
                "profile_released": shutdown.profile_released,
                "lifecycle_committed": shutdown.lifecycle_committed(),
                "fully_released": shutdown.fully_released(),
            },
            "user_data_dir": entry.user_data_dir,
            "user_data_dir_state": entry.user_data_dir.as_ref().map(|_| {
                daemon_ctl_path_state(
                    "daemon_ctl.upgrade.registry_entry.user_data_dir",
                    "registry_authority_entry",
                    "managed_user_data_dir",
                )
            }),
        }),
    ))
}

async fn probe_upgrade_check(client: &mut IpcClient, session_name: &str) -> Result<(), RubError> {
    client
        .send(&rub_ipc::protocol::IpcRequest::new(
            "_upgrade_check",
            serde_json::json!({}),
            3_000,
        ))
        .await
        .map(|_| ())
        .map_err(|error| upgrade_probe_send_error(session_name, error))
}

async fn probe_upgrade_check_until(
    client: &mut IpcClient,
    session_name: &str,
    budget: AttachBudget,
) -> Result<(), RubError> {
    let remaining_timeout_ms = remaining_budget_ms(budget.deadline);
    if remaining_timeout_ms == 0 {
        return Err(command_timeout_error(
            budget.timeout_ms,
            "existing_daemon_upgrade_check",
        ));
    }
    client
        .send(&rub_ipc::protocol::IpcRequest::new(
            "_upgrade_check",
            serde_json::json!({}),
            remaining_timeout_ms.max(1),
        ))
        .await
        .map(|_| ())
        .map_err(|error| upgrade_probe_send_error(session_name, error))
}

fn upgrade_probe_send_error(session_name: &str, error: IpcClientError) -> RubError {
    match error {
        IpcClientError::Protocol(envelope) => RubError::domain_with_context(
            envelope.code,
            format!(
                "Failed to fetch upgrade status for session '{session_name}': {}",
                envelope.message
            ),
            envelope.context.unwrap_or_else(|| serde_json::json!({})),
        ),
        IpcClientError::Transport(io_error) => {
            let mut context = serde_json::Map::from_iter([(
                "reason".to_string(),
                serde_json::json!("daemon_ctl_upgrade_check_transport_failed"),
            )]);
            if let Some(transport_reason) = classify_io_transient(&io_error) {
                context.insert(
                    "transport_reason".to_string(),
                    serde_json::json!(transport_reason),
                );
            }
            RubError::domain_with_context(
                ErrorCode::IpcProtocolError,
                format!("Failed to fetch upgrade status for session '{session_name}': {io_error}"),
                serde_json::Value::Object(context),
            )
        }
    }
}

fn transport_reason_from_error(error: &RubError) -> Option<String> {
    match error {
        RubError::Domain(envelope) => envelope
            .context
            .as_ref()
            .and_then(|context| context.get("transport_reason"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) async fn detect_or_connect_hardened(
    rub_home: &Path,
    session_name: &str,
    transient_socket_policy: TransientSocketPolicy,
) -> Result<DaemonConnection, RubError> {
    detect_or_connect_hardened_with_budget(rub_home, session_name, transient_socket_policy, None)
        .await
}

pub(crate) async fn detect_or_connect_hardened_until(
    rub_home: &Path,
    session_name: &str,
    transient_socket_policy: TransientSocketPolicy,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<DaemonConnection, RubError> {
    detect_or_connect_hardened_with_budget(
        rub_home,
        session_name,
        transient_socket_policy,
        Some(AttachBudget {
            deadline,
            timeout_ms,
        }),
    )
    .await
}

async fn detect_or_connect_hardened_with_budget(
    rub_home: &Path,
    session_name: &str,
    transient_socket_policy: TransientSocketPolicy,
    budget: Option<AttachBudget>,
) -> Result<DaemonConnection, RubError> {
    let authority_entry = registry_entry_by_name(rub_home, session_name)?;
    let socket_paths = socket_candidates_for_session(authority_entry.as_ref())?;

    if socket_paths.is_empty() {
        if let Some(entry) = latest_definitely_stale_entry_by_name(rub_home, session_name)? {
            cleanup_stale(rub_home, &entry);
        }
        return Ok(DaemonConnection::NeedStart);
    }

    let mut last_failure = None;
    for socket_path in socket_paths {
        let connect_result = if let Some(budget) = budget {
            connect_ipc_with_retry_until(
                &socket_path,
                budget,
                "existing_daemon_connect",
                ErrorCode::IpcProtocolError,
                "Failed to connect to an existing daemon socket",
                "daemon_ctl.connect.socket_path",
                "session_socket_candidates",
            )
            .await
        } else {
            connect_ipc_with_retry(
                &socket_path,
                ErrorCode::IpcProtocolError,
                "Failed to connect to an existing daemon socket",
                "daemon_ctl.connect.socket_path",
                "session_socket_candidates",
            )
            .await
        };
        match connect_result {
            Ok((mut handshake_client, _attribution)) => {
                let mut requires_handshake_reconnect = false;
                if let Some(entry) = authority_entry.as_ref()
                    && entry.ipc_protocol_version != rub_ipc::protocol::IPC_PROTOCOL_VERSION
                {
                    let upgrade_check = if let Some(budget) = budget {
                        probe_upgrade_check_until(&mut handshake_client, session_name, budget).await
                    } else {
                        probe_upgrade_check(&mut handshake_client, session_name).await
                    };
                    match upgrade_check {
                        Ok(()) => requires_handshake_reconnect = true,
                        Err(error) => {
                            if let Some(reason) = transport_reason_from_error(&error) {
                                last_failure = Some(RetryFailure {
                                    error,
                                    attribution: RetryAttribution {
                                        retry_count: 0,
                                        retry_reason: Some(reason),
                                    },
                                    final_failure_class: ConnectionFailureClass::TransportTransient,
                                });
                                continue;
                            }
                            hard_cut_outdated_daemon(rub_home, session_name, entry).await?;
                            return Ok(DaemonConnection::NeedStart);
                        }
                    }
                }
                if requires_handshake_reconnect {
                    let reconnect_result = if let Some(budget) = budget {
                        connect_ipc_with_retry_until(
                            &socket_path,
                            budget,
                            "existing_daemon_reconnect_after_upgrade",
                            ErrorCode::IpcProtocolError,
                            "Failed to reconnect to daemon socket after upgrade probe",
                            "daemon_ctl.connect.socket_path",
                            "session_socket_candidates",
                        )
                        .await
                    } else {
                        connect_ipc_with_retry(
                            &socket_path,
                            ErrorCode::IpcProtocolError,
                            "Failed to reconnect to daemon socket after upgrade probe",
                            "daemon_ctl.connect.socket_path",
                            "session_socket_candidates",
                        )
                        .await
                    };
                    let (reconnected_client, _attribution) =
                        reconnect_result.map_err(|failure| failure.into_error())?;
                    handshake_client = reconnected_client;
                }
                let handshake_result = if let Some(budget) = budget {
                    fetch_handshake_info_until(
                        &mut handshake_client,
                        budget.deadline,
                        budget.timeout_ms,
                        "existing_daemon_handshake",
                    )
                    .await
                } else {
                    fetch_handshake_info(&mut handshake_client).await
                };
                let handshake = match handshake_result {
                    Ok(handshake) => handshake,
                    Err(error) => {
                        if let Some(reason) = transport_reason_from_error(&error) {
                            last_failure = Some(RetryFailure {
                                error,
                                attribution: RetryAttribution {
                                    retry_count: 0,
                                    retry_reason: Some(reason),
                                },
                                final_failure_class: ConnectionFailureClass::TransportTransient,
                            });
                            continue;
                        }
                        return Err(error);
                    }
                };
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

pub(crate) fn authority_bound_deferred_client(
    socket_path: &Path,
    daemon_session_id: &str,
) -> Result<IpcClient, String> {
    IpcClient::deferred(socket_path.to_path_buf()).bind_daemon_session_id(daemon_session_id)
}

pub(crate) async fn connect_ipc_with_retry(
    socket_path: &Path,
    error_code: ErrorCode,
    message_prefix: impl AsRef<str>,
    path_authority: &str,
    upstream_truth: &str,
) -> Result<(IpcClient, RetryAttribution), RetryFailure> {
    let socket_path = socket_path.to_path_buf();
    let message_prefix = message_prefix.as_ref().to_string();
    let path_authority = path_authority.to_string();
    let upstream_truth = upstream_truth.to_string();
    run_with_bounded_retry(RetryPolicy::default(), move || {
        let socket_path = socket_path.clone();
        let message_prefix = message_prefix.clone();
        let path_authority = path_authority.clone();
        let upstream_truth = upstream_truth.clone();
        async move {
            IpcClient::connect(&socket_path).await.map_err(|error| {
                let message = format!("{} {}: {error}", message_prefix, socket_path.display());
                let rub_error = daemon_ctl_path_error(
                    error_code,
                    message,
                    DaemonCtlPathContext {
                        path_key: "socket_path",
                        path: &socket_path,
                        path_authority: &path_authority,
                        upstream_truth: &upstream_truth,
                        path_kind: "session_socket",
                        reason: "daemon_socket_connect_failed",
                    },
                );
                if let Some(reason) = classify_io_transient(&error) {
                    AttemptError::retryable(rub_error, reason)
                } else {
                    AttemptError::terminal(rub_error, classify_error_code(error_code))
                }
            })
        }
    })
    .await
}

async fn connect_ipc_with_retry_until(
    socket_path: &Path,
    budget: AttachBudget,
    phase: &'static str,
    error_code: ErrorCode,
    message_prefix: impl AsRef<str>,
    path_authority: &str,
    upstream_truth: &str,
) -> Result<(IpcClient, RetryAttribution), RetryFailure> {
    let socket_path = socket_path.to_path_buf();
    let message_prefix = message_prefix.as_ref().to_string();
    let path_authority = path_authority.to_string();
    let upstream_truth = upstream_truth.to_string();
    let policy = RetryPolicy::default();
    let mut attribution = RetryAttribution::default();

    loop {
        let Some(remaining) = remaining_budget_duration(budget.deadline) else {
            return Err(RetryFailure {
                error: command_timeout_error(budget.timeout_ms, phase),
                attribution,
                final_failure_class: ConnectionFailureClass::Unknown,
            });
        };

        let connect_attempt = tokio::time::timeout(remaining, IpcClient::connect(&socket_path))
            .await
            .map_err(|_| RetryFailure {
                error: command_timeout_error(budget.timeout_ms, phase),
                attribution: attribution.clone(),
                final_failure_class: ConnectionFailureClass::Unknown,
            })?;

        match connect_attempt {
            Ok(client) => return Ok((client, attribution)),
            Err(error) => {
                let message = format!("{} {}: {error}", message_prefix, socket_path.display());
                let rub_error = daemon_ctl_path_error(
                    error_code,
                    message,
                    DaemonCtlPathContext {
                        path_key: "socket_path",
                        path: &socket_path,
                        path_authority: &path_authority,
                        upstream_truth: &upstream_truth,
                        path_kind: "session_socket",
                        reason: "daemon_socket_connect_failed",
                    },
                );
                let Some(reason) = classify_io_transient(&error).map(str::to_string) else {
                    return Err(RetryFailure {
                        error: rub_error,
                        attribution,
                        final_failure_class: classify_error_code(error_code),
                    });
                };
                if attribution.retry_count >= policy.max_retries {
                    return Err(RetryFailure {
                        error: rub_error,
                        attribution,
                        final_failure_class: ConnectionFailureClass::TransportTransient,
                    });
                }
                attribution.retry_count += 1;
                attribution.retry_reason = Some(reason);
                let Some(delay_budget) = remaining_budget_duration(budget.deadline) else {
                    return Err(RetryFailure {
                        error: command_timeout_error(budget.timeout_ms, phase),
                        attribution,
                        final_failure_class: ConnectionFailureClass::Unknown,
                    });
                };
                tokio::time::sleep(policy.delay.min(delay_budget)).await;
            }
        }
    }
}

pub(crate) async fn connect_ipc_once(
    socket_path: &Path,
    error_code: ErrorCode,
    message_prefix: impl AsRef<str>,
    path_authority: &str,
    upstream_truth: &str,
) -> Result<IpcClient, AttemptError> {
    IpcClient::connect(socket_path).await.map_err(|error| {
        let message = format!(
            "{} {}: {error}",
            message_prefix.as_ref(),
            socket_path.display()
        );
        let rub_error = daemon_ctl_path_error(
            error_code,
            message,
            DaemonCtlPathContext {
                path_key: "socket_path",
                path: socket_path,
                path_authority,
                upstream_truth,
                path_kind: "session_socket",
                reason: "daemon_socket_connect_failed",
            },
        );
        if let Some(reason) = classify_io_transient(&error) {
            AttemptError::retryable(rub_error, reason)
        } else {
            AttemptError::terminal(rub_error, classify_error_code(error_code))
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShutdownFenceStatus {
    pub(crate) daemon_stopped: bool,
    pub(crate) profile_released: bool,
}

impl ShutdownFenceStatus {
    pub(crate) fn lifecycle_committed(self) -> bool {
        self.daemon_stopped
    }

    pub(crate) fn fully_released(self) -> bool {
        self.daemon_stopped && self.profile_released
    }
}

pub(crate) async fn wait_for_shutdown_until(
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ShutdownFenceStatus {
        daemon_stopped: false,
        profile_released: profile_released(entry),
    }
}

pub(crate) async fn wait_for_shutdown(
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

pub(crate) fn socket_candidates_for_session(
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

pub(crate) fn remaining_budget_duration(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}
