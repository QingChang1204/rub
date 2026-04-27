use super::{
    StartupCommitGuard, protocol_read_failure_response, publish_pid_projection,
    publish_socket_projection, publish_startup_commit_marker, signal_ready,
    wait_for_transaction_drain,
};
use crate::daemon::io::read_failure_envelope;
use crate::daemon::shutdown::{
    wait_for_transaction_drain_with_timeout, wait_for_worker_shutdown_with_timeout,
};
use crate::rub_paths::RubPaths;
use crate::session::{RegistryEntry, SessionState, read_registry, write_registry};
use rub_core::error::ErrorCode;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_HOME_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_home() -> std::path::PathBuf {
    let sequence = TEMP_HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("rub-daemon-test-{}-{sequence}", std::process::id()))
}

fn ensure_socket_path_parent(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
}

#[test]
fn publish_pid_projection_writes_canonical_pid_projection() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_paths = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(session_paths.session_dir()).unwrap();
    if let Some(parent) = session_paths.socket_path().parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(session_paths.socket_path(), b"socket").unwrap();
    let state = SessionState::new("default", home.clone(), None);

    publish_pid_projection(&state, 4242).unwrap();

    assert_eq!(
        std::fs::read_to_string(session_paths.canonical_pid_path())
            .unwrap()
            .trim(),
        "4242"
    );

    let _ = std::fs::remove_file(session_paths.socket_path());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn publish_socket_projection_links_canonical_socket_to_actual_socket() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let state = SessionState::new_with_id("default", "sess-default", home.clone(), None);
    let runtime_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
    let projection_paths = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(runtime_paths.session_dir()).unwrap();
    if let Some(parent) = runtime_paths.socket_path().parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(runtime_paths.socket_path(), b"socket").unwrap();

    publish_socket_projection(&state).unwrap();

    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::read_link(projection_paths.canonical_socket_path()).unwrap(),
            runtime_paths.socket_path()
        );
    }

    let _ = std::fs::remove_file(runtime_paths.socket_path());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_failure_envelope_classifies_partial_frames_as_protocol_errors() {
    let error = std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "NDJSON frame terminated before newline commit fence",
    );
    let envelope = read_failure_envelope(Box::new(error));
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("partial_ndjson_frame")
    );
}

#[test]
fn read_failure_envelope_classifies_oversized_frames_as_protocol_errors() {
    let error = rub_ipc::codec::oversized_frame_io_error(rub_ipc::codec::MAX_FRAME_BYTES);
    let envelope = read_failure_envelope(Box::new(error));
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("oversized_ndjson_frame")
    );
}

#[test]
fn protocol_read_failure_response_wraps_structured_error_envelope() {
    let response =
        protocol_read_failure_response(read_failure_envelope(Box::new(serde_json::Error::io(
            std::io::Error::new(std::io::ErrorKind::InvalidData, "bad json"),
        ))));
    assert_eq!(response.status, rub_ipc::protocol::ResponseStatus::Error);
    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some(ErrorCode::IpcProtocolError)
    );
}

#[test]
fn read_failure_envelope_preserves_request_schema_reason() {
    let envelope = read_failure_envelope(Box::new(rub_ipc::protocol::IpcProtocolDecodeError::new(
        rub_ipc::protocol::IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": "1.0",
            "command": "doctor",
            "args": {},
            "timeout_ms": 1000,
            "unexpected": "field",
        }))
        .expect_err("strict decode should reject unknown fields"),
    )));
    assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    assert_eq!(
        envelope
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("reason"))
            .and_then(|value| value.as_str()),
        Some("invalid_ipc_request_schema")
    );
}

