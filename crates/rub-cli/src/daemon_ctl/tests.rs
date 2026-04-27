use super::{
    AuthorityBoundConnectSpec, CompatibilityDegradedOwnedReason, ShutdownFenceStatus,
    StartupCleanupAuthorityKind, StartupCleanupProof, StartupSignalFiles, acquire_startup_lock,
    apply_hard_cut_shutdown_outcome, authority_bound_connected_client, classify_close_all_result,
    cleanup_startup_fallback_browser_authority_for_test, close_all_session_targets,
    close_all_sessions, command_matches_daemon_identity, connect_ipc_with_retry_until,
    current_socket_path_identity, detect_or_connect_hardened_until, fetch_handshake_info_until,
    fetch_handshake_info_with_timeout, hard_cut_outdated_daemon_until_for_test, ipc_timeout_error,
    project_batch_close_result, project_request_onto_deadline, read_startup_cleanup_proof,
    read_startup_error, registry_authority_snapshot, replay_retry_matches_daemon_authority,
    requires_immediate_batch_shutdown_after_external_close,
    should_escalate_close_all_to_kill_fallback, socket_candidates_for_session,
    startup_cleanup_signal_path, startup_lock_scope_keys, startup_signal_paths, try_lock_exclusive,
    unlock, upgrade_startup_lock_to_canonical_attachment_until, wait_for_ready,
    wait_for_ready_until, write_startup_cleanup_proof_at,
};
#[cfg(unix)]
use super::{FORCE_SETSID_FAILURE, detach_daemon_session};
use crate::timeout_budget::WAIT_IPC_BUFFER_MS;
use rub_core::error::ErrorCode;
use rub_core::model::LaunchPolicyInfo;
use rub_daemon::rub_paths::RubPaths;
use rub_daemon::session::{
    HardCutReleasePendingProof, RegistryData, RegistryEntry, write_hard_cut_release_pending_proof,
    write_registry,
};
use rub_ipc::client::IpcClient;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID;
use rub_ipc::protocol::{IpcRequest, IpcResponse, ResponseStatus, UPGRADE_CHECK_PROBE_COMMAND_ID};
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
    os::unix::fs::{PermissionsExt, symlink},
    os::unix::net::UnixListener as StdUnixListener,
    process::Command,
};

fn temp_home() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rub-daemon-ctl-test-{}", Uuid::now_v7()))
}

#[cfg(unix)]
fn ensure_unix_socket_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create unix socket parent");
    }
}

#[cfg(unix)]
fn bind_std_unix_listener(path: &Path) -> StdUnixListener {
    ensure_unix_socket_parent(path);
    StdUnixListener::bind(path).expect("bind unix listener")
}

#[cfg(unix)]
fn bind_tokio_unix_listener(path: &Path) -> UnixListener {
    ensure_unix_socket_parent(path);
    UnixListener::bind(path).expect("bind unix listener")
}

