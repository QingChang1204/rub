use crate::connection_hardening::ConnectionFailureClass;
use crate::timeout_budget::helpers::mutating_request;
use rub_core::error::{ErrorCode, RubError};
use rub_daemon::session::RegistrySessionSnapshot;
use std::path::Path;
use std::time::Instant;

use super::{
    AttachBudget, AuthorityBoundConnectSpec, CompatibilityDegradedOwnedSession, DaemonConnection,
    ExistingCloseOutcome, TransientSocketPolicy, compatibility_degraded_owned_from_snapshot,
    connect_ipc_with_retry_until, current_socket_path_identity, detect_or_connect_hardened_until,
    fetch_handshake_info_until, ipc_budget_exhausted_error, registry_authority_snapshot,
    send_existing_request_with_replay_recovery, verify_socket_path_identity,
};

fn augment_close_existing_error(
    error: RubError,
    session_name: &str,
    daemon_session_id: Option<&str>,
    command_id: Option<&str>,
) -> RubError {
    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert("session".to_string(), serde_json::json!(session_name));
    context.insert(
        "daemon_session_id".to_string(),
        serde_json::json!(daemon_session_id),
    );
    context.insert("command_id".to_string(), serde_json::json!(command_id));
    envelope.message = format!(
        "Failed to close existing session '{session_name}': {}",
        envelope.message
    );
    envelope.context = Some(serde_json::Value::Object(context));
    RubError::Domain(envelope)
}

fn close_existing_authority_resolution_error(
    session_name: &str,
    snapshot: &RegistrySessionSnapshot,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcProtocolError,
        format!(
            "Close target for session '{session_name}' has no authoritative live daemon and the registry projection is stale or inconclusive"
        ),
        serde_json::json!({
            "reason": "close_existing_authority_unavailable",
            "session": session_name,
            "latest_entry": snapshot.latest_entry().map(|entry| {
                serde_json::json!({
                    "daemon_session_id": entry.entry.session_id,
                    "liveness": format!("{:?}", entry.liveness),
                })
            }),
            "stale_session_ids": snapshot.stale_entries().into_iter().map(|entry| entry.session_id).collect::<Vec<_>>(),
            "has_uncertain_entries": snapshot.has_uncertain_entries(),
        }),
    )
}

fn close_existing_authority_disappeared_error(
    session_name: &str,
    authoritative_daemon_session_id: &str,
    expected_daemon_session_id: Option<&str>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcProtocolError,
        format!(
            "Close target for session '{session_name}' lost its authoritative live daemon before dispatch"
        ),
        serde_json::json!({
            "reason": "close_existing_authority_disappeared_before_dispatch",
            "session": session_name,
            "authoritative_daemon_session_id": authoritative_daemon_session_id,
            "expected_daemon_session_id": expected_daemon_session_id,
        }),
    )
}

fn close_existing_compatibility_degraded_owned_error(
    session_name: &str,
    compatibility_degraded_owned: &CompatibilityDegradedOwnedSession,
    expected_daemon_session_id: Option<&str>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::SessionBusy,
        format!(
            "Close target for session '{session_name}' is still owned but compatibility-degraded, so dispatch must fail closed"
        ),
        serde_json::json!({
            "reason": "close_existing_compatibility_degraded_but_owned",
            "session": session_name,
            "expected_daemon_session_id": expected_daemon_session_id,
            "compatibility_degraded_owned": compatibility_degraded_owned,
        }),
    )
}

fn should_normalize_close_existing_connect_error(
    error: &RubError,
    compatibility_degraded_owned: &CompatibilityDegradedOwnedSession,
) -> bool {
    matches!(
        error,
        RubError::Domain(envelope)
            if envelope.code == ErrorCode::IpcVersionMismatch
                && envelope
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.get("reason"))
                    .and_then(|value| value.as_str())
                    == Some("hard_cut_upgrade_fence_incomplete")
                && matches!(
                    compatibility_degraded_owned.reason,
                    crate::daemon_ctl::compatibility::CompatibilityDegradedOwnedReason::ProtocolIncompatible
                        | crate::daemon_ctl::compatibility::CompatibilityDegradedOwnedReason::HardCutReleasePending
                )
    )
}

