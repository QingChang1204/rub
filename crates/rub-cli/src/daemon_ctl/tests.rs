#[cfg(unix)]
use super::{FORCE_SETSID_FAILURE, detach_daemon_session};
use super::{
    ShutdownFenceStatus, StartupSignalFiles, acquire_startup_lock, classify_close_all_result,
    close_all_session_targets, close_all_sessions, command_matches_daemon_identity,
    fetch_handshake_info_with_timeout, ipc_timeout_error, project_batch_close_result,
    project_request_onto_deadline, read_startup_error, registry_authority_snapshot,
    replay_retry_matches_daemon_authority, socket_candidates_for_session, startup_lock_scope_keys,
    startup_signal_paths, try_lock_exclusive, unlock, wait_for_ready,
};
use crate::timeout_budget::WAIT_IPC_BUFFER_MS;
use rub_core::error::ErrorCode;
use rub_core::model::LaunchPolicyInfo;
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{RegistryData, RegistryEntry, write_registry};
use rub_ipc::client::IpcClient;
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
    os::unix::fs::symlink,
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
    std::fs::create_dir_all(stale_socket.parent().unwrap()).unwrap();
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
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
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
async fn detect_or_connect_hardened_cleans_stale_dead_pid_and_requests_restart() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-stale");
    let projection = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();

    std::fs::write(runtime.pid_path(), "999999").unwrap();
    std::fs::write(projection.canonical_pid_path(), "999999").unwrap();
    std::fs::create_dir_all(runtime.socket_path().parent().unwrap()).unwrap();
    std::fs::write(runtime.socket_path(), b"stale").unwrap();
    let _ = std::fs::remove_file(projection.canonical_socket_path());
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    std::fs::write(runtime.startup_committed_path(), "sess-stale").unwrap();
    std::fs::write(projection.startup_committed_path(), "sess-stale").unwrap();

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: "sess-stale".to_string(),
                session_name: "default".to_string(),
                pid: 999_999,
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-09T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let resolution = super::detect_or_connect_hardened(
        &home,
        "default",
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    .expect("stale authority should resolve to restart");
    assert!(
        matches!(resolution, super::DaemonConnection::NeedStart),
        "stale dead authority must fail closed to restart"
    );

    assert!(
        !runtime.pid_path().exists(),
        "stale runtime pid file must be cleaned"
    );
    assert!(
        !projection.canonical_pid_path().exists(),
        "stale projection pid file must be cleaned"
    );
    assert!(
        !runtime.socket_path().exists(),
        "stale runtime socket must be cleaned"
    );
    assert!(
        !projection.canonical_socket_path().exists(),
        "stale projection socket must be cleaned"
    );
    assert!(
        !projection.startup_committed_path().exists(),
        "stale startup commit marker must be cleaned"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn detect_or_connect_hardened_reconnects_after_successful_upgrade_probe() {
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
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
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
                created_at: "2026-04-09T00:00:00Z".to_string(),
                ipc_protocol_version: "0.9".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let server = tokio::spawn(async move {
        for (index, expected_command) in ["_handshake", "_upgrade_check", "_handshake"]
            .into_iter()
            .enumerate()
        {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert_eq!(request.command, expected_command);
            let response = IpcResponse::success(
                format!("response-{expected_command}"),
                serde_json::json!({
                    "daemon_session_id": "sess-default",
                    "launch_policy": {
                        "headless": true,
                        "ignore_cert_errors": false,
                        "hide_infobars": false
                    }
                }),
            );
            let write_result = NdJsonCodec::write(&mut writer, &response).await;
            if index == 0 {
                let _ = write_result;
            } else {
                write_result.expect("write response");
            }
        }
    });

    let resolution = super::detect_or_connect_hardened(
        &home,
        "default",
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    .expect("upgrade reconnect path should succeed");
    let super::DaemonConnection::Connected {
        daemon_session_id, ..
    } = resolution
    else {
        panic!("successful upgrade probe should reconnect to a live daemon");
    };
    assert_eq!(daemon_session_id.as_deref(), Some("sess-default"));

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
    std::fs::create_dir_all(pending_runtime.socket_path().parent().unwrap()).unwrap();
    std::fs::write(pending_runtime.socket_path(), b"pending").unwrap();
    std::fs::create_dir_all(live_runtime.socket_path().parent().unwrap()).unwrap();
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
    let source = std::io::Error::new(std::io::ErrorKind::TimedOut, "replay budget exhausted");
    let error = ipc_timeout_error(
        &source,
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

#[tokio::test]
async fn fetch_handshake_info_preserves_version_mismatch_code_and_context() {
    let socket_dir = std::path::PathBuf::from(format!("/tmp/rdi-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket_path = socket_dir.join("ipc.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let _: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let mut response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": "sess-default",
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
        );
        response.ipc_protocol_version = "0.9".to_string();
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let error = fetch_handshake_info_with_timeout(&mut client, 1_000)
        .await
        .expect_err("mismatched protocol should fail");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcVersionMismatch);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("ipc_response_protocol_version_mismatch")
    );

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
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
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::DaemonStartFailed);
    let context = envelope.context.expect("startup lock timeout context");
    assert_eq!(context["reason"], "startup_lock_timeout");
    assert_eq!(
        context["lock_path_state"]["path_authority"],
        "daemon_ctl.startup.lock_path"
    );
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
fn read_startup_error_missing_file_preserves_error_file_state() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let error_path = home.join("missing.error");

    let envelope = read_startup_error(&error_path)
        .expect_err("missing startup error file should fail")
        .into_envelope();
    let context = envelope.context.expect("startup error file context");
    assert_eq!(context["reason"], "startup_error_file_read_failed");
    assert_eq!(
        context["error_file_state"]["path_authority"],
        "daemon_ctl.startup.error_file"
    );

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
    std::fs::create_dir_all(session_paths.canonical_socket_path().parent().unwrap()).unwrap();
    std::fs::write(session_paths.canonical_socket_path(), b"").unwrap();
    std::fs::create_dir_all(session_paths.socket_path().parent().unwrap()).unwrap();
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
fn startup_ready_retry_timeout_failure_preserves_socket_path_state() {
    let failure = super::startup_ready_retry_timeout_failure(
        super::RetryAttribution::default(),
        Path::new("/tmp/rub.sock"),
    );
    let context = failure
        .error
        .into_envelope()
        .context
        .expect("timeout context");
    assert_eq!(context["reason"], "startup_handshake_timeout");
    assert_eq!(
        context["socket_path_state"]["path_authority"],
        "daemon_ctl.startup.handshake.socket_path"
    );
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
fn registry_authority_snapshot_failure_preserves_rub_home_state() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::write(&home, b"not-a-directory").unwrap();

    let envelope = registry_authority_snapshot(&home)
        .expect_err("invalid rub_home should fail")
        .into_envelope();
    let context = envelope.context.expect("registry authority context");
    assert_eq!(context["reason"], "registry_authority_resolution_failed");
    assert_eq!(
        context["rub_home_state"]["path_authority"],
        "daemon_ctl.registry_authority.rub_home"
    );

    let _ = std::fs::remove_file(home);
}

#[test]
fn daemon_ctl_socket_error_preserves_socket_path_state_and_reason() {
    let error = super::daemon_ctl_socket_error(
        ErrorCode::DaemonStartFailed,
        "boom".to_string(),
        Path::new("/tmp/rub.sock"),
        "daemon_ctl.startup.handshake.socket_path",
        "startup_ready_monitor.socket_path",
        "startup_handshake_bind_failed",
    )
    .into_envelope();
    let context = error.context.expect("daemon_ctl socket error context");
    assert_eq!(context["reason"], "startup_handshake_bind_failed");
    assert_eq!(
        context["socket_path_state"]["path_authority"],
        "daemon_ctl.startup.handshake.socket_path"
    );
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

#[test]
fn project_batch_close_result_marks_subject_rub_home_as_local_runtime_reference() {
    let projected = project_batch_close_result(
        Path::new("/tmp/rub-home"),
        &super::BatchCloseResult {
            closed: vec!["default".to_string()],
            cleaned_stale: vec!["work".to_string()],
            failed: vec!["broken".to_string()],
        },
    );

    assert_eq!(
        projected["subject"]["rub_home_state"]["path_authority"],
        "cli.close_all.subject.rub_home"
    );
    assert_eq!(
        projected["subject"]["rub_home_state"]["truth_level"],
        "local_runtime_reference"
    );
}

#[test]
fn daemon_ctl_path_state_marks_display_only_local_runtime_reference() {
    let state = super::daemon_ctl_path_state(
        "daemon_ctl.upgrade.registry_entry.socket_path",
        "registry_authority_entry",
        "session_socket",
    );

    assert_eq!(state.truth_level, "local_runtime_reference");
    assert_eq!(
        state.path_authority,
        "daemon_ctl.upgrade.registry_entry.socket_path"
    );
    assert_eq!(state.upstream_truth, "registry_authority_entry");
    assert_eq!(state.path_kind, "session_socket");
    assert_eq!(state.control_role, "display_only");
}

#[test]
fn daemon_ctl_path_error_preserves_path_state_and_reason() {
    let error = super::daemon_ctl_path_error(
        ErrorCode::DaemonStartFailed,
        "boom".to_string(),
        super::DaemonCtlPathContext {
            path_key: "socket_path",
            path: Path::new("/tmp/rub.sock"),
            path_authority: "daemon_ctl.connect.socket_path",
            upstream_truth: "session_socket_candidates",
            path_kind: "session_socket",
            reason: "daemon_socket_connect_failed",
        },
    )
    .into_envelope();
    let context = error.context.expect("daemon_ctl path error context");
    assert_eq!(context["reason"], "daemon_socket_connect_failed");
    assert_eq!(
        context["socket_path_state"]["path_authority"],
        "daemon_ctl.connect.socket_path"
    );
    assert_eq!(
        context["socket_path_state"]["upstream_truth"],
        "session_socket_candidates"
    );
}

#[tokio::test]
async fn daemon_authority_mismatch_preserves_socket_path_state() {
    let entry = RegistryEntry {
        session_id: "sess-registry".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: "/tmp/rub-home/default.sock".to_string(),
        created_at: "2026-04-08T00:00:00Z".to_string(),
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let handshake = super::HandshakePayload {
        daemon_session_id: "sess-handshake".to_string(),
        launch_policy: LaunchPolicyInfo {
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
        },
    };

    let error = super::maybe_upgrade_if_needed(
        Path::new("/tmp/rub-home"),
        "default",
        Some(&entry),
        &handshake,
        Path::new("/tmp/rub-home/default.sock"),
    )
    .await
    .err()
    .expect("mismatched authority must fail")
    .into_envelope();

    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    let context = error.context.expect("context");
    assert_eq!(context["reason"], "daemon_authority_mismatch");
    assert_eq!(
        context["socket_path_state"]["path_authority"],
        "daemon_ctl.upgrade.registry_entry.socket_path"
    );
}