#[cfg(unix)]
fn spawn_synthetic_chrome_profile_holder(home: &Path, profile_dir: &Path) -> std::process::Child {
    let bin_dir = home.join("fake-browser-bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake browser bin dir");
    let browser_path = bin_dir.join("Google Chrome");
    std::fs::write(&browser_path, b"#!/bin/sh\nsleep 30\n").expect("write fake browser");
    let mut permissions = std::fs::metadata(&browser_path)
        .expect("fake browser metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&browser_path, permissions).expect("chmod fake browser");

    Command::new(&browser_path)
        .arg("--user-data-dir")
        .arg(profile_dir)
        .spawn()
        .expect("spawn synthetic Chrome profile holder")
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

#[test]
fn project_request_onto_deadline_rejects_zero_embedded_wait_budget_before_ipc_send() {
    let request = IpcRequest::new(
        "wait",
        serde_json::json!({
            "selector": "#ready",
            "timeout_ms": 5_000,
        }),
        5_000 + WAIT_IPC_BUFFER_MS,
    );
    let deadline = Instant::now() + Duration::from_millis(WAIT_IPC_BUFFER_MS.saturating_sub(1));

    assert!(
        project_request_onto_deadline(&request, deadline).is_none(),
        "CLI must fail closed instead of projecting a zero-ms embedded wait onto the daemon"
    );
}

#[test]
fn project_request_onto_deadline_shrinks_inspect_list_wait_budget_together() {
    let request = IpcRequest::new(
        "inspect",
        serde_json::json!({
            "sub": "list",
            "collection": ".mail-row",
            "wait_field": "subject",
            "wait_contains": "Confirm",
            "wait_timeout_ms": 12_500,
        }),
        12_500 + WAIT_IPC_BUFFER_MS,
    );
    let deadline = Instant::now() + Duration::from_millis(2_000 + WAIT_IPC_BUFFER_MS);

    let projected =
        project_request_onto_deadline(&request, deadline).expect("deadline should remain");
    let embedded_timeout_ms = projected
        .args
        .get("wait_timeout_ms")
        .and_then(|value| value.as_u64())
        .expect("inspect list wait payload should keep embedded timeout");

    assert!(projected.timeout_ms <= 2_000 + WAIT_IPC_BUFFER_MS);
    assert_eq!(
        embedded_timeout_ms,
        projected.timeout_ms.saturating_sub(WAIT_IPC_BUFFER_MS)
    );
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
#[cfg(unix)]
async fn close_all_does_not_escalate_when_hardened_attach_cannot_prove_authority() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-live-missing-socket";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit marker parent"),
    )
    .unwrap();
    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(home.display().to_string())
        .spawn()
        .expect("spawn daemon-shaped process");
    std::fs::write(runtime.pid_path(), child.id().to_string()).unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: child.id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-21T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let result = close_all_sessions(&home, 250).await.unwrap();
    assert!(result.closed.is_empty());
    assert!(result.cleaned_stale.is_empty());
    assert_eq!(result.failed, vec![session_name.to_string()]);

    let registry = rub_daemon::session::read_registry(&home).unwrap();
    assert_eq!(registry.sessions.len(), 1);
    assert_eq!(registry.sessions[0].session_id, session_id);

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
#[cfg(unix)]
async fn close_all_preserves_authoritative_entry_when_uncertain_siblings_remain_unproven() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let live_runtime = RubPaths::new(&home).session_runtime("default", "sess-live");
    let uncertain_runtime = RubPaths::new(&home).session_runtime("default", "sess-uncertain");
    std::fs::create_dir_all(live_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(
        live_runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit marker parent"),
    )
    .unwrap();
    std::fs::write(live_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(live_runtime.startup_committed_path(), "sess-live").unwrap();
    std::fs::create_dir_all(live_runtime.socket_path().parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(live_runtime.socket_path());
    let listener = bind_std_unix_listener(&live_runtime.socket_path());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept snapshot handshake");
        let mut reader = StdBufReader::new(
            stream
                .try_clone()
                .expect("clone accepted stream for blocking handshake read"),
        );
        let request: IpcRequest = NdJsonCodec::read_blocking(&mut reader)
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "_handshake");
        assert_eq!(
            request.command_id.as_deref(),
            Some(HANDSHAKE_PROBE_COMMAND_ID)
        );
        let response = IpcResponse::success(
            request.command.clone(),
            serde_json::json!({
                "daemon_session_id": "sess-live",
            }),
        )
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .unwrap()
        .with_daemon_session_id("sess-live")
        .unwrap();
        let encoded = NdJsonCodec::encode(&response).unwrap();
        stream.write_all(&encoded).expect("write response");

        let (stream, _) = listener.accept().expect("accept attach attempt");
        let mut reader = StdBufReader::new(stream);
        let request: IpcRequest = NdJsonCodec::read_blocking(&mut reader)
            .expect("read attach request")
            .expect("attach request");
        assert_eq!(request.command, "_handshake");
    });

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: "sess-uncertain".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: uncertain_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-21T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "sess-live".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: live_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-21T00:00:01Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        },
    )
    .unwrap();

    let result = close_all_sessions(&home, 250).await.unwrap();
    assert!(result.closed.is_empty());
    assert!(result.cleaned_stale.is_empty());
    assert_eq!(result.failed, vec!["default".to_string()]);

    let registry = rub_daemon::session::read_registry(&home).unwrap();
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-live"),
        "authoritative entry must remain until its shutdown fence is actually proven"
    );
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-uncertain"),
        "uncertain sibling must remain visible after fail-closed batch close"
    );

    server.join().unwrap();
    let _ = std::fs::remove_file(live_runtime.socket_path());
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn close_existing_session_noops_without_creating_rub_home() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let outcome = super::close_existing_session_until(
        &home,
        "default",
        Instant::now() + Duration::from_secs(1),
        1_000,
    )
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
    let listener = bind_tokio_unix_listener(&socket_path);
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
                    )
                    .with_command_id(
                        request
                            .command_id
                            .clone()
                            .expect("handshake probe must carry command_id"),
                    )
                    .expect("probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid");
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
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid")
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

    let outcome = super::close_existing_session_until(
        &home,
        "default",
        Instant::now() + Duration::from_secs(5),
        5_000,
    )
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
#[cfg(unix)]
async fn detect_or_connect_hardened_cleans_the_proven_stale_authority_not_newer_uncertain_sibling()
{
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let old_session_id = "sess-live-old";
    let new_session_id = "sess-uncertain-new";
    let old_runtime = RubPaths::new(&home).session_runtime(session_name, old_session_id);
    let new_runtime = RubPaths::new(&home).session_runtime(session_name, new_session_id);
    let projection = RubPaths::new(&home).session(session_name);
    std::fs::create_dir_all(old_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(new_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(
        old_runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit marker parent"),
    )
    .unwrap();

    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(old_session_id)
        .arg("--rub-home")
        .arg(home.display().to_string())
        .spawn()
        .expect("spawn daemon-shaped process");

    std::fs::write(old_runtime.pid_path(), child.id().to_string()).unwrap();
    std::fs::write(old_runtime.startup_committed_path(), old_session_id).unwrap();
    std::fs::create_dir_all(old_runtime.socket_path().parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(old_runtime.socket_path());
    let listener = bind_std_unix_listener(&old_runtime.socket_path());
    let child_pid = child.id();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept snapshot handshake");
        let mut reader = StdBufReader::new(
            stream
                .try_clone()
                .expect("clone accepted stream for blocking handshake read"),
        );
        let request: IpcRequest = NdJsonCodec::read_blocking(&mut reader)
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "_handshake");
        let response = IpcResponse::success(
            request.command.clone(),
            serde_json::json!({
                "daemon_session_id": old_session_id,
            }),
        )
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .unwrap()
        .with_daemon_session_id(old_session_id)
        .unwrap();
        let encoded = NdJsonCodec::encode(&response).unwrap();
        stream.write_all(&encoded).expect("write response");
        let _ = unsafe { libc::kill(child_pid as i32, libc::SIGTERM) };
    });

    std::fs::write(new_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(
        projection.canonical_pid_path(),
        std::process::id().to_string(),
    )
    .unwrap();
    std::fs::write(projection.startup_committed_path(), old_session_id).unwrap();

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: old_session_id.to_string(),
                    session_name: session_name.to_string(),
                    pid: child.id(),
                    socket_path: old_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-21T00:00:00Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: new_session_id.to_string(),
                    session_name: session_name.to_string(),
                    pid: std::process::id(),
                    socket_path: new_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-21T00:00:01Z".to_string(),
                    ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        },
    )
    .unwrap();

    let resolution = super::detect_or_connect_hardened(
        &home,
        session_name,
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    .expect("stale dead authority should resolve to restart");
    assert!(matches!(resolution, super::DaemonConnection::NeedStart));

    server.join().unwrap();
    let _ = child.wait();

    assert!(
        !old_runtime.pid_path().exists(),
        "cleanup must target the proven stale authority entry"
    );
    assert!(
        new_runtime.pid_path().exists(),
        "cleanup must not remove the newer uncertain sibling runtime state"
    );

    let _ = std::fs::remove_file(old_runtime.socket_path());
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
#[cfg(unix)]
async fn detect_or_connect_hardened_does_not_clean_authority_on_unproven_transport_flap() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-live-flap";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let projection = RubPaths::new(&home).session(session_name);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit marker parent"),
    )
    .unwrap();

    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(home.display().to_string())
        .spawn()
        .expect("spawn daemon-shaped process");

    std::fs::write(runtime.pid_path(), child.id().to_string()).unwrap();
    std::fs::write(projection.canonical_pid_path(), child.id().to_string()).unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();
    std::fs::write(projection.startup_committed_path(), session_id).unwrap();
    std::fs::create_dir_all(runtime.socket_path().parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(runtime.socket_path());
    let listener = bind_std_unix_listener(&runtime.socket_path());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept snapshot handshake");
        let mut reader = StdBufReader::new(
            stream
                .try_clone()
                .expect("clone accepted stream for blocking handshake read"),
        );
        let request: IpcRequest = NdJsonCodec::read_blocking(&mut reader)
            .expect("read request")
            .expect("request");
        assert_eq!(request.command, "_handshake");
        let response = IpcResponse::success(
            request.command.clone(),
            serde_json::json!({
                "daemon_session_id": session_id,
            }),
        )
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .unwrap()
        .with_daemon_session_id(session_id)
        .unwrap();
        let encoded = NdJsonCodec::encode(&response).unwrap();
        stream.write_all(&encoded).expect("write response");

        let (_stream, _) = listener.accept().expect("accept attach handshake");
        // Drop immediately to force a transport flap without proving the authority stale.
    });

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: child.id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-21T00:00:00Z".to_string(),
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
        session_name,
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    .expect("transport flap before lock should still fall back to restart");
    assert!(matches!(resolution, super::DaemonConnection::NeedStart));

    server.join().unwrap();
    assert!(
        runtime.pid_path().exists(),
        "transport flap must not scrub the authoritative runtime pid without a stale proof"
    );
    assert!(
        projection.canonical_pid_path().exists(),
        "transport flap must not scrub the authoritative projection pid without a stale proof"
    );
    assert!(
        projection.startup_committed_path().exists(),
        "transport flap must not clear the startup commit marker without a stale proof"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(runtime.socket_path());
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
#[cfg(unix)]
async fn detect_or_connect_hardened_preserves_committed_registry_authority_when_socket_is_missing()
{
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-live-missing-socket";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit marker parent"),
    )
    .unwrap();
    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(home.display().to_string())
        .spawn()
        .expect("spawn daemon-shaped process");
    std::fs::write(runtime.pid_path(), child.id().to_string()).unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: child.id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-21T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();
    let entry = rub_daemon::session::read_registry(&home)
        .unwrap()
        .sessions
        .into_iter()
        .next()
        .expect("registry entry");
    assert!(rub_core::process::is_process_alive(child.id()));
    assert_eq!(
        std::fs::read_to_string(runtime.pid_path()).unwrap().trim(),
        child.id().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(runtime.startup_committed_path())
            .unwrap()
            .trim(),
        session_id
    );
    let runtime_socket_matches = std::path::Path::new(&entry.socket_path) == runtime.socket_path();
    assert!(
        rub_core::process::is_process_alive(entry.pid)
            && runtime_socket_matches
            && std::fs::read_to_string(runtime.pid_path())
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                == Some(entry.pid)
            && std::fs::read_to_string(runtime.startup_committed_path())
                .ok()
                .is_some_and(|raw| raw.trim() == session_id),
        "fixture must satisfy committed runtime authority proof before hardened attach runs"
    );

    let error = match super::detect_or_connect_hardened(
        &home,
        session_name,
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    {
        Ok(_) => {
            panic!(
                "committed registry authority with missing socket must not collapse to NeedStart"
            )
        }
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::SessionBusy);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(|value| value.as_str()),
        Some("daemon_registry_authority_socket_missing")
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
#[cfg(unix)]
async fn detect_or_connect_hardened_preserves_hard_cut_release_pending_authority_across_retry() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-hard-cut";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let projection = RubPaths::new(&home).session(session_name);
    let profile_dir = rub_cdp::projected_managed_profile_path_for_session(session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(runtime.pid_path(), "999999").unwrap();
    std::fs::write(projection.canonical_pid_path(), "999999").unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();
    std::fs::write(projection.startup_committed_path(), session_id).unwrap();
    write_hard_cut_release_pending_proof(
        &home,
        session_name,
        &HardCutReleasePendingProof {
            session_id: session_id.to_string(),
        },
    )
    .unwrap();

    let mut child = spawn_synthetic_chrome_profile_holder(&home, &profile_dir);

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: 999_999,
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-19T00:00:00Z".to_string(),
                ipc_protocol_version: "0.9".to_string(),
                user_data_dir: Some(profile_dir.display().to_string()),
                attachment_identity: Some(format!("user_data_dir:{}", profile_dir.display())),
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let error = match super::detect_or_connect_hardened(
        &home,
        session_name,
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    {
        Ok(_) => panic!("hard-cut release-pending authority must fail closed across retry"),
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcVersionMismatch);
    let context = envelope.context.expect("hard-cut release pending context");
    assert_eq!(context["reason"], "hard_cut_upgrade_fence_incomplete");
    assert_eq!(context["shutdown_fence"]["daemon_stopped"], true);
    assert_eq!(context["shutdown_fence"]["profile_released"], false);

    assert!(
        runtime.pid_path().exists(),
        "retry path must not clean runtime pid while release-pending fallback authority holds"
    );
    assert!(
        projection.canonical_pid_path().exists(),
        "retry path must not clean projection pid while release-pending fallback authority holds"
    );
    assert!(
        projection.startup_committed_path().exists(),
        "retry path must not clean startup commit marker while release-pending fallback authority holds"
    );
    assert!(
        projection.hard_cut_release_pending_path().exists(),
        "retry path must preserve the explicit hard-cut fallback authority proof"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(profile_dir);
}

#[tokio::test]
#[cfg(unix)]
async fn detect_or_connect_hardened_preserves_malformed_hard_cut_release_pending_authority_across_retry()
 {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-hard-cut-malformed";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let projection = RubPaths::new(&home).session(session_name);
    let profile_dir = rub_cdp::projected_managed_profile_path_for_session(session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(runtime.pid_path(), "999999").unwrap();
    std::fs::write(projection.canonical_pid_path(), "999999").unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();
    std::fs::write(projection.startup_committed_path(), session_id).unwrap();
    std::fs::write(projection.hard_cut_release_pending_path(), b"{bad-proof").unwrap();

    let mut child = spawn_synthetic_chrome_profile_holder(&home, &profile_dir);

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: 999_999,
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-21T00:00:00Z".to_string(),
                ipc_protocol_version: "0.9".to_string(),
                user_data_dir: Some(profile_dir.display().to_string()),
                attachment_identity: Some(format!("user_data_dir:{}", profile_dir.display())),
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let error = match super::detect_or_connect_hardened(
        &home,
        session_name,
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    {
        Ok(_) => panic!("malformed hard-cut fallback proof must still fail closed across retry"),
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcVersionMismatch);
    let context = envelope.context.expect("hard-cut release pending context");
    assert_eq!(context["reason"], "hard_cut_upgrade_fence_incomplete");
    assert_eq!(context["shutdown_fence"]["daemon_stopped"], true);
    assert_eq!(context["shutdown_fence"]["profile_released"], false);

    assert!(
        projection.hard_cut_release_pending_path().exists(),
        "retry path must preserve malformed hard-cut fallback proof instead of cleaning authority"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(profile_dir);
}

#[tokio::test]
#[cfg(unix)]
async fn detect_or_connect_hardened_present_socket_transport_transient_still_fails_closed_when_hard_cut_release_pending_authority_holds()
 {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-hard-cut-present-socket";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let projection = RubPaths::new(&home).session(session_name);
    let profile_dir = rub_cdp::projected_managed_profile_path_for_session(session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::create_dir_all(runtime.socket_path().parent().unwrap()).unwrap();
    write_hard_cut_release_pending_proof(
        &home,
        session_name,
        &HardCutReleasePendingProof {
            session_id: session_id.to_string(),
        },
    )
    .unwrap();

    let mut child = spawn_synthetic_chrome_profile_holder(&home, &profile_dir);
    std::fs::write(runtime.pid_path(), child.id().to_string()).unwrap();
    std::fs::write(projection.canonical_pid_path(), child.id().to_string()).unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();
    std::fs::write(projection.startup_committed_path(), session_id).unwrap();

    let listener = bind_tokio_unix_listener(&runtime.socket_path());
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: child.id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-21T00:00:00Z".to_string(),
                ipc_protocol_version: "0.9".to_string(),
                user_data_dir: Some(profile_dir.display().to_string()),
                attachment_identity: Some(format!("user_data_dir:{}", profile_dir.display())),
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let handshake_profile_dir = profile_dir.clone();
    let server = tokio::spawn(async move {
        let mut handshake_attempts = 0;
        loop {
            let (stream, _) = listener.accept().await.expect("accept control-plane probe");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read control-plane request")
                .expect("control-plane request");
            match request.command.as_str() {
                "_handshake" => {
                    handshake_attempts += 1;
                    let mut response = IpcResponse::success(
                        "response-_handshake",
                        serde_json::json!({
                            "daemon_session_id": session_id,
                            "attachment_identity": format!(
                                "user_data_dir:{}",
                                handshake_profile_dir.display()
                            ),
                            "launch_policy": {
                                "headless": true,
                                "ignore_cert_errors": false,
                                "hide_infobars": false
                            }
                        }),
                    )
                    .with_command_id(
                        request
                            .command_id
                            .clone()
                            .expect("handshake probe must carry command_id"),
                    )
                    .expect("probe command_id must be valid")
                    .with_daemon_session_id(session_id)
                    .expect("daemon_session_id must be valid");
                    response.ipc_protocol_version = "0.9".to_string();
                    let _ = NdJsonCodec::write(&mut writer, &response).await;
                }
                "_upgrade_check" => {
                    assert!(
                        handshake_attempts > 0,
                        "upgrade check must not run before any successful handshake probe"
                    );
                    assert_eq!(
                        request.command_id.as_deref(),
                        Some(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    );
                    assert_eq!(request.daemon_session_id.as_deref(), Some(session_id));
                    break;
                }
                other => panic!("unexpected control-plane command {other}"),
            }
        }
    });

    let error = match super::detect_or_connect_hardened(
        &home,
        session_name,
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    {
        Ok(_) => {
            panic!(
                "present-socket transport transient must still fail closed while hard-cut fallback authority holds"
            )
        }
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcVersionMismatch);
    let context = envelope.context.expect("hard-cut release pending context");
    assert_eq!(context["reason"], "hard_cut_upgrade_fence_incomplete");
    assert_eq!(context["shutdown_fence"]["daemon_stopped"], false);
    assert_eq!(context["shutdown_fence"]["profile_released"], false);

    assert!(
        projection.hard_cut_release_pending_path().exists(),
        "present-socket transient attach path must preserve hard-cut fallback authority"
    );
    assert!(
        runtime.socket_path().exists(),
        "present-socket transient path must not clean the selected authority socket"
    );

    server.await.expect("server task");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(profile_dir);
}

#[tokio::test]
async fn detect_or_connect_hardened_proves_handshake_before_successful_upgrade_probe() {
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
    let listener = bind_tokio_unix_listener(&socket_path);
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
        let mut saw_upgrade_check = false;
        let mut handshake_attempts = 0;
        while !saw_upgrade_check {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            let response = match request.command.as_str() {
                "_handshake" => {
                    handshake_attempts += 1;
                    let mut response = IpcResponse::success(
                        "response-_handshake",
                        serde_json::json!({
                            "daemon_session_id": "sess-default",
                            "launch_policy": {
                                "headless": true,
                                "ignore_cert_errors": false,
                                "hide_infobars": false
                            }
                        }),
                    )
                    .with_command_id(
                        request
                            .command_id
                            .clone()
                            .expect("handshake probe must carry command_id"),
                    )
                    .expect("probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid");
                    response.ipc_protocol_version = "0.9".to_string();
                    response
                }
                "_upgrade_check" => {
                    saw_upgrade_check = true;
                    assert!(
                        handshake_attempts > 0,
                        "upgrade check must not run before any successful handshake probe"
                    );
                    assert_eq!(
                        request.command_id.as_deref(),
                        Some(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    );
                    assert_eq!(request.daemon_session_id.as_deref(), Some("sess-default"));
                    IpcResponse::success(
                        "response-_upgrade_check",
                        serde_json::json!({
                            "idle": true,
                            "semantic_command_protocol": {
                                "compatible": true,
                                "daemon_protocol_version": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
                                "compatible_cli_protocol_versions": [
                                    rub_ipc::protocol::IPC_PROTOCOL_VERSION
                                ],
                            },
                        }),
                    )
                    .with_command_id(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    .expect("upgrade-check probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid")
                }
                other => panic!("unexpected control-plane command {other}"),
            };
            let write_result = NdJsonCodec::write(&mut writer, &response).await;
            if !saw_upgrade_check {
                let _ = write_result;
            } else {
                write_result.expect("write response");
            }
        }
        let (_stream, _) = listener.accept().await.expect("bound authority connect");
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
async fn detect_or_connect_hardened_rejects_malformed_upgrade_check_payload() {
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
    let listener = bind_tokio_unix_listener(&socket_path);
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
        let mut saw_upgrade_check = false;
        let mut handshake_attempts = 0;
        while !saw_upgrade_check {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            let response = match request.command.as_str() {
                "_handshake" => {
                    handshake_attempts += 1;
                    let mut response = IpcResponse::success(
                        "response-_handshake",
                        serde_json::json!({
                            "daemon_session_id": "sess-default",
                            "launch_policy": {
                                "headless": true,
                                "ignore_cert_errors": false,
                                "hide_infobars": false
                            }
                        }),
                    )
                    .with_command_id(
                        request
                            .command_id
                            .clone()
                            .expect("handshake probe must carry command_id"),
                    )
                    .expect("probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid");
                    response.ipc_protocol_version = "0.9".to_string();
                    response
                }
                "_upgrade_check" => {
                    saw_upgrade_check = true;
                    assert!(
                        handshake_attempts > 0,
                        "upgrade check must not run before any successful handshake probe"
                    );
                    assert_eq!(
                        request.command_id.as_deref(),
                        Some(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    );
                    assert_eq!(request.daemon_session_id.as_deref(), Some("sess-default"));
                    IpcResponse::success(
                        "response-_upgrade_check",
                        serde_json::json!({
                            "idle": "yes",
                        }),
                    )
                    .with_command_id(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    .expect("upgrade-check probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid")
                }
                other => panic!("unexpected control-plane command {other}"),
            };
            let write_result = NdJsonCodec::write(&mut writer, &response).await;
            if !saw_upgrade_check {
                let _ = write_result;
            } else {
                write_result.expect("write response");
            }
        }
    });

    let error = match super::detect_or_connect_hardened(
        &home,
        "default",
        super::TransientSocketPolicy::NeedStartBeforeLock,
    )
    .await
    {
        Ok(_) => panic!("malformed upgrade_check payload must fail closed"),
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|context| context.get("reason")),
        Some(&serde_json::json!(
            "daemon_ctl_upgrade_check_payload_invalid"
        ))
    );

    server.await.expect("server task");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn detect_or_connect_hardened_until_projects_remaining_budget_onto_handshake_and_upgrade_probe()
 {
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
    let listener = bind_tokio_unix_listener(&socket_path);
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
        let mut saw_upgrade_check = false;
        let mut handshake_attempts = 0;
        while !saw_upgrade_check {
            let (stream, _) = listener.accept().await.expect("accept");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let request: IpcRequest = NdJsonCodec::read(&mut reader)
                .await
                .expect("read request")
                .expect("request");
            assert!(
                request.timeout_ms < 3_000,
                "existing-daemon attach must project the remaining top-level budget instead of using a fixed 3s lane"
            );
            let response = match request.command.as_str() {
                "_handshake" => {
                    handshake_attempts += 1;
                    let mut response = IpcResponse::success(
                        "response-_handshake",
                        serde_json::json!({
                            "daemon_session_id": "sess-default",
                            "launch_policy": {
                                "headless": true,
                                "ignore_cert_errors": false,
                                "hide_infobars": false
                            }
                        }),
                    )
                    .with_command_id(
                        request
                            .command_id
                            .clone()
                            .expect("handshake probe must carry command_id"),
                    )
                    .expect("probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid");
                    response.ipc_protocol_version = "0.9".to_string();
                    response
                }
                "_upgrade_check" => {
                    saw_upgrade_check = true;
                    assert!(
                        handshake_attempts > 0,
                        "upgrade check must not run before any successful handshake probe"
                    );
                    assert_eq!(
                        request.command_id.as_deref(),
                        Some(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    );
                    assert_eq!(request.daemon_session_id.as_deref(), Some("sess-default"));
                    IpcResponse::success(
                        "response-_upgrade_check",
                        serde_json::json!({
                            "idle": true,
                            "semantic_command_protocol": {
                                "compatible": true,
                                "daemon_protocol_version": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
                                "compatible_cli_protocol_versions": [
                                    rub_ipc::protocol::IPC_PROTOCOL_VERSION
                                ],
                            },
                        }),
                    )
                    .with_command_id(UPGRADE_CHECK_PROBE_COMMAND_ID)
                    .expect("upgrade-check probe command_id must be valid")
                    .with_daemon_session_id("sess-default")
                    .expect("daemon_session_id must be valid")
                }
                other => panic!("unexpected control-plane command {other}"),
            };
            let write_result = NdJsonCodec::write(&mut writer, &response).await;
            if !saw_upgrade_check {
                let _ = write_result;
            } else {
                write_result.expect("write response");
            }
        }
        let (_stream, _) = listener.accept().await.expect("bound authority connect");
    });

    let resolution = detect_or_connect_hardened_until(
        &home,
        "default",
        super::TransientSocketPolicy::NeedStartBeforeLock,
        Instant::now() + Duration::from_millis(2_500),
        2_500,
    )
    .await
    .expect("budgeted existing-daemon attach should still succeed");
    let super::DaemonConnection::Connected {
        daemon_session_id, ..
    } = resolution
    else {
        panic!("budgeted existing-daemon attach should resolve to the live daemon");
    };
    assert_eq!(daemon_session_id.as_deref(), Some("sess-default"));

    server.await.expect("server task");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn hard_cut_outdated_daemon_until_bounds_shutdown_wait_to_caller_deadline() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    let socket_path = runtime.socket_path();
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
    std::fs::write(&socket_path, b"stale-socket-authority").unwrap();
    let entry = RegistryEntry {
        session_id: "sess-default".to_string(),
        session_name: "default".to_string(),
        pid: 424_242,
        socket_path: socket_path.display().to_string(),
        created_at: "2026-04-19T00:00:00Z".to_string(),
        ipc_protocol_version: "0.9".to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    let started = Instant::now();
    let error = match hard_cut_outdated_daemon_until_for_test(
        &home,
        "default",
        &entry,
        started + Duration::from_millis(250),
    )
    .await
    {
        Ok(_) => panic!("hard-cut incomplete release must fail closed"),
        Err(error) => error,
    };
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "caller-owned deadline should bound hard-cut shutdown wait instead of spending a fixed 5s lane"
    );
    assert_eq!(
        error
            .into_envelope()
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("hard_cut_upgrade_fence_incomplete")
    );

    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn detect_or_connect_hardened_until_fails_closed_when_attach_budget_is_already_exhausted() {
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
    let _listener = bind_tokio_unix_listener(&socket_path);
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: "sess-default".to_string(),
                session_name: "default".to_string(),
                pid: std::process::id(),
                socket_path: socket_path.display().to_string(),
                created_at: "2026-04-09T00:00:00Z".to_string(),
                ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let error = match detect_or_connect_hardened_until(
        &home,
        "default",
        super::TransientSocketPolicy::NeedStartBeforeLock,
        Instant::now() - Duration::from_millis(1),
        250,
    )
    .await
    {
        Ok(_) => panic!("attach should fail before connect when the top-level budget is exhausted"),
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("phase"))
            .and_then(|value| value.as_str()),
        Some("existing_daemon_connect")
    );

    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn connect_ipc_with_retry_until_respects_shared_attach_budget_on_stale_socket() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(runtime.socket_path().parent().unwrap()).unwrap();
    let stale_listener = bind_std_unix_listener(&runtime.socket_path());
    drop(stale_listener);

    let started = std::time::Instant::now();
    let failure = match connect_ipc_with_retry_until(
        runtime.socket_path().as_path(),
        super::AttachBudget {
            deadline: Instant::now() + Duration::from_millis(75),
            timeout_ms: 75,
        },
        "close_selector_resolution",
        ErrorCode::IpcProtocolError,
        "Failed to connect to existing daemon while resolving close selector authority",
        "daemon_ctl.close_selector.socket_path",
        "registry_authority_entry",
    )
    .await
    {
        Ok(_) => {
            panic!("shared attach budget must fail closed before default retry policy can run long")
        }
        Err(failure) => failure,
    };
    let envelope = failure.into_error().into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("phase"))
            .and_then(|value| value.as_str()),
        Some("close_selector_resolution")
    );
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "deadline-bound attach helper must stay within the shared timeout budget"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn bootstrap_client_refuses_to_start_when_expected_session_authority_is_missing() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();

    let error = match super::bootstrap_client(
        &home,
        "default",
        Some("sess-remembered"),
        Instant::now() + Duration::from_secs(1),
        1_000,
        &[],
        super::StartupAuthorityRequest {
            connection_request: &crate::session_policy::ConnectionRequest::None,
            attachment_identity: None,
        },
    )
    .await
    {
        Ok(_) => panic!("remembered live reuse must not start a replacement daemon"),
        Err(error) => error,
    };
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::DaemonNotRunning);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("existing_session_bootstrap_authority_unavailable")
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn hard_cut_upgrade_keeps_projection_when_profile_release_lags() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    let projection = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(
        runtime
            .startup_committed_path()
            .parent()
            .expect("startup commit marker parent"),
    )
    .unwrap();
    std::fs::write(runtime.pid_path(), "999999").unwrap();
    std::fs::write(projection.canonical_pid_path(), "999999").unwrap();
    std::fs::write(runtime.startup_committed_path(), "sess-default").unwrap();
    std::fs::write(projection.startup_committed_path(), "sess-default").unwrap();

    let entry = RegistryEntry {
        session_id: "sess-default".to_string(),
        session_name: "default".to_string(),
        pid: 999_999,
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-15T00:00:00Z".to_string(),
        ipc_protocol_version: "old".to_string(),
        user_data_dir: Some("/tmp/rub-profile-lag".to_string()),
        attachment_identity: None,
        connection_target: None,
    };

    let error = apply_hard_cut_shutdown_outcome(
        &home,
        "default",
        &entry,
        ShutdownFenceStatus {
            daemon_stopped: true,
            profile_released: false,
        },
    )
    .expect_err("profile lag must keep authority projection intact");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcVersionMismatch);
    let context = envelope.context.expect("shutdown fence context");
    assert_eq!(context["reason"], "hard_cut_upgrade_fence_incomplete");
    assert_eq!(context["shutdown_fence"]["daemon_stopped"], true);
    assert_eq!(context["shutdown_fence"]["profile_released"], false);

    assert!(
        runtime.pid_path().exists(),
        "runtime authority must remain until the shutdown fence is fully released"
    );
    assert!(
        projection.canonical_pid_path().exists(),
        "projection authority must remain until the shutdown fence is fully released"
    );
    assert!(
        projection.startup_committed_path().exists(),
        "startup commit marker must not be cleaned early"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn terminate_registry_entry_process_accepts_protocol_incompatible_owned_daemon() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-default";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(runtime.projection_dir()).unwrap();
    std::fs::write(runtime.pid_path(), "0").unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();
    let socket_path = runtime.socket_path();
    let _ = std::fs::remove_file(&socket_path);

    let listener = bind_std_unix_listener(&socket_path);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept handshake connection");
        let mut request = String::new();
        StdBufReader::new(
            stream
                .try_clone()
                .expect("clone accepted stream for reading"),
        )
        .read_line(&mut request)
        .expect("read handshake request");
        let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        assert_eq!(
            decoded.command_id.as_deref(),
            Some(HANDSHAKE_PROBE_COMMAND_ID)
        );
        let mut response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": session_id,
            }),
        )
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .expect("probe command_id must be valid")
        .with_daemon_session_id(session_id)
        .expect("daemon_session_id must be valid");
        response.ipc_protocol_version = "0.9".to_string();
        serde_json::to_writer(&mut stream, &response).expect("write handshake response");
        stream.write_all(b"\n").expect("terminate handshake frame");
    });

    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(home.as_os_str())
        .spawn()
        .expect("spawn synthetic owned daemon");

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: child.id(),
        socket_path: socket_path.display().to_string(),
        created_at: "2026-04-18T00:00:00Z".to_string(),
        ipc_protocol_version: "0.9".to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    std::fs::write(runtime.pid_path(), child.id().to_string()).unwrap();

    super::terminate_registry_entry_process(&home, &entry)
        .expect("protocol-incompatible owned daemon should remain targetable for hard-cut");
    server.join().expect("handshake server should join");

    let mut exited = false;
    for _ in 0..50 {
        if child.try_wait().expect("query child status").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        panic!("hard-cut target should terminate after SIGTERM");
    }

    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn terminate_registry_entry_process_rejects_protocol_incompatible_foreign_socket_without_runtime_commit()
 {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-default";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    let socket_path = runtime.socket_path();
    let _ = std::fs::remove_file(&socket_path);

    let listener = bind_std_unix_listener(&socket_path);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept handshake connection");
        let mut request = String::new();
        StdBufReader::new(
            stream
                .try_clone()
                .expect("clone accepted stream for reading"),
        )
        .read_line(&mut request)
        .expect("read handshake request");
        let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        assert_eq!(
            decoded.command_id.as_deref(),
            Some(HANDSHAKE_PROBE_COMMAND_ID)
        );
        let mut response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": session_id,
            }),
        )
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .expect("probe command_id must be valid")
        .with_daemon_session_id(session_id)
        .expect("daemon_session_id must be valid");
        response.ipc_protocol_version = "0.9".to_string();
        serde_json::to_writer(&mut stream, &response).expect("write handshake response");
        stream.write_all(b"\n").expect("terminate handshake frame");
    });

    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(home.as_os_str())
        .spawn()
        .expect("spawn synthetic daemon-like process");

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: child.id(),
        socket_path: socket_path.display().to_string(),
        created_at: "2026-04-19T00:00:00Z".to_string(),
        ipc_protocol_version: "0.9".to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    let error = super::terminate_registry_entry_process(&home, &entry).expect_err(
        "foreign socket authority without runtime commit proof must not authorize termination",
    );
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    server.join().expect("handshake server should join");

    assert!(
        child.try_wait().expect("query child status").is_none(),
        "denied termination must leave the child alive"
    );
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn terminate_registry_entry_process_accepts_inconclusive_socket_for_runtime_committed_old_daemon() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-default";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(runtime.projection_dir()).unwrap();

    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(30)")
        .arg("__daemon")
        .arg("--session")
        .arg(session_name)
        .arg("--session-id")
        .arg(session_id)
        .arg("--rub-home")
        .arg(home.as_os_str())
        .spawn()
        .expect("spawn synthetic owned daemon");
    std::fs::write(runtime.pid_path(), child.id().to_string()).unwrap();
    std::fs::write(runtime.startup_committed_path(), session_id).unwrap();

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: child.id(),
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-19T00:00:00Z".to_string(),
        ipc_protocol_version: "0.9".to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    super::terminate_registry_entry_process(&home, &entry)
        .expect("runtime-committed old daemon should remain targetable when socket identity is inconclusive");

    let mut exited = false;
    for _ in 0..50 {
        if child.try_wait().expect("query child status").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        panic!("hard-cut target should terminate after SIGTERM");
    }

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
    let listener = bind_std_unix_listener(&live_runtime.socket_path());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        StdBufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let decoded: rub_ipc::protocol::IpcRequest =
            serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        assert_eq!(
            decoded.command_id.as_deref(),
            Some(HANDSHAKE_PROBE_COMMAND_ID)
        );
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": "sess-live",
            }),
        )
        .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
        .expect("probe command_id must be valid")
        .with_daemon_session_id("sess-live")
        .expect("daemon_session_id must be valid");
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
        false,
        ShutdownFenceStatus {
            daemon_stopped: true,
            profile_released: false,
        },
        false,
    );
    assert_eq!(disposition, super::CloseAllDisposition::Failed);
}