fn should_normalize_close_selector_resolution_error(
    error: &RubError,
    compatibility_degraded_owned: &CompatibilityDegradedOwnedSession,
) -> bool {
    !matches!(
        error,
        RubError::Domain(envelope) if envelope.code == ErrorCode::IpcTimeout
    ) && matches!(
        compatibility_degraded_owned.reason,
        crate::daemon_ctl::compatibility::CompatibilityDegradedOwnedReason::ProtocolIncompatible
            | crate::daemon_ctl::compatibility::CompatibilityDegradedOwnedReason::HardCutReleasePending
    )
}

fn close_selector_authority_resolution_error(
    requested_attachment_identity: &str,
    candidate_entries: &[rub_daemon::session::RegistryEntry],
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcProtocolError,
        format!(
            "Close selector matched projected daemon authority for attachment '{}' but no authoritative live daemon confirmed that binding",
            requested_attachment_identity
        ),
        serde_json::json!({
            "reason": "close_selector_authority_unavailable",
            "requested_attachment_identity": requested_attachment_identity,
            "projected_candidates": candidate_entries.iter().map(|entry| {
                serde_json::json!({
                    "session": entry.session_name,
                    "daemon_session_id": entry.session_id,
                    "socket_path": entry.socket_path,
                })
            }).collect::<Vec<_>>(),
        }),
    )
}

fn close_selector_compatibility_degraded_owned_error(
    requested_attachment_identity: &str,
    compatibility_degraded_owned: &[CompatibilityDegradedOwnedSession],
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::SessionBusy,
        format!(
            "Close selector matched owned attachment authority for '{}' but every candidate remained compatibility-degraded",
            requested_attachment_identity
        ),
        serde_json::json!({
            "reason": "close_selector_compatibility_degraded_but_owned",
            "requested_attachment_identity": requested_attachment_identity,
            "compatibility_degraded_owned_sessions": compatibility_degraded_owned,
        }),
    )
}

fn close_selector_handshake_transport_failed(error: &RubError) -> bool {
    matches!(
        error,
        RubError::Domain(envelope)
            if matches!(
                envelope
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.get("reason"))
                    .and_then(|value| value.as_str()),
                Some(
                    "handshake_transport_failed"
                        | "ipc_response_transport_failure_after_request_commit"
                )
            )
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingCloseTargetAuthority {
    pub(crate) session_name: String,
    pub(crate) daemon_session_id: String,
}

#[cfg(test)]
pub(crate) async fn close_existing_session_until(
    rub_home: &Path,
    session_name: &str,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<ExistingCloseOutcome, RubError> {
    close_existing_session_targeted_until(rub_home, session_name, None, deadline, timeout_ms).await
}

pub(crate) async fn close_existing_session_targeted_until(
    rub_home: &Path,
    session_name: &str,
    expected_daemon_session_id: Option<&str>,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<ExistingCloseOutcome, RubError> {
    if !rub_home.exists() {
        return Ok(ExistingCloseOutcome::Noop);
    }
    if super::remaining_budget_ms(deadline) == 0 {
        return Err(ipc_budget_exhausted_error(
            None,
            timeout_ms.max(1),
            "existing_daemon_connect",
        ));
    }

    let snapshot = registry_authority_snapshot(rub_home)?;
    let Some(session_snapshot) = snapshot.session(session_name) else {
        return Ok(ExistingCloseOutcome::Noop);
    };
    let Some(authoritative_entry) = session_snapshot.authoritative_entry() else {
        return Err(close_existing_authority_resolution_error(
            session_name,
            session_snapshot,
        ));
    };
    let compatibility_degraded_owned =
        compatibility_degraded_owned_from_snapshot(authoritative_entry);

    let connection = match detect_or_connect_hardened_until(
        rub_home,
        session_name,
        TransientSocketPolicy::FailAfterLock,
        deadline,
        timeout_ms.max(1),
    )
    .await
    {
        Ok(connection) => connection,
        Err(error)
            if compatibility_degraded_owned
                .as_ref()
                .is_some_and(|degraded| {
                    should_normalize_close_existing_connect_error(&error, degraded)
                }) =>
        {
            return Err(close_existing_compatibility_degraded_owned_error(
                session_name,
                compatibility_degraded_owned
                    .as_ref()
                    .expect("normalization checked degraded compatibility authority"),
                expected_daemon_session_id,
            ));
        }
        Err(error) => return Err(error),
    };
    let (mut client, daemon_session_id) = match connection {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
            ..
        } => (client, daemon_session_id),
        DaemonConnection::NeedStart => {
            return Err(
                if let Some(compatibility_degraded_owned) = compatibility_degraded_owned.as_ref() {
                    close_existing_compatibility_degraded_owned_error(
                        session_name,
                        compatibility_degraded_owned,
                        expected_daemon_session_id,
                    )
                } else {
                    close_existing_authority_disappeared_error(
                        session_name,
                        authoritative_entry.entry.session_id.as_str(),
                        expected_daemon_session_id,
                    )
                },
            );
        }
    };
    if let Some(expected) = expected_daemon_session_id
        && daemon_session_id.as_deref() != Some(expected)
    {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!("Close authority changed before dispatch for session '{session_name}'"),
            serde_json::json!({
                "reason": "close_existing_authority_mismatch",
                "session": session_name,
                "expected_daemon_session_id": expected,
                "actual_daemon_session_id": daemon_session_id,
            }),
        ));
    }

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
        augment_close_existing_error(
            error,
            session_name,
            daemon_session_id.as_deref(),
            request.command_id.as_deref(),
        )
    })?;
    Ok(ExistingCloseOutcome::Closed(Box::new(response)))
}

