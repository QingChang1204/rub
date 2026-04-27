use std::path::{Path, PathBuf};

use crate::rub_paths::RubPaths;

use super::{RegistryEntry, validation::is_legacy_runtime_socket_path};

pub fn cleanup_projections(home: &Path, entry: &RegistryEntry) {
    let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
    let projection = RubPaths::new(home).session(&entry.session_name);
    let runtime_socket_path = runtime.socket_path();
    let entry_socket_path = PathBuf::from(&entry.socket_path);

    for path in [
        runtime_socket_path.clone(),
        entry_socket_path.clone(),
        runtime.pid_path(),
        runtime.lock_path(),
        runtime.post_commit_journal_path(),
    ] {
        if path != entry_socket_path
            || path == runtime_socket_path
            || is_legacy_runtime_socket_path(&path)
        {
            let _ = std::fs::remove_file(path);
        }
    }

    if let Ok(entries) = std::fs::read_dir(runtime.session_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("startup.")
                && (name.ends_with(".ready") || name.ends_with(".error"))
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let _ = std::fs::remove_dir(runtime.session_dir());

    cleanup_socket_projection_if_matches(&projection.canonical_socket_path(), &runtime_socket_path);
    cleanup_socket_projection_if_matches(&projection.canonical_socket_path(), &entry_socket_path);
    cleanup_pid_projection_if_matches(&projection.canonical_pid_path(), entry.pid);
    cleanup_hard_cut_release_pending_if_matches(
        &projection.hard_cut_release_pending_path(),
        &entry.session_id,
    );
    cleanup_startup_commit_marker_if_matches(
        &projection.startup_committed_path(),
        &entry.session_id,
    );

    if projection.projection_dir() != runtime.session_dir() {
        let _ = std::fs::remove_dir(projection.projection_dir());
    }
}

fn cleanup_startup_commit_marker_if_matches(path: &Path, session_id: &str) {
    let matches_entry = std::fs::read_to_string(path)
        .ok()
        .is_some_and(|current| current.trim() == session_id);
    if matches_entry {
        let _ = std::fs::remove_file(path);
    }
}

fn cleanup_socket_projection_if_matches(path: &Path, actual_socket: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    #[cfg(unix)]
    {
        if metadata.file_type().is_symlink()
            && std::fs::read_link(path).ok().as_deref() == Some(actual_socket)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn cleanup_pid_projection_if_matches(path: &Path, pid: u32) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    if contents.trim() == pid.to_string() {
        let _ = std::fs::remove_file(path);
    }
}

fn cleanup_hard_cut_release_pending_if_matches(path: &Path, session_id: &str) {
    let Ok(raw) = std::fs::read(path) else {
        return;
    };
    let Ok(proof) =
        serde_json::from_slice::<crate::session::registry::HardCutReleasePendingProof>(&raw)
    else {
        return;
    };
    if proof.session_id == session_id {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::cleanup_projections;
    use crate::session::registry::RegistryEntry;

    #[test]
    fn cleanup_projections_removes_runtime_dir_even_with_post_commit_journal() {
        let home = std::env::temp_dir().join(format!(
            "rub-registry-projection-cleanup-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);

        let entry = RegistryEntry {
            session_id: "sess-123".to_string(),
            session_name: "default".to_string(),
            pid: 123,
            socket_path: "/tmp/rub.sock".to_string(),
            created_at: "2026-04-10T00:00:00Z".to_string(),
            ipc_protocol_version: "1".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let runtime = crate::rub_paths::RubPaths::new(&home)
            .session_runtime(&entry.session_name, &entry.session_id);
        std::fs::create_dir_all(runtime.session_dir()).expect("create runtime dir");
        std::fs::write(
            runtime.post_commit_journal_path(),
            b"{\"command\":\"open\"}\n",
        )
        .expect("write journal");

        cleanup_projections(&home, &entry);

        assert!(!runtime.session_dir().exists());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cleanup_projections_removes_legacy_runtime_socket_path() {
        let home = std::env::temp_dir().join(format!(
            "rub-registry-projection-cleanup-legacy-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let legacy_socket_dir = std::path::PathBuf::from("/tmp")
            .join(format!("rub-sock-cleanup-{}", uuid::Uuid::now_v7()));
        let legacy_socket_path = legacy_socket_dir.join("0123456789abcdef.sock");
        let _ = std::fs::remove_dir_all(&legacy_socket_dir);
        std::fs::create_dir_all(&legacy_socket_dir).expect("create legacy socket dir");
        std::fs::write(&legacy_socket_path, b"socket").expect("write legacy socket");

        let entry = RegistryEntry {
            session_id: "sess-123".to_string(),
            session_name: "default".to_string(),
            pid: 123,
            socket_path: legacy_socket_path.display().to_string(),
            created_at: "2026-04-10T00:00:00Z".to_string(),
            ipc_protocol_version: "1".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        cleanup_projections(&home, &entry);

        assert!(
            !legacy_socket_path.exists(),
            "legacy runtime socket authority should be cleaned through the recorded registry path"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&legacy_socket_dir);
    }

    #[test]
    fn cleanup_projections_does_not_remove_lookalike_non_legacy_socket_path() {
        let home = std::env::temp_dir().join(format!(
            "rub-registry-projection-cleanup-lookalike-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let lookalike_socket_dir = home.join("rub-sock-legacy");
        let lookalike_socket_path = lookalike_socket_dir.join("0123456789abcdef.sock");
        std::fs::create_dir_all(&lookalike_socket_dir).expect("create lookalike socket dir");
        std::fs::write(&lookalike_socket_path, b"socket").expect("write lookalike socket");

        let entry = RegistryEntry {
            session_id: "sess-123".to_string(),
            session_name: "default".to_string(),
            pid: 123,
            socket_path: lookalike_socket_path.display().to_string(),
            created_at: "2026-04-10T00:00:00Z".to_string(),
            ipc_protocol_version: "1".to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        cleanup_projections(&home, &entry);

        assert!(
            lookalike_socket_path.exists(),
            "cleanup must not treat arbitrary rub-sock-shaped paths as legacy runtime socket authority"
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
