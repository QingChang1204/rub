use super::{
    RegistryData, RegistryEntry, RegistryEntryLiveness, authoritative_entry_by_session_name,
    check_profile_in_use, cleanup_projections, force_busy_registry_socket_probe_once_for_test,
    force_dead_registry_socket_probe_once_for_test, force_live_registry_socket_probe_once_for_test,
    force_probe_contract_failure_registry_socket_probe_once_for_test,
    force_protocol_incompatible_registry_socket_probe_once_for_test,
    is_matching_rub_daemon_command, latest_entry_by_session_name, new_session_id,
    promote_session_authority, read_registry, register_pending_session, register_session,
    registry_authority_snapshot, registry_entry_is_live_for_home,
    registry_entry_is_pending_startup_for_home, write_hard_cut_release_pending_proof,
    write_registry,
};
use crate::rub_paths::RubPaths;
use rub_ipc::protocol::IPC_PROTOCOL_VERSION;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::process::Command;
use uuid::Uuid;

fn temp_home() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rub-registry-test-{}", Uuid::now_v7()))
}

#[cfg(unix)]
fn spawn_synthetic_chrome_profile_holder(
    home: &std::path::Path,
    profile_dir: &std::path::Path,
) -> std::process::Child {
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

fn ensure_socket_path_parent(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
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
fn read_registry_accepts_legacy_runtime_socket_path_for_upgrade_compatibility() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let legacy_socket_path = "/tmp/rub-sock-olduser/0123456789abcdef.sock";
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "default",
                "pid": 1234,
                "socket_path": legacy_socket_path,
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let registry = read_registry(&home).expect("legacy runtime socket path should load");
    assert_eq!(registry.sessions[0].socket_path, legacy_socket_path);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn read_registry_rejects_lookalike_legacy_socket_path_outside_legacy_tmp_root() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let lookalike_socket_path = home.join("rub-sock-olduser/0123456789abcdef.sock");
    std::fs::write(
        home.join("registry.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": [{
                "session_id": "sess-default",
                "session_name": "default",
                "pid": 1234,
                "socket_path": lookalike_socket_path,
                "created_at": "2026-03-31T00:00:00Z",
                "ipc_protocol_version": "1.0",
                "user_data_dir": null
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let error = read_registry(&home)
        .expect_err("lookalike legacy socket outside /tmp/rub-sock-<tag> should be rejected");
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
    ensure_socket_path_parent(&runtime.socket_path());
    std::fs::write(runtime.socket_path(), b"socket").unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    force_live_registry_socket_probe_once_for_test(&runtime.socket_path());

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
    ensure_socket_path_parent(&runtime.socket_path());
    std::fs::write(runtime.socket_path(), b"socket").unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    force_dead_registry_socket_probe_once_for_test(&runtime.socket_path());

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
    ensure_socket_path_parent(&runtime.socket_path());
    std::fs::write(runtime.socket_path(), b"socket").unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    force_protocol_incompatible_registry_socket_probe_once_for_test(&runtime.socket_path());

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![RegistryEntry {
                session_id: session_id.to_string(),
                session_name: session_name.to_string(),
                pid: std::process::id(),
                socket_path: runtime.socket_path().display().to_string(),
                created_at: "2026-04-02T00:00:00Z".to_string(),
                ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            }],
        },
    )
    .unwrap();

    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session_snapshot = snapshot.session(session_name).expect("session snapshot");
    let latest_entry = session_snapshot.latest_entry().expect("latest entry");
    assert!(latest_entry.is_protocol_incompatible_authority());
    let entry_snapshot = session_snapshot
        .authoritative_entry()
        .expect("protocol-incompatible entry should remain authoritative");
    assert!(entry_snapshot.is_protocol_incompatible_authority());
    let active = snapshot.active_entry_snapshots();
    assert_eq!(active.len(), 1);
    assert_eq!(
        active[0].liveness,
        RegistryEntryLiveness::ProtocolIncompatible
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn live_registry_identity_rejects_payload_and_protocol_echo_divergence() {
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
    ensure_socket_path_parent(&runtime.socket_path());
    std::fs::write(runtime.socket_path(), b"socket").unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    force_probe_contract_failure_registry_socket_probe_once_for_test(&runtime.socket_path());

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

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![entry.clone()],
        },
    )
    .unwrap();
    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session_snapshot = snapshot.session(session_name).expect("session snapshot");
    let entry_snapshot = session_snapshot
        .authoritative_entry()
        .expect("probe-contract-failure entry should remain authoritative");
    assert!(entry_snapshot.is_probe_contract_failure_authority());
    let active = snapshot.active_entry_snapshots();
    assert_eq!(active.len(), 1);
    assert_eq!(
        active[0].liveness,
        RegistryEntryLiveness::ProbeContractFailure
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[cfg(unix)]
fn live_registry_identity_requires_matching_handshake_command_id() {
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
    ensure_socket_path_parent(&runtime.socket_path());
    std::fs::write(runtime.socket_path(), b"socket").unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    force_probe_contract_failure_registry_socket_probe_once_for_test(&runtime.socket_path());

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

    write_registry(
        &home,
        &RegistryData {
            sessions: vec![entry.clone()],
        },
    )
    .unwrap();
    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session_snapshot = snapshot.session(session_name).expect("session snapshot");
    let entry_snapshot = session_snapshot
        .authoritative_entry()
        .expect("probe-contract-failure entry should remain authoritative");
    assert!(entry_snapshot.is_probe_contract_failure_authority());
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
    ensure_socket_path_parent(&runtime.socket_path());
    std::fs::write(runtime.socket_path(), b"socket").unwrap();
    symlink(runtime.socket_path(), projection.canonical_socket_path()).unwrap();
    force_busy_registry_socket_probe_once_for_test(&runtime.socket_path());

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
    ensure_socket_path_parent(&runtime.socket_path());
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
#[cfg(unix)]
fn hard_cut_release_pending_remains_authoritative_and_blocks_profile_reuse() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-hard-cut";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let profile_dir = rub_cdp::projected_managed_profile_path_for_session(session_id);
    std::fs::create_dir_all(&profile_dir).unwrap();

    let mut child = spawn_synthetic_chrome_profile_holder(&home, &profile_dir);

    let attachment_identity = format!("user_data_dir:{}", profile_dir.display());

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
                attachment_identity: Some(attachment_identity.clone()),
                connection_target: None,
            }],
        },
    )
    .unwrap();
    write_hard_cut_release_pending_proof(
        &home,
        session_name,
        &super::HardCutReleasePendingProof {
            session_id: session_id.to_string(),
        },
    )
    .unwrap();

    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session = snapshot.session(session_name).expect("session snapshot");
    let authority = session
        .authoritative_entry()
        .expect("hard-cut release pending must remain authoritative");
    assert!(authority.is_hard_cut_release_pending_authority());
    assert_eq!(authority.entry.session_id, session_id);

    assert_eq!(
        check_profile_in_use(&home, &attachment_identity, None).unwrap(),
        Some(session_name.to_string())
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(profile_dir);
}

#[test]
#[cfg(unix)]
fn malformed_hard_cut_release_pending_proof_still_blocks_profile_reuse() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-hard-cut-malformed";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let projection = RubPaths::new(&home).session(session_name);
    let profile_dir = rub_cdp::projected_managed_profile_path_for_session(session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(projection.projection_dir()).unwrap();
    std::fs::create_dir_all(&profile_dir).unwrap();

    let mut child = spawn_synthetic_chrome_profile_holder(&home, &profile_dir);

    let attachment_identity = format!("user_data_dir:{}", profile_dir.display());
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
                attachment_identity: Some(attachment_identity.clone()),
                connection_target: None,
            }],
        },
    )
    .unwrap();
    std::fs::write(projection.hard_cut_release_pending_path(), b"{not-json").unwrap();

    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session = snapshot.session(session_name).expect("session snapshot");
    let authority = session
        .authoritative_entry()
        .expect("malformed hard-cut fallback proof must still remain authoritative");
    assert!(authority.is_hard_cut_release_pending_authority());

    assert_eq!(
        check_profile_in_use(&home, &attachment_identity, None).unwrap(),
        Some(session_name.to_string())
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(profile_dir);
}