#[test]
#[cfg(unix)]
fn close_all_targets_keep_protocol_incompatible_authority_entry() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_name = "default";
    let session_id = "sess-incompatible";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let projection = RubPaths::new(&home).session(session_name);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(
        projection.canonical_pid_path(),
        std::process::id().to_string(),
    )
    .unwrap();
    std::fs::write(projection.startup_committed_path(), session_id).unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();

    let session_id_for_server = session_id.to_string();
    let listener = bind_std_unix_listener(&runtime.socket_path());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        StdBufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        assert_eq!(
            decoded.command_id.as_deref(),
            Some(HANDSHAKE_PROBE_COMMAND_ID)
        );
        let mut response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": session_id_for_server,
            }),
        );
        response = response
            .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
            .expect("probe command_id must be valid");
        response.ipc_protocol_version = "1.0".to_string();
        serde_json::to_writer(&mut stream, &response).unwrap();
        stream.write_all(b"\n").unwrap();
    });

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: std::process::id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-03T00:00:00Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let snapshot = registry_authority_snapshot(&home).unwrap();
    let targets = close_all_session_targets(&snapshot);
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0]
            .authority_entry
            .as_ref()
            .map(|entry| entry.session_id.as_str()),
        Some(session_id)
    );

    server.join().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
#[cfg(unix)]
async fn close_all_zero_budget_preserves_compatibility_degraded_owned_family() {
    let mut failed = Vec::new();
    let mut compatibility_degraded_owned_sessions = Vec::new();

    super::close_all::record_close_all_budget_exhausted(
        super::close_all::CloseAllSessionTarget {
            session_name: "default".to_string(),
            authority_entry: None,
            compatibility_degraded_owned: Some(super::CompatibilityDegradedOwnedSession {
                session: "default".to_string(),
                daemon_session_id: "sess-hard-cut-zero-budget".to_string(),
                reason: CompatibilityDegradedOwnedReason::HardCutReleasePending,
            }),
            stale_entries: Vec::new(),
            has_uncertain_entries: false,
        },
        &mut failed,
        &mut compatibility_degraded_owned_sessions,
    );

    assert!(failed.is_empty());
    assert_eq!(compatibility_degraded_owned_sessions.len(), 1);
    assert_eq!(compatibility_degraded_owned_sessions[0].session, "default");
    assert_eq!(
        compatibility_degraded_owned_sessions[0].reason,
        CompatibilityDegradedOwnedReason::HardCutReleasePending
    );
}