pub(crate) async fn resolve_existing_close_target_by_attachment_identity_until(
    rub_home: &Path,
    requested_attachment_identity: &str,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<Option<ExistingCloseTargetAuthority>, RubError> {
    if !rub_home.exists() {
        return Ok(None);
    }
    if super::remaining_budget_ms(deadline) == 0 {
        return Err(ipc_budget_exhausted_error(
            None,
            timeout_ms.max(1),
            "close_selector_resolution",
        ));
    }

    let snapshot = registry_authority_snapshot(rub_home)?;
    if snapshot.sessions.is_empty() {
        return Ok(None);
    }

    let candidate_entries = snapshot
        .sessions
        .iter()
        .filter_map(|session| {
            session.authoritative_entry().map(|entry| {
                (
                    entry.entry.clone(),
                    compatibility_degraded_owned_from_snapshot(entry),
                )
            })
        })
        .filter(|(entry, _)| {
            entry.attachment_identity.as_deref() == Some(requested_attachment_identity)
        })
        .collect::<Vec<_>>();
    if candidate_entries.is_empty() {
        return Ok(None);
    }

    let mut matches = Vec::new();
    for (entry, compatibility_degraded_owned) in &candidate_entries {
        let remaining_timeout_ms = super::remaining_budget_ms(deadline);
        if remaining_timeout_ms == 0 {
            return Err(ipc_budget_exhausted_error(
                None,
                timeout_ms.max(1),
                "close_selector_resolution",
            ));
        }
        let attach_budget = AttachBudget {
            deadline,
            timeout_ms: timeout_ms.max(1),
        };
        let socket_path = std::path::PathBuf::from(&entry.socket_path);
        let socket_identity = match current_socket_path_identity(
            &socket_path,
            "daemon_ctl.close_selector.socket_path",
            "registry_authority_entry",
            ErrorCode::IpcProtocolError,
            "close_selector_socket_identity_read_failed",
        ) {
            Ok(identity) => identity,
            Err(error) if close_selector_handshake_transport_failed(&error) => continue,
            Err(error)
                if compatibility_degraded_owned
                    .as_ref()
                    .is_some_and(|degraded| {
                        should_normalize_close_selector_resolution_error(&error, degraded)
                    }) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let (mut client, _attribution) = match connect_ipc_with_retry_until(
            &socket_path,
            attach_budget,
            "close_selector_resolution",
            ErrorCode::IpcProtocolError,
            "Failed to connect to existing daemon while resolving close selector authority",
            "daemon_ctl.close_selector.socket_path",
            "registry_authority_entry",
        )
        .await
        {
            Ok(connected) => connected,
            Err(failure)
                if matches!(
                    failure.final_failure_class,
                    ConnectionFailureClass::TransportTransient
                ) =>
            {
                continue;
            }
            Err(failure)
                if compatibility_degraded_owned
                    .as_ref()
                    .is_some_and(|degraded| {
                        should_normalize_close_selector_resolution_error(&failure.error, degraded)
                    }) =>
            {
                continue;
            }
            Err(failure) => return Err(failure.into_error()),
        };
        let identity_spec = AuthorityBoundConnectSpec {
            phase: "close_selector_resolution",
            error_code: ErrorCode::IpcProtocolError,
            message_prefix: "Failed to bind close selector authority",
            path_authority: "daemon_ctl.close_selector.socket_path",
            upstream_truth: "registry_authority_entry",
        };
        verify_socket_path_identity(&socket_path, socket_identity, &identity_spec)?;
        let handshake = match fetch_handshake_info_until(
            &mut client,
            deadline,
            timeout_ms.max(1),
            "close_selector_resolution",
        )
        .await
        {
            Ok(handshake) => handshake,
            Err(error) if close_selector_handshake_transport_failed(&error) => continue,
            Err(error)
                if compatibility_degraded_owned
                    .as_ref()
                    .is_some_and(|degraded| {
                        should_normalize_close_selector_resolution_error(&error, degraded)
                    }) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        verify_socket_path_identity(&socket_path, socket_identity, &identity_spec)?;
        if handshake.daemon_session_id != entry.session_id {
            continue;
        }
        if handshake.attachment_identity.as_deref() != Some(requested_attachment_identity) {
            continue;
        }
        matches.push(ExistingCloseTargetAuthority {
            session_name: entry.session_name.clone(),
            daemon_session_id: entry.session_id.clone(),
        });
    }

    if matches.len() > 1 {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!(
                "Close selector matched multiple live daemon authorities for attachment '{}'",
                requested_attachment_identity
            ),
            serde_json::json!({
                "reason": "close_selector_resolves_to_multiple_live_sessions",
                "requested_attachment_identity": requested_attachment_identity,
                "matches": matches.iter().map(|entry| {
                    serde_json::json!({
                        "session": entry.session_name,
                        "daemon_session_id": entry.daemon_session_id,
                    })
                }).collect::<Vec<_>>(),
            }),
        ));
    }

    if matches.is_empty() {
        let compatibility_degraded_owned = candidate_entries
            .iter()
            .filter_map(|(_, compatibility_degraded_owned)| compatibility_degraded_owned.clone())
            .collect::<Vec<_>>();
        return Err(if compatibility_degraded_owned.is_empty() {
            close_selector_authority_resolution_error(
                requested_attachment_identity,
                &candidate_entries
                    .iter()
                    .map(|(entry, _)| entry.clone())
                    .collect::<Vec<_>>(),
            )
        } else {
            close_selector_compatibility_degraded_owned_error(
                requested_attachment_identity,
                &compatibility_degraded_owned,
            )
        });
    }

    Ok(matches.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::{
        ExistingCloseTargetAuthority, augment_close_existing_error,
        close_existing_compatibility_degraded_owned_error, close_existing_session_targeted_until,
        close_selector_compatibility_degraded_owned_error,
        resolve_existing_close_target_by_attachment_identity_until,
        should_normalize_close_existing_connect_error,
        should_normalize_close_selector_resolution_error,
    };
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::model::LaunchPolicyInfo;
    use rub_daemon::rub_paths::RubPaths;
    use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
    use rub_ipc::codec::NdJsonCodec;
    use rub_ipc::protocol::IpcResponse;
    use std::path::PathBuf;
    use tokio::io::BufReader;
    use tokio::net::UnixListener;
    use uuid::Uuid;

    use crate::daemon_ctl::{CompatibilityDegradedOwnedReason, CompatibilityDegradedOwnedSession};

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("rub-close-existing-{}", Uuid::now_v7()))
    }

    #[test]
    fn close_existing_error_preserves_original_error_code() {
        let error = RubError::domain_with_context(
            ErrorCode::IpcTimeout,
            "IPC timeout: replay send exhausted budget",
            serde_json::json!({
                "reason": "ipc_replay_budget_exhausted",
            }),
        );

        let augmented =
            augment_close_existing_error(error, "default", Some("sess-1"), Some("cmd-1"));
        let envelope = augmented.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason")),
            Some(&serde_json::json!("ipc_replay_budget_exhausted"))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("daemon_session_id")),
            Some(&serde_json::json!("sess-1"))
        );
        assert!(
            envelope
                .message
                .contains("Failed to close existing session 'default'"),
            "{}",
            envelope.message
        );
    }

    #[test]
    fn close_existing_compatibility_degraded_owned_error_uses_shared_family() {
        let envelope = close_existing_compatibility_degraded_owned_error(
            "default",
            &CompatibilityDegradedOwnedSession {
                session: "default".to_string(),
                daemon_session_id: "sess-default".to_string(),
                reason: CompatibilityDegradedOwnedReason::ProtocolIncompatible,
            },
            Some("sess-default"),
        )
        .into_envelope();
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&serde_json::json!(
                "close_existing_compatibility_degraded_but_owned"
            ))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("compatibility_degraded_owned"))
                .and_then(|value| value.get("reason")),
            Some(&serde_json::json!("protocol_incompatible"))
        );
    }

    #[test]
    fn close_selector_compatibility_degraded_owned_error_uses_shared_family() {
        let envelope = close_selector_compatibility_degraded_owned_error(
            "profile:/tmp/a/Profile 1",
            &[CompatibilityDegradedOwnedSession {
                session: "default".to_string(),
                daemon_session_id: "sess-default".to_string(),
                reason: CompatibilityDegradedOwnedReason::HardCutReleasePending,
            }],
        )
        .into_envelope();
        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&serde_json::json!(
                "close_selector_compatibility_degraded_but_owned"
            ))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("compatibility_degraded_owned_sessions"))
                .and_then(|value| value.get(0))
                .and_then(|value| value.get("reason")),
            Some(&serde_json::json!("hard_cut_release_pending"))
        );
    }

    #[test]
    fn close_existing_normalizes_hard_cut_incomplete_into_shared_family() {
        let error = RubError::domain_with_context(
            ErrorCode::IpcVersionMismatch,
            "hard-cut incomplete",
            serde_json::json!({
                "reason": "hard_cut_upgrade_fence_incomplete",
            }),
        );
        assert!(should_normalize_close_existing_connect_error(
            &error,
            &CompatibilityDegradedOwnedSession {
                session: "default".to_string(),
                daemon_session_id: "sess-default".to_string(),
                reason: CompatibilityDegradedOwnedReason::ProtocolIncompatible,
            }
        ));
    }

    #[test]
    fn close_selector_normalizes_ingress_protocol_failures_into_shared_family() {
        let error = RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "ingress protocol failed",
            serde_json::json!({
                "reason": "ipc_request_protocol_mismatch",
            }),
        );
        assert!(should_normalize_close_selector_resolution_error(
            &error,
            &CompatibilityDegradedOwnedSession {
                session: "default".to_string(),
                daemon_session_id: "sess-default".to_string(),
                reason: CompatibilityDegradedOwnedReason::ProtocolIncompatible,
            }
        ));
    }

    #[tokio::test]
    async fn resolve_close_target_by_attachment_identity_finds_unique_live_match() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let default_runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        let work_runtime = RubPaths::new(&home).session_runtime("work", "sess-work");
        std::fs::create_dir_all(default_runtime.session_dir()).unwrap();
        std::fs::create_dir_all(work_runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            default_runtime
                .startup_committed_path()
                .parent()
                .expect("default startup commit parent"),
        )
        .unwrap();
        std::fs::create_dir_all(
            work_runtime
                .startup_committed_path()
                .parent()
                .expect("work startup commit parent"),
        )
        .unwrap();
        std::fs::create_dir_all(
            default_runtime
                .socket_path()
                .parent()
                .expect("default socket parent should exist"),
        )
        .unwrap();
        std::fs::create_dir_all(
            work_runtime
                .socket_path()
                .parent()
                .expect("work socket parent should exist"),
        )
        .unwrap();
        std::fs::write(default_runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(work_runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(default_runtime.startup_committed_path(), "sess-default").unwrap();
        std::fs::write(work_runtime.startup_committed_path(), "sess-work").unwrap();

        let default_listener = UnixListener::bind(default_runtime.socket_path()).unwrap();
        let work_listener = UnixListener::bind(work_runtime.socket_path()).unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![
                    RegistryEntry {
                        session_id: "sess-default".to_string(),
                        session_name: "default".to_string(),
                        pid: std::process::id(),
                        socket_path: default_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-16T00:00:00Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: Some("profile:/tmp/a/Profile 1".to_string()),
                        connection_target: None,
                    },
                    RegistryEntry {
                        session_id: "sess-work".to_string(),
                        session_name: "work".to_string(),
                        pid: std::process::id(),
                        socket_path: work_runtime.socket_path().display().to_string(),
                        created_at: "2026-04-16T00:00:01Z".to_string(),
                        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                        user_data_dir: None,
                        attachment_identity: Some("profile:/tmp/b/Profile 2".to_string()),
                        connection_target: None,
                    },
                ],
            },
        )
        .unwrap();

        let default_server = tokio::spawn(async move {
            loop {
                let Ok(Ok((stream, _))) = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    default_listener.accept(),
                )
                .await
                else {
                    break;
                };
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let Some(request): Option<serde_json::Value> =
                    NdJsonCodec::read(&mut reader).await.unwrap()
                else {
                    continue;
                };
                assert_eq!(request["command"], "_handshake");
                assert_eq!(
                    request["command_id"],
                    serde_json::json!(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
                );
                let response = IpcResponse::success(
                    "handshake-default",
                    serde_json::json!({
                        "daemon_session_id": "sess-default",
                        "launch_policy": {
                            "headless": true,
                            "ignore_cert_errors": false,
                            "hide_infobars": false
                        },
                        "attachment_identity": "profile:/tmp/a/Profile 1"
                    }),
                )
                .with_command_id(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
                .expect("handshake probe command_id must be valid")
                .with_daemon_session_id("sess-default")
                .expect("daemon_session_id must be valid");
                let _ = NdJsonCodec::write(&mut writer, &response).await;
            }
        });
        let work_server = tokio::spawn(async move {
            loop {
                let Ok(Ok((stream, _))) = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    work_listener.accept(),
                )
                .await
                else {
                    break;
                };
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let Some(request): Option<serde_json::Value> =
                    NdJsonCodec::read(&mut reader).await.unwrap()
                else {
                    continue;
                };
                assert_eq!(request["command"], "_handshake");
                assert_eq!(
                    request["command_id"],
                    serde_json::json!(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
                );
                let response = IpcResponse::success(
                    "handshake-work",
                    serde_json::json!({
                        "daemon_session_id": "sess-work",
                        "launch_policy": {
                            "headless": true,
                            "ignore_cert_errors": false,
                            "hide_infobars": false
                        },
                        "attachment_identity": "profile:/tmp/b/Profile 2"
                    }),
                )
                .with_command_id(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
                .expect("handshake probe command_id must be valid")
                .with_daemon_session_id("sess-work")
                .expect("daemon_session_id must be valid");
                let _ = NdJsonCodec::write(&mut writer, &response).await;
            }
        });

        let resolved = resolve_existing_close_target_by_attachment_identity_until(
            &home,
            "profile:/tmp/b/Profile 2",
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            2_000,
        )
        .await
        .unwrap();
        assert_eq!(
            resolved,
            Some(ExistingCloseTargetAuthority {
                session_name: "work".to_string(),
                daemon_session_id: "sess-work".to_string(),
            })
        );

        default_server.await.unwrap();
        work_server.await.unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn close_existing_session_targeted_rejects_authority_mismatch() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-actual");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit parent"),
        )
        .unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-actual").unwrap();
        std::fs::create_dir_all(
            runtime
                .socket_path()
                .parent()
                .expect("socket path parent should exist"),
        )
        .unwrap();
        let listener = UnixListener::bind(runtime.socket_path()).unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-actual".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: runtime.socket_path().display().to_string(),
                    created_at: "2026-04-16T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let server = tokio::spawn(async move {
            loop {
                let Ok(Ok((stream, _))) =
                    tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept())
                        .await
                else {
                    break;
                };
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let Some(request): Option<serde_json::Value> =
                    NdJsonCodec::read(&mut reader).await.unwrap()
                else {
                    continue;
                };
                assert_eq!(request["command"], "_handshake");
                assert_eq!(
                    request["command_id"],
                    serde_json::json!(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
                );
                let response = IpcResponse::success(
                    "handshake",
                    serde_json::json!({
                        "daemon_session_id": "sess-actual",
                        "launch_policy": {
                            "headless": true,
                            "ignore_cert_errors": false,
                            "hide_infobars": false
                        }
                    }),
                )
                .with_command_id(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
                .expect("handshake probe command_id must be valid")
                .with_daemon_session_id("sess-actual")
                .expect("daemon_session_id must be valid");
                let _ = NdJsonCodec::write(&mut writer, &response).await;
            }
        });

        let error = close_existing_session_targeted_until(
            &home,
            "default",
            Some("sess-other"),
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            2_000,
        )
        .await
        .expect_err("authority mismatch must fail closed")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&serde_json::json!("close_existing_authority_mismatch"))
        );

        server.await.unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn close_existing_session_rejects_stale_session_projection() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-stale");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit parent"),
        )
        .unwrap();
        std::fs::write(runtime.pid_path(), "999999").unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-stale").unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-stale".to_string(),
                    session_name: "default".to_string(),
                    pid: 999_999,
                    socket_path: runtime.socket_path().display().to_string(),
                    created_at: "2026-04-17T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let error = close_existing_session_targeted_until(
            &home,
            "default",
            None,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            1_000,
        )
        .await
        .expect_err("stale session projection must fail closed")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&serde_json::json!("close_existing_authority_unavailable"))
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn close_existing_session_targeted_until_respects_callers_shared_deadline() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit parent"),
        )
        .unwrap();
        std::fs::create_dir_all(
            runtime
                .socket_path()
                .parent()
                .expect("socket path parent should exist"),
        )
        .unwrap();
        let _listener = UnixListener::bind(runtime.socket_path()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-default").unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-default".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: runtime.socket_path().display().to_string(),
                    created_at: "2026-04-18T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let started = std::time::Instant::now();
        let error = close_existing_session_targeted_until(
            &home,
            "default",
            None,
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(1))
                .unwrap(),
            1_000,
        )
        .await
        .expect_err("shared close deadline exhaustion must fail closed")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcTimeout);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("phase"))
                .and_then(|value| value.as_str()),
            Some("existing_daemon_connect")
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "close dispatch must respect the caller-owned deadline instead of restarting timeout"
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn resolve_close_target_by_attachment_identity_rejects_stale_projection_without_live_match()
     {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit parent"),
        )
        .unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-default").unwrap();
        std::fs::create_dir_all(
            runtime
                .socket_path()
                .parent()
                .expect("socket path parent should exist"),
        )
        .unwrap();
        let listener = UnixListener::bind(runtime.socket_path()).unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-default".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: runtime.socket_path().display().to_string(),
                    created_at: "2026-04-17T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: Some("/tmp/profile".to_string()),
                    attachment_identity: Some("user_data_dir:/tmp/profile".to_string()),
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (_reader, mut writer) = stream.into_split();
            let response = IpcResponse::success(
                "handshake",
                serde_json::json!({
                    "daemon_session_id": "sess-default",
                    "attachment_identity": "user_data_dir:/tmp/other",
                    "launch_policy": serde_json::to_value(LaunchPolicyInfo {
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
                    }).unwrap(),
                }),
            )
            .with_command_id(rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID)
            .expect("probe command_id must be valid")
            .with_daemon_session_id("sess-default")
            .expect("daemon_session_id must be valid");
            let _ = NdJsonCodec::write(&mut writer, &response).await;
        });

        let error = resolve_existing_close_target_by_attachment_identity_until(
            &home,
            "user_data_dir:/tmp/profile",
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            1_000,
        )
        .await
        .expect_err("stale selector projection must fail closed")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.as_ref().and_then(|ctx| ctx.get("reason")),
            Some(&serde_json::json!("close_selector_authority_unavailable"))
        );

        server.await.unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn resolve_close_target_by_attachment_identity_until_respects_callers_shared_deadline() {
        let home = temp_home();
        std::fs::create_dir_all(&home).unwrap();
        let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(
            runtime
                .startup_committed_path()
                .parent()
                .expect("startup commit parent"),
        )
        .unwrap();
        std::fs::create_dir_all(
            runtime
                .socket_path()
                .parent()
                .expect("socket path parent should exist"),
        )
        .unwrap();
        let _listener = UnixListener::bind(runtime.socket_path()).unwrap();
        std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(runtime.startup_committed_path(), "sess-default").unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![RegistryEntry {
                    session_id: "sess-default".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: runtime.socket_path().display().to_string(),
                    created_at: "2026-04-18T00:00:01Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: Some("/tmp/profile".to_string()),
                    attachment_identity: Some("user_data_dir:/tmp/profile".to_string()),
                    connection_target: None,
                }],
            },
        )
        .unwrap();

        let started = std::time::Instant::now();
        let error = resolve_existing_close_target_by_attachment_identity_until(
            &home,
            "user_data_dir:/tmp/profile",
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(1))
                .unwrap(),
            1_000,
        )
        .await
        .expect_err("shared selector-close deadline exhaustion must fail closed")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::IpcTimeout);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("phase"))
                .and_then(|value| value.as_str()),
            Some("close_selector_resolution")
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "close target resolution must respect the caller-owned deadline instead of restarting timeout"
        );

        let _ = std::fs::remove_dir_all(home);
    }
}
