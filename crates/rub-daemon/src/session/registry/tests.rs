use super::{
    RegistryData, RegistryEntry, authoritative_entry_by_session_name, cleanup_projections,
    is_matching_rub_daemon_command, latest_entry_by_session_name, new_session_id,
    promote_session_authority, read_registry, register_pending_session, register_session,
    registry_authority_snapshot, registry_entry_is_live_for_home,
    registry_entry_is_pending_startup_for_home, write_registry,
};
use crate::rub_paths::RubPaths;
use rub_ipc::codec::NdJsonCodec;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::time::Duration;
use uuid::Uuid;

fn temp_home() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rub-registry-test-{}", Uuid::now_v7()))
}

#[test]
fn read_registry_does_not_create_missing_home() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);

    let registry = read_registry(&home).expect("missing home should read as empty registry");

    assert!(registry.sessions.is_empty());
    assert!(
        !home.exists(),
        "read-only registry access must not create missing RUB_HOME"
    );
}

#[test]
fn read_registry_rejects_entries_missing_canonical_session_id() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_name": "default",
                "pid": 1234,
                "socket_path": runtime.socket_path(),
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("noncanonical schema should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_entries_with_invalid_canonical_session_id() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "../escape",
                "session_name": "default",
                "pid": 1234,
                "socket_path": runtime.socket_path(),
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("invalid session_id should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_entries_with_noncanonical_created_at() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "default",
                "pid": 1234,
                "socket_path": runtime.socket_path(),
                "created_at": "2026-03-31T00:00:00.0Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("noncanonical created_at should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_entries_with_invalid_session_name() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("bad/name", "sess-default");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "bad/name",
                "pid": 1234,
                "socket_path": runtime.socket_path(),
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("invalid session_name should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_entries_with_noncanonical_socket_path() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "default",
                "pid": 1234,
                "socket_path": "/tmp/not-rub.sock",
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("noncanonical socket_path should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_entries_with_zero_pid() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "default",
                "pid": 0,
                "socket_path": runtime.socket_path(),
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("zero pid should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_entries_with_invalid_protocol_version_shape() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let runtime = RubPaths::new(&home).session_runtime("default", "sess-default");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "default",
                "pid": 1234,
                "socket_path": runtime.socket_path(),
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": " 1.x ",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home).expect_err("invalid protocol version should be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn write_registry_preserves_explicit_session_id() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let session_id = new_session_id();
    let runtime = RubPaths::new(&home).session_runtime("default", &session_id);
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.clone(),
                session_name: "default".to_string(),
                pid: 1234,
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-03-31T00:00:00Z".to_string(),
                ipc_protocol_version: "1.0".to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let registry = read_registry(&home).unwrap();
    assert_eq!(registry.sessions[0].session_id, session_id);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn pending_session_keeps_existing_same_name_authority_until_promoted() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let old_runtime = RubPaths::new(&home).session_runtime("default", "old");
    let new_runtime = RubPaths::new(&home).session_runtime("default", "new");

    register_session(
        &home,
        RegistryEntry {
            session_id: "old".to_string(),
            session_name: "default".to_string(),
            pid: 1234,
            socket_path: old_runtime.socket_path().display().to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        },
    )
    .unwrap();
    register_pending_session(
        &home,
        RegistryEntry {
            session_id: "new".to_string(),
            session_name: "default".to_string(),
            pid: 5678,
            socket_path: new_runtime.socket_path().display().to_string(),
            created_at: "2026-04-01T00:00:01Z".to_string(),
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        },
    )
    .unwrap();

    let registry = read_registry(&home).unwrap();
    assert_eq!(registry.sessions.len(), 2);
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "old")
    );
    assert!(
        registry
            .sessions
            .iter()
            .any(|entry| entry.session_id == "new")
    );

    promote_session_authority(&home, "default", "new").unwrap();
    let registry = read_registry(&home).unwrap();
    assert_eq!(registry.sessions.len(), 1);
    assert_eq!(registry.sessions[0].session_id, "new");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn live_registry_identity_requires_matching_session_and_home() {
    let home = temp_home();
    let command = format!(
        r#"/workspace/target/debug/rub __daemon --session default --rub-home "{}""#,
        home.display()
    );
    assert!(is_matching_rub_daemon_command(&command, &home, "default"));
    assert!(!is_matching_rub_daemon_command(&command, &home, "other"));
    assert!(!is_matching_rub_daemon_command(
        &command,
        &home.join("nested"),
        "default"
    ));
}

#[test]
#[cfg(unix)]
fn live_registry_identity_requires_socket_handshake() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_name = "default";
    let session_id = "sess-default";
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

    let listener = UnixListener::bind(runtime.socket_path()).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": "sess-default",
            }),
        );
        serde_json::to_writer(&mut stream, &response).unwrap();
        stream.write_all(b"\n").unwrap();
    });

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: std::process::id(),
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-02T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    assert!(registry_entry_is_live_for_home(&home, &entry));
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn live_registry_identity_requires_matching_handshake_session_id() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_name = "default";
    let session_id = "sess-default";
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

    let listener = UnixListener::bind(runtime.socket_path()).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        let response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": "other-session",
            }),
        );
        serde_json::to_writer(&mut stream, &response).unwrap();
        stream.write_all(b"\n").unwrap();
    });

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: std::process::id(),
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-02T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    assert!(!registry_entry_is_live_for_home(&home, &entry));
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn live_registry_identity_requires_matching_protocol_version() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_name = "default";
    let session_id = "sess-default";
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

    let listener = UnixListener::bind(runtime.socket_path()).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
        assert_eq!(decoded.command, "_handshake");
        let mut response = IpcResponse::success(
            "req-1",
            serde_json::json!({
                "daemon_session_id": "sess-default",
            }),
        );
        response.ipc_protocol_version = "0.9".to_string();
        serde_json::to_writer(&mut stream, &response).unwrap();
        stream.write_all(b"\n").unwrap();
    });

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: std::process::id(),
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-02T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    assert!(!registry_entry_is_live_for_home(&home, &entry));
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn slow_handshake_is_treated_as_busy_not_dead() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_name = "default";
    let session_id = "sess-slow";
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

    let listener = UnixListener::bind(runtime.socket_path()).unwrap();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(900));
    });

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: std::process::id(),
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-02T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    assert!(registry_entry_is_live_for_home(&home, &entry));
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn pending_startup_is_not_live_but_is_explicitly_detectable() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    let session_name = "default";
    let session_id = "sess-pending";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::write(runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::write(runtime.socket_path(), b"socket").unwrap();

    let entry = RegistryEntry {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        pid: std::process::id(),
        socket_path: runtime.socket_path().display().to_string(),
        created_at: "2026-04-02T00:00:00Z".to_string(),
        ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };

    assert!(!registry_entry_is_live_for_home(&home, &entry));
    assert!(registry_entry_is_pending_startup_for_home(&home, &entry));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn register_session_replaces_same_session_name_authority() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let old_runtime = RubPaths::new(&home).session_runtime("default", "old");
    let new_runtime = RubPaths::new(&home).session_runtime("default", "new");

    register_session(
        &home,
        RegistryEntry {
            session_id: "old".to_string(),
            session_name: "default".to_string(),
            pid: 1234,
            socket_path: old_runtime.socket_path().display().to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        },
    )
    .unwrap();
    register_session(
        &home,
        RegistryEntry {
            session_id: "new".to_string(),
            session_name: "default".to_string(),
            pid: 5678,
            socket_path: new_runtime.socket_path().display().to_string(),
            created_at: "2026-04-01T00:00:01Z".to_string(),
            ipc_protocol_version: "1.0".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        },
    )
    .unwrap();

    let registry = read_registry(&home).unwrap();
    assert_eq!(registry.sessions.len(), 1);
    assert_eq!(registry.sessions[0].session_id, "new");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn authoritative_entry_by_session_name_falls_back_to_newest_stale_entry() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let older_runtime = RubPaths::new(&home).session_runtime("default", "older");
    let newer_runtime = RubPaths::new(&home).session_runtime("default", "newer");
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: "older".to_string(),
                    session_name: "default".to_string(),
                    pid: 1234,
                    socket_path: older_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-01T00:00:00Z".to_string(),
                    ipc_protocol_version: "1.0".to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "newer".to_string(),
                    session_name: "default".to_string(),
                    pid: 5678,
                    socket_path: newer_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-01T00:00:01Z".to_string(),
                    ipc_protocol_version: "1.0".to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        },
    )
    .unwrap();

    assert!(
        authoritative_entry_by_session_name(&home, "default")
            .unwrap()
            .is_none()
    );
    let entry = latest_entry_by_session_name(&home, "default")
        .unwrap()
        .expect("latest entry");
    assert_eq!(entry.session_id, "newer");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn registry_authority_snapshot_classifies_stale_and_uncertain_entries_once() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let live_runtime = RubPaths::new(&home).session_runtime("default", "sess-live");
    std::fs::create_dir_all(live_runtime.session_dir()).unwrap();
    std::fs::write(live_runtime.pid_path(), std::process::id().to_string()).unwrap();
    std::fs::create_dir_all(
        live_runtime
            .startup_committed_path()
            .parent()
            .expect("startup committed parent"),
    )
    .unwrap();
    std::fs::write(live_runtime.startup_committed_path(), "sess-live").unwrap();
    let listener = UnixListener::bind(live_runtime.socket_path()).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: IpcRequest = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(request.command, "_handshake");
        let response = IpcResponse::success(
            request.command.clone(),
            serde_json::json!({
                "daemon_session_id": "sess-live",
            }),
        );
        let encoded = NdJsonCodec::encode(&response).unwrap();
        reader.get_mut().write_all(&encoded).unwrap();
    });

    let dead_runtime = RubPaths::new(&home).session_runtime("default", "sess-dead");
    let uncertain_runtime = RubPaths::new(&home).session_runtime("default", "sess-uncertain");
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: "sess-dead".to_string(),
                    session_name: "default".to_string(),
                    pid: 999_999,
                    socket_path: dead_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-03T00:00:00Z".to_string(),
                    ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "sess-uncertain".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: uncertain_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-03T00:00:01Z".to_string(),
                    ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "sess-live".to_string(),
                    session_name: "default".to_string(),
                    pid: std::process::id(),
                    socket_path: live_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-03T00:00:02Z".to_string(),
                    ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        },
    )
    .unwrap();

    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session = snapshot.session("default").expect("session snapshot");
    assert_eq!(
        session
            .authoritative_entry()
            .map(|entry| entry.entry.session_id.as_str()),
        Some("sess-live")
    );
    assert_eq!(
        session
            .stale_entries()
            .into_iter()
            .map(|entry| entry.session_id)
            .collect::<Vec<_>>(),
        vec!["sess-dead".to_string()]
    );
    assert!(session.has_uncertain_entries());

    server.join().unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn latest_entry_by_session_name_orders_by_parsed_timestamp_not_raw_string() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let older_runtime = RubPaths::new(&home).session_runtime("default", "older");
    let newer_runtime = RubPaths::new(&home).session_runtime("default", "newer");
    write_registry(
        &home,
        &RegistryData {
            sessions: vec![
                RegistryEntry {
                    session_id: "older".to_string(),
                    session_name: "default".to_string(),
                    pid: 1234,
                    socket_path: older_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-01T00:00:00Z".to_string(),
                    ipc_protocol_version: "1.0".to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
                RegistryEntry {
                    session_id: "newer".to_string(),
                    session_name: "default".to_string(),
                    pid: 5678,
                    socket_path: newer_runtime.socket_path().display().to_string(),
                    created_at: "2026-04-01T00:00:00.9Z".to_string(),
                    ipc_protocol_version: "1.0".to_string(),
                    user_data_dir: None,
                    attachment_identity: None,
                    connection_target: None,
                },
            ],
        },
    )
    .unwrap();

    let entry = latest_entry_by_session_name(&home, "default")
        .unwrap()
        .expect("latest entry");
    assert_eq!(entry.session_id, "newer");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn cleanup_projections_preserves_foreign_startup_commit_marker() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let old_entry = RegistryEntry {
        session_id: "old".to_string(),
        session_name: "default".to_string(),
        pid: 1234,
        socket_path: RubPaths::new(&home)
            .session_runtime("default", "old")
            .socket_path()
            .display()
            .to_string(),
        created_at: "2026-04-01T00:00:00Z".to_string(),
        ipc_protocol_version: "1.0".to_string(),
        user_data_dir: None,
        attachment_identity: None,
        connection_target: None,
    };
    let projection = RubPaths::new(&home).session("default");
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::write(projection.startup_committed_path(), b"new").unwrap();

    cleanup_projections(&home, &old_entry);

    assert_eq!(
        std::fs::read_to_string(projection.startup_committed_path()).unwrap(),
        "new"
    );

    let _ = std::fs::remove_dir_all(home);
}