#[test]
fn signal_ready_reports_write_failures() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    unsafe {
        std::env::set_var("RUB_DAEMON_READY_FILE", &home);
    }
    let error = signal_ready().expect_err("directory path should fail ready marker write");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::IsADirectory
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Other
    ));
    unsafe {
        std::env::remove_var("RUB_DAEMON_READY_FILE");
    }

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_rolls_back_registry_and_projections_before_commit() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let state = SessionState::new_with_id("default", "sess-default", home.clone(), None);
    let runtime_paths = RubPaths::new(&home).session_runtime("default", "sess-default");
    let projection_paths = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(runtime_paths.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&runtime_paths.socket_path());
    std::fs::write(runtime_paths.socket_path(), b"socket").unwrap();
    std::fs::write(runtime_paths.pid_path(), b"4242").unwrap();
    publish_socket_projection(&state).unwrap();
    publish_pid_projection(&state, 4242).unwrap();
    publish_startup_commit_marker(&state).unwrap();

    let entry = RegistryEntry {
        session_id: "sess-default".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: runtime_paths.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![entry.clone()],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, entry, None);
    }

    assert!(!runtime_paths.socket_path().exists());
    assert!(!runtime_paths.pid_path().exists());
    assert!(!projection_paths.canonical_socket_path().exists());
    assert!(!projection_paths.canonical_pid_path().exists());
    assert!(!projection_paths.startup_committed_path().exists());
    assert!(read_registry(&home).unwrap().sessions.is_empty());

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_skips_stale_previous_authority_restore() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let state = SessionState::new_with_id("default", "sess-new", home.clone(), None);
    let runtime_paths = RubPaths::new(&home).session_runtime("default", "sess-new");
    let projection_paths = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(runtime_paths.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&runtime_paths.socket_path());
    std::fs::write(runtime_paths.socket_path(), b"socket").unwrap();
    std::fs::write(runtime_paths.pid_path(), b"4242").unwrap();
    publish_socket_projection(&state).unwrap();
    publish_pid_projection(&state, 4242).unwrap();
    publish_startup_commit_marker(&state).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-new".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: runtime_paths.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: 9_999_999,
        socket_path: RubPaths::new(&home)
            .session_runtime("default", "sess-old")
            .socket_path()
            .display()
            .to_string(),
        created_at: "2026-03-31T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![previous_entry.clone(), current_entry.clone()],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert!(!runtime_paths.socket_path().exists());
    assert!(!runtime_paths.pid_path().exists());
    assert!(!projection_paths.canonical_socket_path().exists());
    assert!(!projection_paths.canonical_pid_path().exists());
    assert!(!projection_paths.startup_committed_path().exists());
    assert!(
        read_registry(&home).unwrap().sessions.is_empty(),
        "stale previous authority must not be republished during rollback"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_does_not_restore_previous_authority_without_public_commit_marker() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let current_state = SessionState::new_with_id("default", "sess-current", home.clone(), None);
    let current_runtime = RubPaths::new(&home).session_runtime("default", "sess-current");
    let previous_runtime = RubPaths::new(&home).session_runtime("default", "sess-old");
    let projection_paths = RubPaths::new(&home).session("default");

    std::fs::create_dir_all(current_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(previous_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&current_runtime.socket_path());
    ensure_socket_path_parent(&previous_runtime.socket_path());
    std::fs::write(current_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(current_runtime.pid_path(), b"4242").unwrap();
    std::fs::write(previous_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(previous_runtime.socket_path(), b"socket").unwrap();

    publish_socket_projection(&current_state).unwrap();
    publish_pid_projection(&current_state, 4242).unwrap();
    publish_startup_commit_marker(&current_state).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-current".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: current_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:01Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: previous_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![previous_entry.clone(), current_entry.clone()],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert!(!projection_paths.canonical_socket_path().exists());
    assert!(!projection_paths.canonical_pid_path().exists());
    assert!(!projection_paths.startup_committed_path().exists());
    let registry = read_registry(&home).unwrap();
    assert!(
        registry.sessions.is_empty(),
        "rollback restore must not republish runtime-only authority without the public committed marker"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_restores_previous_authority_after_replacement_startup_failure() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let current_state = SessionState::new_with_id("default", "sess-current", home.clone(), None);
    let previous_state = SessionState::new_with_id("default", "sess-old", home.clone(), None);
    let current_runtime = RubPaths::new(&home).session_runtime("default", "sess-current");
    let previous_runtime = RubPaths::new(&home).session_runtime("default", "sess-old");
    let projection_paths = RubPaths::new(&home).session("default");

    std::fs::create_dir_all(current_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(previous_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&current_runtime.socket_path());
    ensure_socket_path_parent(&previous_runtime.socket_path());
    std::fs::write(current_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(current_runtime.pid_path(), b"4242").unwrap();
    std::fs::write(previous_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(previous_runtime.socket_path(), b"socket").unwrap();
    crate::session::force_live_registry_socket_probe_once_for_test(&previous_runtime.socket_path());
    crate::session::force_live_registry_socket_probe_once_for_test(&previous_runtime.socket_path());

    publish_socket_projection(&current_state).unwrap();
    publish_pid_projection(&current_state, 4242).unwrap();
    publish_startup_commit_marker(&previous_state).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-current".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: current_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:01Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: previous_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![previous_entry.clone(), current_entry.clone()],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert_eq!(
        std::fs::read_link(projection_paths.canonical_socket_path()).unwrap(),
        previous_runtime.socket_path()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.canonical_pid_path())
            .unwrap()
            .trim(),
        std::process::id().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.startup_committed_path())
            .unwrap()
            .trim(),
        "sess-old"
    );
    let registry = read_registry(&home).unwrap();
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-old")
    );
    assert!(
        !registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-current")
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_does_not_restore_previous_authority_over_newer_pending_startup() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let current_runtime = RubPaths::new(&home).session_runtime("default", "sess-current");
    let previous_state = SessionState::new_with_id("default", "sess-old", home.clone(), None);
    let previous_runtime = RubPaths::new(&home).session_runtime("default", "sess-old");
    let newer_state = SessionState::new_with_id("default", "sess-newer", home.clone(), None);
    let newer_runtime = RubPaths::new(&home).session_runtime("default", "sess-newer");
    let projection_paths = RubPaths::new(&home).session("default");

    std::fs::create_dir_all(current_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(previous_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(newer_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&current_runtime.socket_path());
    ensure_socket_path_parent(&previous_runtime.socket_path());
    ensure_socket_path_parent(&newer_runtime.socket_path());
    std::fs::write(current_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(current_runtime.pid_path(), b"4242").unwrap();
    std::fs::write(previous_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(newer_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(newer_runtime.pid_path(), std::process::id().to_string()).unwrap();
    publish_socket_projection(&previous_state).unwrap();
    publish_pid_projection(&previous_state, std::process::id()).unwrap();
    publish_startup_commit_marker(&previous_state).unwrap();
    publish_socket_projection(&newer_state).unwrap();
    publish_pid_projection(&newer_state, std::process::id()).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-current".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: current_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:01Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: previous_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let newer_entry = RegistryEntry {
        session_id: "sess-newer".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: newer_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:02Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![
                previous_entry.clone(),
                current_entry.clone(),
                newer_entry.clone(),
            ],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert_eq!(
        std::fs::read_link(projection_paths.canonical_socket_path()).unwrap(),
        newer_runtime.socket_path()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.canonical_pid_path())
            .unwrap()
            .trim(),
        std::process::id().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.startup_committed_path())
            .unwrap()
            .trim(),
        "sess-old",
        "pending newer startup must keep the live previous authority marker instead of restoring over newer candidate projections"
    );
    let registry = read_registry(&home).unwrap();
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-newer")
    );
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-old")
    );
    assert!(
        !registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-current")
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_does_not_restore_previous_authority_when_non_latest_candidate_is_pending() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let current_runtime = RubPaths::new(&home).session_runtime("default", "sess-current");
    let previous_state = SessionState::new_with_id("default", "sess-old", home.clone(), None);
    let previous_runtime = RubPaths::new(&home).session_runtime("default", "sess-old");
    let pending_state = SessionState::new_with_id("default", "sess-pending", home.clone(), None);
    let pending_runtime = RubPaths::new(&home).session_runtime("default", "sess-pending");
    let projection_paths = RubPaths::new(&home).session("default");

    std::fs::create_dir_all(current_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(previous_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(pending_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&current_runtime.socket_path());
    ensure_socket_path_parent(&previous_runtime.socket_path());
    ensure_socket_path_parent(&pending_runtime.socket_path());
    std::fs::write(current_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(current_runtime.pid_path(), b"4242").unwrap();
    std::fs::write(previous_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(pending_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(pending_runtime.pid_path(), std::process::id().to_string()).unwrap();
    publish_socket_projection(&previous_state).unwrap();
    publish_pid_projection(&previous_state, std::process::id()).unwrap();
    publish_startup_commit_marker(&previous_state).unwrap();
    publish_socket_projection(&pending_state).unwrap();
    publish_pid_projection(&pending_state, std::process::id()).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-current".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: current_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:01Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: previous_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let pending_entry = RegistryEntry {
        session_id: "sess-pending".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: pending_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:02Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let stale_latest_entry = RegistryEntry {
        session_id: "sess-stale-latest".to_string(),
        session_name: "default".to_string(),
        pid: 999_999,
        socket_path: RubPaths::new(&home)
            .session_runtime("default", "sess-stale-latest")
            .socket_path()
            .display()
            .to_string(),
        created_at: "2026-04-01T00:00:03Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![
                previous_entry.clone(),
                current_entry.clone(),
                pending_entry.clone(),
                stale_latest_entry,
            ],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert_eq!(
        std::fs::read_link(projection_paths.canonical_socket_path()).unwrap(),
        pending_runtime.socket_path()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.canonical_pid_path())
            .unwrap()
            .trim(),
        std::process::id().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.startup_committed_path())
            .unwrap()
            .trim(),
        "sess-old"
    );
    let registry = read_registry(&home).unwrap();
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-pending")
    );
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-old")
    );
    assert!(
        !registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-current")
    );

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_does_not_restore_previous_authority_over_newer_committed_authority() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let current_runtime = RubPaths::new(&home).session_runtime("default", "sess-current");
    let newer_state = SessionState::new_with_id("default", "sess-newer", home.clone(), None);
    let newer_runtime = RubPaths::new(&home).session_runtime("default", "sess-newer");
    let projection_paths = RubPaths::new(&home).session("default");

    std::fs::create_dir_all(current_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(newer_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&current_runtime.socket_path());
    ensure_socket_path_parent(&newer_runtime.socket_path());
    std::fs::write(current_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(current_runtime.pid_path(), b"4242").unwrap();
    std::fs::write(newer_runtime.pid_path(), std::process::id().to_string()).unwrap();

    std::fs::write(newer_runtime.socket_path(), b"socket").unwrap();

    publish_socket_projection(&newer_state).unwrap();
    publish_pid_projection(&newer_state, std::process::id()).unwrap();
    publish_startup_commit_marker(&newer_state).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-current".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: current_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:01Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: 9999,
        socket_path: RubPaths::new(&home)
            .session_runtime("default", "sess-old")
            .socket_path()
            .display()
            .to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let newer_entry = RegistryEntry {
        session_id: "sess-newer".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: newer_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:02Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![
                previous_entry.clone(),
                current_entry.clone(),
                newer_entry.clone(),
            ],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert_eq!(
        std::fs::read_link(projection_paths.canonical_socket_path()).unwrap(),
        newer_runtime.socket_path()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.canonical_pid_path())
            .unwrap()
            .trim(),
        std::process::id().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(projection_paths.startup_committed_path())
            .unwrap()
            .trim(),
        "sess-newer"
    );
    let registry = read_registry(&home).unwrap();
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-newer")
    );
    assert!(
        !registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-old")
    );
    assert!(
        !registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "sess-current")
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn startup_guard_drops_previous_authority_that_dies_during_restore() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let current_runtime = RubPaths::new(&home).session_runtime("default", "sess-current");
    let current_state = SessionState::new_with_id("default", "sess-current", home.clone(), None);
    let previous_state = SessionState::new_with_id("default", "sess-old", home.clone(), None);
    let previous_runtime = RubPaths::new(&home).session_runtime("default", "sess-old");
    let projection_paths = RubPaths::new(&home).session("default");

    std::fs::create_dir_all(current_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(previous_runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection_paths.projection_dir()).unwrap();
    ensure_socket_path_parent(&current_runtime.socket_path());
    ensure_socket_path_parent(&previous_runtime.socket_path());
    std::fs::write(current_runtime.socket_path(), b"socket").unwrap();
    std::fs::write(current_runtime.pid_path(), b"4242").unwrap();
    std::fs::write(previous_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(previous_runtime.socket_path(), b"socket").unwrap();
    crate::session::force_live_registry_socket_probe_once_for_test(&previous_runtime.socket_path());

    publish_socket_projection(&current_state).unwrap();
    publish_pid_projection(&current_state, 4242).unwrap();
    publish_startup_commit_marker(&current_state).unwrap();
    publish_socket_projection(&previous_state).unwrap();
    publish_pid_projection(&previous_state, std::process::id()).unwrap();

    let current_entry = RegistryEntry {
        session_id: "sess-current".to_string(),
        session_name: "default".to_string(),
        pid: 4242,
        socket_path: current_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:01Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let previous_entry = RegistryEntry {
        session_id: "sess-old".to_string(),
        session_name: "default".to_string(),
        pid: std::process::id(),
        socket_path: previous_runtime.socket_path().display().to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    write_registry(
        &home,
        &crate::session::RegistryData {
            sessions: vec![previous_entry.clone(), current_entry.clone()],
        },
    )
    .unwrap();

    {
        let _guard = StartupCommitGuard::new(&home, current_entry, Some(previous_entry));
    }

    assert!(!projection_paths.canonical_socket_path().exists());
    assert!(!projection_paths.canonical_pid_path().exists());
    assert!(!projection_paths.startup_committed_path().exists());
    let registry = read_registry(&home).unwrap();
    assert!(registry.sessions.is_empty());

    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn shutdown_drain_flushes_pending_post_commit_projections() {
    let state = Arc::new(SessionState::new_with_id(
        "default",
        "sess-default",
        temp_home(),
        None,
    ));
    let request = IpcRequest::new(
        "pipe",
        serde_json::json!({
            "spec": "[]",
            "spec_source": { "kind": "file", "path": "/tmp/workflow.json" }
        }),
        30_000,
    )
    .with_command_id("cmd-1")
    .expect("static command_id must be valid");
    let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}))
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");

    state.submit_post_commit_projection(&request, &response);
    wait_for_transaction_drain(&state).await;

    assert_eq!(state.pending_post_commit_projection_count(), 0);
    assert_eq!(state.command_history(5).await.entries.len(), 1);
    assert_eq!(state.workflow_capture(5).await.entries.len(), 1);
}

#[tokio::test]
async fn shutdown_drain_waits_past_soft_timeout_until_transactions_finish() {
    let state = Arc::new(SessionState::new_with_id(
        "default",
        "sess-default",
        temp_home(),
        None,
    ));
    state
        .in_flight_count
        .store(1, std::sync::atomic::Ordering::SeqCst);

    let drain_state = state.clone();
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        drain_state
            .in_flight_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
    });

    let start = tokio::time::Instant::now();
    wait_for_transaction_drain_with_timeout(
        &state,
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(1),
    )
    .await;
    let elapsed = start.elapsed();

    releaser.await.unwrap();
    assert!(
        elapsed >= std::time::Duration::from_millis(20),
        "drain returned before in-flight transaction quiesced"
    );
}

#[tokio::test]
async fn shutdown_drain_waits_past_soft_timeout_until_connected_request_fence_clears() {
    let state = Arc::new(SessionState::new_with_id(
        "default",
        "sess-default",
        temp_home(),
        None,
    ));
    state
        .connected_client_count
        .store(1, std::sync::atomic::Ordering::SeqCst);

    let drain_state = state.clone();
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        drain_state
            .connected_client_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
    });

    let start = tokio::time::Instant::now();
    wait_for_transaction_drain_with_timeout(
        &state,
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(1),
    )
    .await;
    let elapsed = start.elapsed();
    let metrics = state.automation_scheduler_metrics().await;
    releaser.await.unwrap();

    assert!(
        elapsed >= std::time::Duration::from_millis(20),
        "drain returned before the connected request fence quiesced"
    );
    assert_eq!(
        metrics["shutdown_drain"]["soft_timeout_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        metrics["shutdown_drain"]["connected_only_soft_release_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_in_flight_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_connected_client_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_pre_request_response_fence_count"],
        serde_json::json!(0)
    );
}

#[tokio::test]
async fn shutdown_drain_waits_past_soft_timeout_until_pre_request_response_fence_clears() {
    let state = Arc::new(SessionState::new_with_id(
        "default",
        "sess-default",
        temp_home(),
        None,
    ));
    state
        .pre_request_response_fence_count
        .store(1, std::sync::atomic::Ordering::SeqCst);

    let drain_state = state.clone();
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        drain_state
            .pre_request_response_fence_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
    });

    let start = tokio::time::Instant::now();
    wait_for_transaction_drain_with_timeout(
        &state,
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(1),
    )
    .await;
    let elapsed = start.elapsed();
    let metrics = state.automation_scheduler_metrics().await;
    releaser.await.unwrap();

    assert!(
        elapsed >= std::time::Duration::from_millis(20),
        "drain returned before the pre-request response fence quiesced"
    );
    assert_eq!(
        metrics["shutdown_drain"]["soft_timeout_count"],
        serde_json::json!(1)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_in_flight_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_connected_client_count"],
        serde_json::json!(0)
    );
    assert_eq!(
        metrics["shutdown_drain"]["max_observed_pre_request_response_fence_count"],
        serde_json::json!(1)
    );
}

#[tokio::test]
async fn shutdown_drain_waits_for_post_commit_followups_after_live_fences_clear() {
    let state = Arc::new(SessionState::new_with_id(
        "default",
        "sess-default",
        temp_home(),
        None,
    ));
    state
        .post_commit_followup_count
        .store(1, std::sync::atomic::Ordering::SeqCst);

    let drain_state = state.clone();
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        drain_state
            .post_commit_followup_count
            .store(0, std::sync::atomic::Ordering::SeqCst);
    });

    let start = tokio::time::Instant::now();
    wait_for_transaction_drain_with_timeout(
        &state,
        std::time::Duration::from_millis(5),
        std::time::Duration::from_millis(1),
    )
    .await;
    let elapsed = start.elapsed();

    releaser.await.unwrap();
    assert!(
        elapsed >= std::time::Duration::from_millis(20),
        "drain returned before downstream post-commit followups quiesced"
    );
    assert_eq!(
        state
            .in_flight_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(
        state
            .connected_client_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(
        state
            .pre_request_response_fence_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn worker_shutdown_waits_past_soft_timeout_without_abort() {
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed_worker = completed.clone();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        completed_worker.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let start = tokio::time::Instant::now();
    wait_for_worker_shutdown_with_timeout(
        handle,
        "test_worker",
        std::time::Duration::from_millis(5),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(
        elapsed >= std::time::Duration::from_millis(20),
        "worker shutdown returned before the worker naturally finished"
    );
    assert!(
        completed.load(std::sync::atomic::Ordering::SeqCst),
        "worker should complete instead of being aborted at the soft timeout"
    );
}