#[test]
#[cfg(unix)]
fn hard_cut_release_pending_profile_observation_failure_still_blocks_authority() {
    let home = temp_home();
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let session_name = "default";
    let session_id = "sess-hard-cut-observe";
    let runtime = RubPaths::new(&home).session_runtime(session_name, session_id);
    let profile_dir = rub_cdp::projected_managed_profile_path_for_session(session_id);
    std::fs::create_dir_all(runtime.session_dir()).unwrap();
    std::fs::create_dir_all(&profile_dir).unwrap();

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
    write_hard_cut_release_pending_proof(
        &home,
        session_name,
        &super::HardCutReleasePendingProof {
            session_id: session_id.to_string(),
        },
    )
    .unwrap();

    super::force_hard_cut_release_pending_profile_observation_failure_for_test();
    let snapshot = registry_authority_snapshot(&home).unwrap();
    let session = snapshot.session(session_name).expect("session snapshot");
    let authority = session
        .authoritative_entry()
        .expect("profile observation failure must fail closed onto hard-cut fallback authority");
    assert!(authority.is_hard_cut_release_pending_authority());

    let _ = std::fs::remove_dir_all(home);
    let _ = std::fs::remove_dir_all(profile_dir);
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
    ensure_socket_path_parent(&live_runtime.socket_path());
    std::fs::write(live_runtime.socket_path(), b"socket").unwrap();
    force_live_registry_socket_probe_once_for_test(&live_runtime.socket_path());

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