#[test]
fn close_all_kill_fallback_requires_release_fence_failure_with_live_authority() {
    assert!(!should_escalate_close_all_to_kill_fallback(
        ShutdownFenceStatus {
            daemon_stopped: true,
            profile_released: false,
        },
        false,
        250,
    ));
    assert!(!should_escalate_close_all_to_kill_fallback(
        ShutdownFenceStatus {
            daemon_stopped: false,
            profile_released: false,
        },
        true,
        0,
    ));
    assert!(should_escalate_close_all_to_kill_fallback(
        ShutdownFenceStatus {
            daemon_stopped: false,
            profile_released: false,
        },
        true,
        250,
    ));
}

#[test]
fn close_all_immediately_escalates_external_sessions_after_graceful_close() {
    let external_entry = RegistryEntry {
        session_name: "default".to_string(),
        session_id: "sess-external".to_string(),
        pid: 42,
        created_at: "now".to_string(),
        socket_path: "/tmp/rub.sock".to_string(),
        user_data_dir: None,
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        attachment_identity: None,
        connection_target: Some(rub_core::model::ConnectionTarget::CdpUrl {
            url: "http://127.0.0.1:9222".to_string(),
        }),
    };
    let managed_entry = RegistryEntry {
        connection_target: Some(rub_core::model::ConnectionTarget::Managed),
        ..external_entry.clone()
    };

    assert!(requires_immediate_batch_shutdown_after_external_close(
        &external_entry,
        true
    ));
    assert!(!requires_immediate_batch_shutdown_after_external_close(
        &external_entry,
        false
    ));
    assert!(!requires_immediate_batch_shutdown_after_external_close(
        &managed_entry,
        true
    ));
}

