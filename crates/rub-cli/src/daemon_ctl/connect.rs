use crate::connection_hardening::{
    AttemptError, ConnectionFailureClass, RetryAttribution, RetryFailure, RetryPolicy,
    classify_error_code, classify_io_transient, run_with_bounded_retry,
};
use crate::main_support::command_timeout_error;
use rub_core::error::{ErrorCode, RubError};
use rub_core::process::is_process_alive;
use rub_daemon::rub_paths::RubPaths;
use rub_ipc::client::{IpcClient, IpcClientError};
use rub_ipc::protocol::{IpcRequest, UPGRADE_CHECK_PROBE_COMMAND_ID};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SocketPathIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime_sec: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

pub(crate) struct AuthorityBoundConnectSpec<'a> {
    pub(crate) phase: &'static str,
    pub(crate) error_code: ErrorCode,
    pub(crate) message_prefix: &'a str,
    pub(crate) path_authority: &'a str,
    pub(crate) upstream_truth: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AttachBudget {
    pub(crate) deadline: Instant,
    pub(crate) timeout_ms: u64,
}

pub(crate) async fn maybe_upgrade_if_needed(
    _rub_home: &Path,
    session_name: &str,
    authority_entry: Option<&rub_daemon::session::RegistryEntry>,
    handshake: &HandshakePayload,
    socket_path: &Path,
    handshake_socket_identity: Option<SocketPathIdentity>,
    budget: Option<AttachBudget>,
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
    if let Some(entry) = authority_entry {
        validate_handshake_attachment_identity(
            session_name,
            entry.attachment_identity.as_deref(),
            handshake.attachment_identity.as_deref(),
            socket_path,
            "daemon_ctl.connect.socket_path",
            "session_socket_candidates",
        )?;
    }

    Ok(DaemonConnection::Connected {
        client: authority_bound_connected_client(
            socket_path,
            &handshake.daemon_session_id,
            handshake_socket_identity,
            budget,
            AuthorityBoundConnectSpec {
                phase: "existing_daemon_authority_bind",
                error_code: ErrorCode::IpcProtocolError,
                message_prefix: "Failed to connect the verified daemon authority",
                path_authority: "daemon_ctl.connect.socket_path",
                upstream_truth: "session_socket_candidates",
            },
        )
        .await?,
        daemon_session_id: Some(handshake.daemon_session_id.clone()),
        authority_socket_path: socket_path.to_path_buf(),
    })
}

pub(crate) fn validate_handshake_attachment_identity(
    session_name: &str,
    expected_attachment_identity: Option<&str>,
    actual_attachment_identity: Option<&str>,
    socket_path: &Path,
    path_authority: &str,
    upstream_truth: &str,
) -> Result<(), RubError> {
    let Some(expected_attachment_identity) = expected_attachment_identity else {
        return Ok(());
    };
    match actual_attachment_identity {
        Some(actual_attachment_identity)
            if actual_attachment_identity == expected_attachment_identity =>
        {
            Ok(())
        }
        Some(actual_attachment_identity) => Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Handshake attachment authority for session '{session_name}' diverged from the canonical attachment identity"
            ),
            serde_json::json!({
                "reason": "handshake_attachment_identity_mismatch",
                "session": session_name,
                "expected_attachment_identity": expected_attachment_identity,
                "actual_attachment_identity": actual_attachment_identity,
                "socket_path": socket_path.display().to_string(),
                "socket_path_state": daemon_ctl_path_state(
                    "socket_path",
                    path_authority,
                    "session_socket",
                ),
                "upstream_truth": upstream_truth,
            }),
        )),
        None => Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Handshake attachment authority for session '{session_name}' omitted the canonical attachment identity"
            ),
            serde_json::json!({
                "reason": "handshake_attachment_identity_missing",
                "session": session_name,
                "expected_attachment_identity": expected_attachment_identity,
                "socket_path": socket_path.display().to_string(),
                "socket_path_state": daemon_ctl_path_state(
                    "socket_path",
                    path_authority,
                    "session_socket",
                ),
                "upstream_truth": upstream_truth,
            }),
        )),
    }
}