#[test]
fn close_all_reports_post_fallback_release_as_cleaned_stale_not_closed() {
    let disposition = classify_close_all_result(
        true,
        true,
        ShutdownFenceStatus {
            daemon_stopped: true,
            profile_released: true,
        },
        false,
    );
    assert_eq!(disposition, super::CloseAllDisposition::CleanedStale);
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
async fn fetch_handshake_info_allows_version_mismatch_for_control_plane_compat_lane() {
    let socket_dir = std::path::PathBuf::from(format!("/tmp/rdi-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket_path = socket_dir.join("ipc.sock");
    let listener = bind_tokio_unix_listener(&socket_path);

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
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
        )
        .with_command_id(
            request
                .command_id
                .clone()
                .expect("handshake probe must carry command_id"),
        )
        .expect("probe command_id must be valid")
        .with_daemon_session_id("sess-default")
        .expect("daemon_session_id must be valid");
        response.ipc_protocol_version = "0.9".to_string();
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let handshake = fetch_handshake_info_with_timeout(&mut client, 1_000)
        .await
        .expect("compat handshake transport lane should remain open across version skew");
    assert_eq!(handshake.daemon_session_id, "sess-default");
    assert_eq!(handshake.ipc_protocol_version, "0.9");
    assert!(handshake.launch_policy.headless);
    assert_eq!(handshake.attachment_identity, None);

    server.await.expect("server join");
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn fetch_handshake_info_until_fails_closed_when_budget_is_already_exhausted() {
    let socket_dir = std::path::PathBuf::from(format!("/tmp/rdi-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket_path = socket_dir.join("ipc.sock");
    let mut client = IpcClient::deferred(socket_path);

    let error = fetch_handshake_info_until(
        &mut client,
        Instant::now() - Duration::from_millis(1),
        250,
        "existing_daemon_handshake",
    )
    .await
    .expect_err("exhausted attach budget should fail before sending handshake");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcTimeout);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("command_deadline_exhausted")
    );
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("phase"))
            .and_then(|value| value.as_str()),
        Some("existing_daemon_handshake")
    );

    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
async fn fetch_handshake_info_rejects_payload_daemon_authority_divergence() {
    let socket_dir = std::path::PathBuf::from(format!("/tmp/rdi-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&socket_dir).unwrap();
    let socket_path = socket_dir.join("ipc.sock");
    let listener = bind_tokio_unix_listener(&socket_path);

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let request: IpcRequest = NdJsonCodec::read(&mut reader)
            .await
            .expect("read request")
            .expect("request");
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": "sess-payload",
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
        .with_command_id(
            request
                .command_id
                .clone()
                .expect("handshake probe must carry command_id"),
        )
        .expect("probe command_id must be valid")
        .with_daemon_session_id("sess-echo")
        .expect("daemon_session_id must be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write response");
    });

    let mut client = IpcClient::connect(&socket_path).await.expect("connect");
    let error = fetch_handshake_info_with_timeout(&mut client, 1_000)
        .await
        .expect_err("diverged daemon authority must fail closed");
    let envelope = error.into_envelope();
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("handshake_daemon_session_id_mismatch")
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
async fn startup_lock_upgrade_adds_canonical_cdp_attachment_scope() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr");
    let ws_url = format!("ws://{address}/devtools/browser/test");
    let server = tokio::spawn({
        let ws_url = ws_url.clone();
        async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await.expect("read request");
            let body = format!(r#"{{"webSocketDebuggerUrl":"{ws_url}"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });

    let requested_identity = format!("cdp:http://{address}");
    let mut guard = acquire_startup_lock(&home, "default", Some(&requested_identity), 1_000)
        .await
        .expect("initial startup lock");
    let canonical_identity = upgrade_startup_lock_to_canonical_attachment_until(
        &mut guard,
        &home,
        Some(&requested_identity),
        Instant::now() + Duration::from_secs(1),
    )
    .await
    .expect("canonical startup lock upgrade");
    let expected_canonical_identity = format!("cdp:{ws_url}");
    assert_eq!(
        canonical_identity.as_deref(),
        Some(expected_canonical_identity.as_str())
    );

    assert!(guard.holds_scope_key_for_test("session-default"));
    assert!(guard.holds_scope_key_for_test(&format!("attachment-{requested_identity}")));
    assert!(guard.holds_scope_key_for_test(&format!("attachment-cdp:{ws_url}")));

    server.await.expect("server join");
}

#[tokio::test]
async fn startup_lock_upgrade_fails_closed_when_canonical_attachment_budget_exhausts() {
    use tokio::net::TcpListener;

    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr");
    let requested_identity = format!("cdp:http://{address}");
    let expected_canonical_scope = format!("attachment-cdp:ws://{address}/devtools/browser/test");
    let mut guard = acquire_startup_lock(&home, "default", Some(&requested_identity), 1_000)
        .await
        .expect("initial startup lock");

    let server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let started = std::time::Instant::now();
    let error = upgrade_startup_lock_to_canonical_attachment_until(
        &mut guard,
        &home,
        Some(&requested_identity),
        Instant::now() + Duration::from_millis(75),
    )
    .await
    .expect_err("canonical attachment upgrade must fail closed when the shared deadline exhausts")
    .into_envelope();
    assert_eq!(error.code, ErrorCode::CdpConnectionFailed);
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "canonical attachment upgrade must stay bounded by the shared startup deadline"
    );
    assert!(guard.holds_scope_key_for_test("session-default"));
    assert!(guard.holds_scope_key_for_test(&format!("attachment-{requested_identity}")));
    assert!(
        !guard.holds_scope_key_for_test(&expected_canonical_scope),
        "failed canonicalization must not publish a canonical attachment scope"
    );

    server.abort();
    let _ = std::fs::remove_dir_all(home);
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
    let cleanup = temp_home().join("cleanup.signal");
    unsafe {
        std::env::set_var("RUB_DAEMON_READY_FILE", &ready);
        std::env::set_var("RUB_DAEMON_ERROR_FILE", &error);
        std::env::set_var("RUB_DAEMON_CLEANUP_FILE", &cleanup);
    }
    let (ready_path, error_path) = startup_signal_paths();
    assert_eq!(ready_path.as_deref(), Some(ready.as_path()));
    assert_eq!(error_path.as_deref(), Some(error.as_path()));
    assert_eq!(
        startup_cleanup_signal_path().as_deref(),
        Some(cleanup.as_path())
    );
    unsafe {
        std::env::remove_var("RUB_DAEMON_READY_FILE");
        std::env::remove_var("RUB_DAEMON_ERROR_FILE");
        std::env::remove_var("RUB_DAEMON_CLEANUP_FILE");
    }
}

#[test]
fn startup_cleanup_proof_round_trip_preserves_managed_browser_authority() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let cleanup_file = home.join("startup.cleanup");
    let proof = StartupCleanupProof {
        kind: StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
        managed_user_data_dir: "/tmp/rub-managed-profile".to_string(),
        managed_profile_directory: None,
        ephemeral: true,
    };

    write_startup_cleanup_proof_at(&cleanup_file, &proof).expect("write cleanup proof");
    let decoded = read_startup_cleanup_proof(&cleanup_file).expect("read cleanup proof");
    assert_eq!(decoded, proof);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_cleanup_proof_round_trip_preserves_profile_scoped_authority() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let cleanup_file = home.join("startup.cleanup");
    let proof = StartupCleanupProof {
        kind: StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
        managed_user_data_dir: "/Users/test/Chrome".to_string(),
        managed_profile_directory: Some("Profile 3".to_string()),
        ephemeral: false,
    };

    write_startup_cleanup_proof_at(&cleanup_file, &proof).expect("write cleanup proof");
    let decoded = read_startup_cleanup_proof(&cleanup_file).expect("read cleanup proof");
    assert_eq!(decoded, proof);

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn failed_startup_cleanup_consumes_precommit_cleanup_proof() {
    let home = temp_home();
    std::fs::create_dir_all(&home).unwrap();
    let cleanup_file = home.join("startup.cleanup");
    let proof = StartupCleanupProof {
        kind: StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
        managed_user_data_dir: home.join("managed-profile").display().to_string(),
        managed_profile_directory: None,
        ephemeral: true,
    };
    write_startup_cleanup_proof_at(&cleanup_file, &proof).expect("write cleanup proof");

    let (attempted, succeeded, authority, error, retained, clear_error) =
        cleanup_startup_fallback_browser_authority_for_test(&cleanup_file).await;
    assert!(attempted);
    assert!(succeeded);
    assert_eq!(authority, Some(proof));
    assert!(error.is_none());
    assert!(!retained);
    assert!(clear_error.is_none());
    assert!(!cleanup_file.exists());

    let _ = std::fs::remove_dir_all(home);
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
    let stderr_file = session_paths.startup_stderr_path("startup");
    let cleanup_file = session_paths.startup_cleanup_path("startup");
    let socket_path = session_paths.socket_path();
    let listener = bind_tokio_unix_listener(&socket_path);

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
        )
        .with_command_id(
            request
                .command_id
                .clone()
                .expect("handshake probe must carry command_id"),
        )
        .expect("probe command_id must be valid")
        .with_daemon_session_id("sess-default")
        .expect("daemon_session_id must be valid");
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
        let response = IpcResponse::success("req-2", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("bound doctor request should carry command_id"),
            )
            .expect("request command_id must remain protocol-valid")
            .with_daemon_session_id("sess-default")
            .expect("daemon_session_id must be valid");
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
        stderr_file,
        cleanup_file,
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
async fn wait_for_ready_until_rejects_attachment_identity_mismatch() {
    let home = temp_home();
    let session_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::create_dir_all(session_paths.session_dir()).unwrap();
    let ready_file = session_paths.startup_ready_path("startup");
    let error_file = session_paths.startup_error_path("startup");
    let stderr_file = session_paths.startup_stderr_path("startup");
    let cleanup_file = session_paths.startup_cleanup_path("startup");
    let socket_path = session_paths.socket_path();
    let listener = bind_tokio_unix_listener(&socket_path);

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
                "attachment_identity": "profile:/tmp/work/Profile 9",
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
        )
        .with_command_id(
            request
                .command_id
                .clone()
                .expect("handshake probe must carry command_id"),
        )
        .expect("probe command_id must be valid")
        .with_daemon_session_id("sess-default")
        .expect("daemon_session_id must be valid");
        NdJsonCodec::write(&mut writer, &response)
            .await
            .expect("write handshake response");
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
        stderr_file,
        cleanup_file,
        daemon_pid: std::process::id(),
        session_id: "sess-default".to_string(),
    };

    let error = match wait_for_ready_until(
        &home,
        "default",
        &signals,
        Instant::now() + Duration::from_millis(3_000),
        Some("profile:/tmp/work/Profile 3"),
    )
    .await
    {
        Ok(_) => panic!("startup readiness must fail closed on attachment mismatch"),
        Err(error) => error.into_envelope(),
    };

    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("handshake_attachment_identity_mismatch")
    );

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
    let stderr_file = session_paths.startup_stderr_path("startup");
    let cleanup_file = session_paths.startup_cleanup_path("startup");
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
        stderr_file,
        cleanup_file,
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
async fn wait_for_ready_uses_startup_stderr_excerpt_when_daemon_dies_before_commit() {
    let home = temp_home();
    let session_paths = RubPaths::new(&home).session_runtime("default", "sess-dead-stderr");
    std::fs::create_dir_all(session_paths.session_dir()).unwrap();
    let ready_file = session_paths.startup_ready_path("startup");
    let error_file = session_paths.startup_error_path("startup");
    let stderr_file = session_paths.startup_stderr_path("startup");
    let cleanup_file = session_paths.startup_cleanup_path("startup");
    std::fs::write(&ready_file, b"ready").unwrap();
    std::fs::write(
        &stderr_file,
        b"DAEMON_START_FAILED: session runtime dir create failed\n",
    )
    .unwrap();

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
        stderr_file,
        cleanup_file,
        daemon_pid,
        session_id: "sess-dead-stderr".to_string(),
    };

    let envelope = wait_for_ready(&home, "default", &signals, 3_000)
        .await
        .err()
        .expect("stderr fallback should surface")
        .into_envelope();
    assert_eq!(
        envelope.message,
        "DAEMON_START_FAILED: session runtime dir create failed"
    );
    let context = envelope.context.expect("stderr fallback context");
    assert_eq!(context["reason"], "daemon_exited_before_startup_commit");
    assert_eq!(
        context["startup_stderr_state"]["path_authority"],
        "daemon_ctl.startup.stderr_file"
    );
    assert_eq!(
        context["startup_stderr_excerpt"],
        "DAEMON_START_FAILED: session runtime dir create failed"
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
    let stderr_file = session_paths.startup_stderr_path("startup");
    let cleanup_file = session_paths.startup_cleanup_path("startup");
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
        stderr_file,
        cleanup_file,
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
            compatibility_degraded_owned_sessions: vec![],
            failed: vec!["broken".to_string()],
            session_error_details: vec![super::BatchCloseSessionError {
                session: "broken".to_string(),
                error: rub_core::error::ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "replay recovery failed",
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_replay_retry_failed",
                    "recovery_contract": {
                        "kind": "session_post_commit_journal",
                    },
                })),
            }],
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
    assert_eq!(
        projected["result"]["session_error_details"][0]["error"]["context"]["recovery_contract"]["kind"],
        serde_json::json!("session_post_commit_journal")
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
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
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
        attachment_identity: None,
    };

    let error = super::maybe_upgrade_if_needed(
        Path::new("/tmp/rub-home"),
        "default",
        Some(&entry),
        &handshake,
        Path::new("/tmp/rub-home/default.sock"),
        None,
        None,
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

#[tokio::test]
async fn daemon_attachment_identity_mismatch_fails_closed() {
    let entry = RegistryEntry {
        session_id: "sess-registry".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: "/tmp/rub-home/default.sock".to_string(),
        created_at: "2026-04-08T00:00:00Z".to_string(),
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: Some("profile:/tmp/work/Profile 3".to_string()),
        connection_target: None,
    };
    let handshake = super::HandshakePayload {
        daemon_session_id: "sess-registry".to_string(),
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
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
        attachment_identity: Some("profile:/tmp/work/Profile 9".to_string()),
    };

    let error = super::maybe_upgrade_if_needed(
        Path::new("/tmp/rub-home"),
        "default",
        Some(&entry),
        &handshake,
        Path::new("/tmp/rub-home/default.sock"),
        None,
        None,
    )
    .await
    .err()
    .expect("attachment identity mismatch must fail")
    .into_envelope();

    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("handshake_attachment_identity_mismatch")
    );
}