#[cfg(unix)]
pub(crate) fn current_socket_path_identity(
    socket_path: &Path,
    path_authority: &str,
    upstream_truth: &str,
    error_code: ErrorCode,
    reason: &str,
) -> Result<Option<SocketPathIdentity>, RubError> {
    let metadata = std::fs::symlink_metadata(socket_path).map_err(|error| {
        daemon_ctl_path_error(
            error_code,
            format!(
                "Failed to read socket identity for {}: {error}",
                socket_path.display()
            ),
            DaemonCtlPathContext {
                path_key: "socket_path",
                path: socket_path,
                path_authority,
                upstream_truth,
                path_kind: "session_socket",
                reason,
            },
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(daemon_ctl_path_error(
            error_code,
            format!(
                "Refusing to bind authority through non-socket path {}",
                socket_path.display()
            ),
            DaemonCtlPathContext {
                path_key: "socket_path",
                path: socket_path,
                path_authority,
                upstream_truth,
                path_kind: "session_socket",
                reason,
            },
        ));
    }
    Ok(Some(SocketPathIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        ctime_sec: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    }))
}

#[cfg(not(unix))]
pub(crate) fn current_socket_path_identity(
    _socket_path: &Path,
    _path_authority: &str,
    _upstream_truth: &str,
    _error_code: ErrorCode,
    _reason: &str,
) -> Result<Option<SocketPathIdentity>, RubError> {
    Ok(None)
}

pub(crate) fn verify_socket_path_identity(
    socket_path: &Path,
    expected_identity: Option<SocketPathIdentity>,
    spec: &AuthorityBoundConnectSpec<'_>,
) -> Result<(), RubError> {
    let Some(expected_identity) = expected_identity else {
        return Ok(());
    };
    let actual_identity = current_socket_path_identity(
        socket_path,
        spec.path_authority,
        spec.upstream_truth,
        spec.error_code,
        "verified_daemon_authority_socket_identity_read_failed",
    )?;
    if actual_identity == Some(expected_identity) {
        return Ok(());
    }
    Err(RubError::domain_with_context(
        spec.error_code,
        "Verified daemon socket authority changed before the execution connection committed",
        serde_json::json!({
            "reason": "verified_daemon_authority_socket_replaced",
            "socket_path": socket_path.display().to_string(),
            "socket_path_state": daemon_ctl_path_state(
                "socket_path",
                spec.path_authority,
                "session_socket",
            ),
            "upstream_truth": spec.upstream_truth,
            "expected_socket_identity": format!("{expected_identity:?}"),
            "actual_socket_identity": actual_identity.map(|identity| format!("{identity:?}")),
        }),
    ))
}

async fn hard_cut_outdated_daemon(
    rub_home: &Path,
    session_name: &str,
    entry: &rub_daemon::session::RegistryEntry,
    deadline: Option<Instant>,
) -> Result<(), RubError> {
    let _ = terminate_registry_entry_process(rub_home, entry);
    let shutdown = if let Some(deadline) = deadline {
        wait_for_shutdown_until(rub_home, entry, deadline).await
    } else {
        wait_for_shutdown(rub_home, entry).await
    };
    apply_hard_cut_shutdown_outcome(rub_home, session_name, entry, shutdown)
}

#[cfg(test)]
pub(crate) async fn hard_cut_outdated_daemon_until_for_test(
    rub_home: &Path,
    session_name: &str,
    entry: &rub_daemon::session::RegistryEntry,
    deadline: Instant,
) -> Result<(), RubError> {
    hard_cut_outdated_daemon(rub_home, session_name, entry, Some(deadline)).await
}

fn hard_cut_upgrade_fence_incomplete_error(
    session_name: &str,
    entry: &rub_daemon::session::RegistryEntry,
    shutdown: ShutdownFenceStatus,
) -> RubError {
    RubError::domain_with_context(
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
    )
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

    if shutdown.daemon_stopped
        && !shutdown.profile_released
        && entry.user_data_dir.as_deref().is_some()
    {
        rub_daemon::session::write_hard_cut_release_pending_proof(
            rub_home,
            &entry.session_name,
            &rub_daemon::session::HardCutReleasePendingProof {
                session_id: entry.session_id.clone(),
            },
        )
        .map_err(|error| {
            daemon_ctl_path_error(
                ErrorCode::IpcVersionMismatch,
                format!(
                    "Failed to persist hard-cut fallback authority for session '{session_name}': {error}"
                ),
                DaemonCtlPathContext {
                    path_key: "hard_cut_release_pending_path",
                    path: &RubPaths::new(rub_home)
                        .session(&entry.session_name)
                        .hard_cut_release_pending_path(),
                    path_authority: "daemon_ctl.hard_cut.release_pending_proof",
                    upstream_truth: "hard_cut_upgrade_fence_incomplete",
                    path_kind: "hard_cut_release_pending_file",
                    reason: "hard_cut_release_pending_proof_write_failed",
                },
            )
        })?;
    }

    Err(hard_cut_upgrade_fence_incomplete_error(
        session_name,
        entry,
        shutdown,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpgradeCheckOutcome {
    Idle(UpgradeCheckCompatibility),
    Busy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UpgradeCheckCompatibility {
    semantic_command_protocol_compatible: bool,
}

fn upgrade_check_probe_request(timeout_ms: u64, daemon_session_id: &str) -> IpcRequest {
    IpcRequest::new("_upgrade_check", serde_json::json!({}), timeout_ms)
        .with_command_id(UPGRADE_CHECK_PROBE_COMMAND_ID)
        .expect("upgrade-check probe command_id must be valid")
        .with_daemon_session_id(daemon_session_id)
        .expect("upgrade-check probe daemon_session_id must be valid")
}

fn handshake_requires_protocol_compatibility_fence(handshake: &HandshakePayload) -> bool {
    handshake.ipc_protocol_version != rub_ipc::protocol::IPC_PROTOCOL_VERSION
}

async fn probe_upgrade_check(
    client: &mut IpcClient,
    session_name: &str,
    daemon_session_id: &str,
) -> Result<UpgradeCheckCompatibility, RubError> {
    let response = client
        .send(&upgrade_check_probe_request(3_000, daemon_session_id))
        .await
        .map_err(|error| upgrade_probe_send_error(session_name, error))?;
    match validate_upgrade_check_response(session_name, response)? {
        UpgradeCheckOutcome::Idle(compatibility) => Ok(compatibility),
        UpgradeCheckOutcome::Busy => Err(RubError::domain_with_context(
            ErrorCode::SessionBusy,
            format!(
                "Session '{session_name}' is not idle enough to satisfy the hard-cut upgrade fence"
            ),
            serde_json::json!({
                "reason": "daemon_ctl_upgrade_check_not_idle",
            }),
        )),
    }
}

async fn probe_upgrade_check_until(
    client: &mut IpcClient,
    session_name: &str,
    daemon_session_id: &str,
    budget: AttachBudget,
) -> Result<UpgradeCheckCompatibility, RubError> {
    let remaining_timeout_ms = remaining_budget_ms(budget.deadline);
    if remaining_timeout_ms == 0 {
        return Err(command_timeout_error(
            budget.timeout_ms,
            "existing_daemon_upgrade_check",
        ));
    }
    let response = client
        .send(&upgrade_check_probe_request(
            remaining_timeout_ms.max(1),
            daemon_session_id,
        ))
        .await
        .map_err(|error| upgrade_probe_send_error(session_name, error))?;
    match validate_upgrade_check_response(session_name, response)? {
        UpgradeCheckOutcome::Idle(compatibility) => Ok(compatibility),
        UpgradeCheckOutcome::Busy => Err(RubError::domain_with_context(
            ErrorCode::SessionBusy,
            format!(
                "Session '{session_name}' is not idle enough to satisfy the hard-cut upgrade fence"
            ),
            serde_json::json!({
                "reason": "daemon_ctl_upgrade_check_not_idle",
            }),
        )),
    }
}

fn validate_upgrade_check_response(
    session_name: &str,
    response: rub_ipc::protocol::IpcResponse,
) -> Result<UpgradeCheckOutcome, RubError> {
    if response.status == rub_ipc::protocol::ResponseStatus::Error {
        if let Some(envelope) = response.error {
            return Err(RubError::domain_with_context(
                envelope.code,
                format!(
                    "Failed to fetch upgrade status for session '{session_name}': {}",
                    envelope.message
                ),
                envelope.context.unwrap_or_else(|| serde_json::json!({})),
            ));
        }
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Failed to fetch upgrade status for session '{session_name}': daemon returned error status without an envelope"
            ),
            serde_json::json!({
                "reason": "daemon_ctl_upgrade_check_response_missing_error_envelope",
            }),
        ));
    }

    let data = response.data.unwrap_or_default();
    let idle = data.get("idle").and_then(|value| value.as_bool()).ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Failed to fetch upgrade status for session '{session_name}': daemon returned malformed upgrade check payload"
            ),
            serde_json::json!({
                "reason": "daemon_ctl_upgrade_check_payload_invalid",
                "upgrade_check": data,
            }),
        )
    })?;
    Ok(if idle {
        UpgradeCheckOutcome::Idle(UpgradeCheckCompatibility {
            semantic_command_protocol_compatible: parse_upgrade_check_semantic_compatibility(
                session_name,
                &data,
            )?,
        })
    } else {
        UpgradeCheckOutcome::Busy
    })
}

fn parse_upgrade_check_semantic_compatibility(
    session_name: &str,
    data: &serde_json::Value,
) -> Result<bool, RubError> {
    let Some(value) = data.get("semantic_command_protocol") else {
        return Ok(false);
    };
    let Some(object) = value.as_object() else {
        return Err(upgrade_check_payload_invalid_error(
            session_name,
            data.clone(),
        ));
    };
    let compatible = object
        .get("compatible")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| upgrade_check_payload_invalid_error(session_name, data.clone()))?;
    let daemon_protocol_version = object
        .get("daemon_protocol_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| upgrade_check_payload_invalid_error(session_name, data.clone()))?;
    let compatible_cli_versions = object
        .get("compatible_cli_protocol_versions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| upgrade_check_payload_invalid_error(session_name, data.clone()))?;
    let cli_version_supported = compatible_cli_versions
        .iter()
        .any(|version| version.as_str() == Some(rub_ipc::protocol::IPC_PROTOCOL_VERSION));
    Ok(compatible && !daemon_protocol_version.is_empty() && cli_version_supported)
}

fn upgrade_check_payload_invalid_error(session_name: &str, data: serde_json::Value) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcProtocolError,
        format!(
            "Failed to fetch upgrade status for session '{session_name}': daemon returned malformed upgrade check payload"
        ),
        serde_json::json!({
            "reason": "daemon_ctl_upgrade_check_payload_invalid",
            "upgrade_check": data,
        }),
    )
}