#[tokio::test]
async fn authority_bound_connected_client_rejects_socket_identity_replacement() {
    let root = std::path::PathBuf::from(format!("/tmp/rbc-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let socket_path = root.join("daemon.sock");
    let listener = bind_tokio_unix_listener(&socket_path);
    let expected_identity = current_socket_path_identity(
        &socket_path,
        "daemon_ctl.connect.socket_path",
        "session_socket_candidates",
        ErrorCode::IpcProtocolError,
        "verified_daemon_authority_socket_identity_read_failed",
    )
    .expect("socket identity");
    drop(listener);
    let _ = std::fs::remove_file(&socket_path);
    let replacement_listener = bind_tokio_unix_listener(&socket_path);

    let error = match authority_bound_connected_client(
        &socket_path,
        "sess-default",
        expected_identity,
        None,
        AuthorityBoundConnectSpec {
            phase: "existing_daemon_authority_bind",
            error_code: ErrorCode::IpcProtocolError,
            message_prefix: "Failed to connect the verified daemon authority",
            path_authority: "daemon_ctl.connect.socket_path",
            upstream_truth: "session_socket_candidates",
        },
    )
    .await
    {
        Ok(_) => panic!("replaced socket authority must fail closed"),
        Err(error) => error.into_envelope(),
    };

    assert_eq!(error.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("verified_daemon_authority_socket_replaced")
    );

    drop(replacement_listener);
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn verify_socket_path_identity_rejects_startup_handshake_socket_replacement() {
    let root = std::path::PathBuf::from(format!("/tmp/rbs-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let socket_path = root.join("daemon.sock");
    let listener = bind_tokio_unix_listener(&socket_path);
    let expected_identity = current_socket_path_identity(
        &socket_path,
        "daemon_ctl.startup.handshake.socket_path",
        "startup_ready_monitor.socket_path",
        ErrorCode::DaemonStartFailed,
        "verified_daemon_authority_socket_identity_read_failed",
    )
    .expect("socket identity");
    drop(listener);
    let _ = std::fs::remove_file(&socket_path);
    let replacement_listener = bind_tokio_unix_listener(&socket_path);

    let error = super::verify_socket_path_identity(
        &socket_path,
        expected_identity,
        &AuthorityBoundConnectSpec {
            phase: "startup_handshake",
            error_code: ErrorCode::DaemonStartFailed,
            message_prefix:
                "Failed to connect to the daemon socket while confirming startup readiness",
            path_authority: "daemon_ctl.startup.handshake.socket_path",
            upstream_truth: "startup_ready_monitor.socket_path",
        },
    )
    .expect_err("replaced startup handshake socket authority must fail closed")
    .into_envelope();

    assert_eq!(error.code, ErrorCode::DaemonStartFailed);
    assert_eq!(
        error
            .context
            .as_ref()
            .and_then(|context| context.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("verified_daemon_authority_socket_replaced")
    );

    drop(replacement_listener);
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(root);
}