fn upgrade_check_not_idle_error(error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(envelope)
            if envelope.code == ErrorCode::SessionBusy
                && envelope
                    .context
                    .as_ref()
                    .and_then(|context| context.get("reason"))
                    .and_then(|value| value.as_str())
                    == Some("daemon_ctl_upgrade_check_not_idle")
    )
}

fn upgrade_probe_send_error(session_name: &str, error: IpcClientError) -> RubError {
    match error {
        IpcClientError::Protocol(envelope) => {
            let transport_reason = super::ipc::replay_recoverable_protocol_reason(&envelope);
            let mut context = envelope
                .context
                .and_then(|context| context.as_object().cloned())
                .unwrap_or_default();
            if let Some(transport_reason) = transport_reason {
                context.insert(
                    "transport_reason".to_string(),
                    serde_json::json!(transport_reason),
                );
            }
            RubError::domain_with_context(
                envelope.code,
                format!(
                    "Failed to fetch upgrade status for session '{session_name}': {}",
                    envelope.message
                ),
                serde_json::Value::Object(context),
            )
        }
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

fn transport_transient_proves_selected_authority_stale(failure: &RetryFailure) -> bool {
    matches!(
        failure.attribution.retry_reason.as_deref(),
        Some("connection_refused" | "socket_not_found")
    )
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
        let latest_registry_entry = if authority_entry.is_none() {
            latest_registry_entry_by_name(rub_home, session_name)?
        } else {
            None
        };
        let preserved_entry = authority_entry.as_ref().or(latest_registry_entry.as_ref());
        if let Some(entry) = preserved_entry
            && rub_daemon::session::hard_cut_release_pending_blocks_entry(rub_home, entry)
        {
            return Err(hard_cut_upgrade_fence_incomplete_error(
                session_name,
                entry,
                ShutdownFenceStatus {
                    daemon_stopped: true,
                    profile_released: false,
                },
            ));
        }
        if let Some(entry) = preserved_entry
            && committed_registry_subject_still_owns_runtime(rub_home, entry)
        {
            return Err(missing_socket_preserves_registry_authority_error(
                session_name,
                entry,
            ));
        }
        if let Some(entry) = latest_definitely_stale_entry_by_name(rub_home, session_name)? {
            cleanup_stale(rub_home, &entry);
        }
        return Ok(DaemonConnection::NeedStart);
    }

    let mut last_failure = None;
    for socket_path in socket_paths {
        let handshake_socket_identity = current_socket_path_identity(
            &socket_path,
            "daemon_ctl.connect.socket_path",
            "session_socket_candidates",
            ErrorCode::IpcProtocolError,
            "verified_daemon_authority_socket_identity_read_failed",
        )?;
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
                let handshake_connect_spec = AuthorityBoundConnectSpec {
                    phase: "existing_daemon_handshake",
                    error_code: ErrorCode::IpcProtocolError,
                    message_prefix: "Failed to connect to an existing daemon socket",
                    path_authority: "daemon_ctl.connect.socket_path",
                    upstream_truth: "session_socket_candidates",
                };
                verify_socket_path_identity(
                    &socket_path,
                    handshake_socket_identity,
                    &handshake_connect_spec,
                )?;
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
                if let Some(entry) = authority_entry.as_ref()
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
                if let Some(entry) = authority_entry.as_ref() {
                    validate_handshake_attachment_identity(
                        session_name,
                        entry.attachment_identity.as_deref(),
                        handshake.attachment_identity.as_deref(),
                        &socket_path,
                        "daemon_ctl.connect.socket_path",
                        "session_socket_candidates",
                    )?;
                }
                if let Some(entry) = authority_entry.as_ref()
                    && handshake_requires_protocol_compatibility_fence(&handshake)
                {
                    verify_socket_path_identity(
                        &socket_path,
                        handshake_socket_identity,
                        &handshake_connect_spec,
                    )?;
                    let upgrade_check_connect_result = if let Some(budget) = budget {
                        connect_ipc_with_retry_until(
                            &socket_path,
                            budget,
                            "existing_daemon_upgrade_check",
                            ErrorCode::IpcProtocolError,
                            "Failed to reconnect to daemon socket for upgrade check",
                            "daemon_ctl.connect.socket_path",
                            "session_socket_candidates",
                        )
                        .await
                    } else {
                        connect_ipc_with_retry(
                            &socket_path,
                            ErrorCode::IpcProtocolError,
                            "Failed to reconnect to daemon socket for upgrade check",
                            "daemon_ctl.connect.socket_path",
                            "session_socket_candidates",
                        )
                        .await
                    };
                    let (mut upgrade_check_client, _attribution) =
                        upgrade_check_connect_result.map_err(|failure| failure.into_error())?;
                    verify_socket_path_identity(
                        &socket_path,
                        handshake_socket_identity,
                        &handshake_connect_spec,
                    )?;
                    let upgrade_check = if let Some(budget) = budget {
                        probe_upgrade_check_until(
                            &mut upgrade_check_client,
                            session_name,
                            &handshake.daemon_session_id,
                            budget,
                        )
                        .await
                    } else {
                        probe_upgrade_check(
                            &mut upgrade_check_client,
                            session_name,
                            &handshake.daemon_session_id,
                        )
                        .await
                    };
                    match upgrade_check {
                        Ok(compatibility) if compatibility.semantic_command_protocol_compatible => {
                        }
                        Ok(_) => {
                            hard_cut_outdated_daemon(
                                rub_home,
                                session_name,
                                entry,
                                budget.map(|budget| budget.deadline),
                            )
                            .await?;
                            return Ok(DaemonConnection::NeedStart);
                        }
                        Err(error) => {
                            if upgrade_check_not_idle_error(&error) {
                                return Err(error);
                            }
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
                            if matches!(
                                &error,
                                RubError::Domain(envelope)
                                    if envelope.code == ErrorCode::IpcProtocolError
                                        && envelope
                                            .context
                                            .as_ref()
                                            .and_then(|context| context.get("reason"))
                                            .and_then(|value| value.as_str())
                                            == Some("daemon_ctl_upgrade_check_payload_invalid")
                            ) {
                                return Err(error);
                            }
                            hard_cut_outdated_daemon(
                                rub_home,
                                session_name,
                                entry,
                                budget.map(|budget| budget.deadline),
                            )
                            .await?;
                            return Ok(DaemonConnection::NeedStart);
                        }
                    }
                }
                return maybe_upgrade_if_needed(
                    rub_home,
                    session_name,
                    authority_entry.as_ref(),
                    &handshake,
                    &socket_path,
                    handshake_socket_identity,
                    budget,
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
        if let Some(entry) = authority_entry.as_ref()
            && rub_daemon::session::hard_cut_release_pending_blocks_entry(rub_home, entry)
        {
            return Err(hard_cut_upgrade_fence_incomplete_error(
                session_name,
                entry,
                ShutdownFenceStatus {
                    daemon_stopped: true,
                    profile_released: false,
                },
            ));
        }
        tracing::warn!(
            session = session_name,
            pid = dead_pid,
            "Detected stale daemon after connect retry failure, cleaning up"
        );
        if let Some(entry) = authority_entry
            .as_ref()
            .cloned()
            .or(latest_registry_entry_by_name(rub_home, session_name)?)
        {
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
        if let Some(entry) = authority_entry.as_ref() {
            if rub_daemon::session::hard_cut_release_pending_blocks_entry(rub_home, entry) {
                return Err(hard_cut_upgrade_fence_incomplete_error(
                    session_name,
                    entry,
                    ShutdownFenceStatus {
                        daemon_stopped: false,
                        profile_released: false,
                    },
                ));
            }
            if transport_transient_proves_selected_authority_stale(&failure) {
                cleanup_stale(rub_home, entry);
            }
        }
        return Ok(DaemonConnection::NeedStart);
    }

    Err(failure.into_error())
}

fn committed_registry_subject_still_owns_runtime(
    rub_home: &Path,
    entry: &rub_daemon::session::RegistryEntry,
) -> bool {
    if !rub_core::process::is_process_alive(entry.pid) {
        return false;
    }
    let runtime = RubPaths::new(rub_home).session_runtime(&entry.session_name, &entry.session_id);
    let pid_matches = std::fs::read_to_string(runtime.pid_path())
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        == Some(entry.pid);
    let committed_matches = std::fs::read_to_string(runtime.startup_committed_path())
        .ok()
        .is_some_and(|raw| raw.trim() == entry.session_id);
    let socket_matches = Path::new(&entry.socket_path) == runtime.socket_path();
    pid_matches && committed_matches && socket_matches
}

fn missing_socket_preserves_registry_authority_error(
    session_name: &str,
    entry: &rub_daemon::session::RegistryEntry,
) -> RubError {
    daemon_ctl_path_error(
        ErrorCode::SessionBusy,
        format!(
            "Session '{session_name}' is still owned by a committed daemon authority, but its recorded socket path is unavailable"
        ),
        DaemonCtlPathContext {
            path_key: "socket_path",
            path: Path::new(&entry.socket_path),
            path_authority: "daemon_ctl.connect.registry_authority.socket_path",
            upstream_truth: "registry_authority_entry",
            path_kind: "session_socket",
            reason: "daemon_registry_authority_socket_missing",
        },
    )
}

pub(crate) async fn authority_bound_connected_client(
    socket_path: &Path,
    daemon_session_id: &str,
    expected_socket_identity: Option<SocketPathIdentity>,
    budget: Option<AttachBudget>,
    spec: AuthorityBoundConnectSpec<'_>,
) -> Result<IpcClient, RubError> {
    verify_socket_path_identity(socket_path, expected_socket_identity, &spec)?;
    let connect_result = if let Some(budget) = budget {
        connect_ipc_with_retry_until(
            socket_path,
            budget,
            spec.phase,
            spec.error_code,
            spec.message_prefix,
            spec.path_authority,
            spec.upstream_truth,
        )
        .await
        .map_err(|failure| failure.into_error())?
    } else {
        connect_ipc_with_retry(
            socket_path,
            spec.error_code,
            spec.message_prefix,
            spec.path_authority,
            spec.upstream_truth,
        )
        .await
        .map_err(|failure| failure.into_error())?
    };
    let (client, _attribution) = connect_result;
    verify_socket_path_identity(socket_path, expected_socket_identity, &spec)?;
    client
        .bind_daemon_session_id(daemon_session_id)
        .map_err(|error| {
            daemon_ctl_socket_error(
                spec.error_code,
                format!("Failed to bind connected daemon authority: {error}"),
                socket_path,
                spec.path_authority,
                spec.upstream_truth,
                "connected_daemon_authority_bind_failed",
            )
        })
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

pub(crate) async fn connect_ipc_with_retry_until(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_upgrade_check_idle_does_not_prove_semantic_protocol_compatibility() {
        let response = rub_ipc::protocol::IpcResponse::success(
            "req-1",
            serde_json::json!({
                "idle": true,
            }),
        );

        let outcome = validate_upgrade_check_response("default", response)
            .expect("legacy idle payload should remain parseable for hard-cut fencing");

        assert_eq!(
            outcome,
            UpgradeCheckOutcome::Idle(UpgradeCheckCompatibility {
                semantic_command_protocol_compatible: false,
            })
        );
    }

    #[test]
    fn upgrade_check_requires_explicit_semantic_protocol_compatibility_for_current_cli_version() {
        let response = rub_ipc::protocol::IpcResponse::success(
            "req-1",
            serde_json::json!({
                "idle": true,
                "semantic_command_protocol": {
                    "compatible": true,
                    "daemon_protocol_version": "14.0.0",
                    "compatible_cli_protocol_versions": [
                        rub_ipc::protocol::IPC_PROTOCOL_VERSION
                    ],
                },
            }),
        );

        let outcome = validate_upgrade_check_response("default", response)
            .expect("current semantic compatibility payload should parse");

        assert_eq!(
            outcome,
            UpgradeCheckOutcome::Idle(UpgradeCheckCompatibility {
                semantic_command_protocol_compatible: true,
            })
        );
    }

    #[test]
    fn busy_upgrade_check_is_a_drain_fence_not_hard_cut_authority() {
        let response = rub_ipc::protocol::IpcResponse::success(
            "req-1",
            serde_json::json!({
                "idle": false,
                "semantic_command_protocol": {
                    "compatible": true,
                    "daemon_protocol_version": "14.0.0",
                    "compatible_cli_protocol_versions": [
                        rub_ipc::protocol::IPC_PROTOCOL_VERSION
                    ],
                },
            }),
        );

        let outcome = validate_upgrade_check_response("default", response)
            .expect("busy upgrade check payload should remain parseable");

        assert_eq!(outcome, UpgradeCheckOutcome::Busy);
        let error = match outcome {
            UpgradeCheckOutcome::Busy => RubError::domain_with_context(
                ErrorCode::SessionBusy,
                "Session 'default' is not idle enough to satisfy the hard-cut upgrade fence",
                serde_json::json!({
                    "reason": "daemon_ctl_upgrade_check_not_idle",
                }),
            ),
            UpgradeCheckOutcome::Idle(_) => unreachable!("busy fixture"),
        };
        assert!(upgrade_check_not_idle_error(&error));
    }

    #[test]
    fn upgrade_check_rejects_malformed_semantic_protocol_payload() {
        let response = rub_ipc::protocol::IpcResponse::success(
            "req-1",
            serde_json::json!({
                "idle": true,
                "semantic_command_protocol": {
                    "compatible": true,
                    "daemon_protocol_version": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
                    "compatible_cli_protocol_versions": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
                },
            }),
        );

        let error = validate_upgrade_check_response("default", response)
            .expect_err("malformed semantic compatibility payload must fail closed")
            .into_envelope();

        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(serde_json::Value::as_str),
            Some("daemon_ctl_upgrade_check_payload_invalid")
        );
    }

    #[test]
    fn protocol_compatibility_fence_uses_live_handshake_authority_only() {
        let launch_policy = rub_core::model::LaunchPolicyInfo {
            headless: true,
            ignore_cert_errors: false,
            hide_infobars: false,
            user_data_dir: None,
            connection_target: None,
            stealth_level: None,
            stealth_patches: None,
            stealth_default_enabled: None,
            humanize_enabled: None,
            humanize_speed: None,
            stealth_coverage: None,
        };
        let current_handshake = HandshakePayload {
            daemon_session_id: "sess-default".to_string(),
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            launch_policy: launch_policy.clone(),
            attachment_identity: None,
        };
        let stale_handshake = HandshakePayload {
            daemon_session_id: "sess-default".to_string(),
            ipc_protocol_version: "0.9".to_string(),
            launch_policy,
            attachment_identity: None,
        };

        assert!(
            !handshake_requires_protocol_compatibility_fence(&current_handshake),
            "registry discovery must not force compatibility fencing when the live handshake already proves the current protocol"
        );
        assert!(
            handshake_requires_protocol_compatibility_fence(&stale_handshake),
            "the live handshake alone must trigger the compatibility fence when the daemon speaks an older protocol"
        );
    }
}
