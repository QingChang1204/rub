use std::path::Path;

use crate::rub_paths::RubPaths;

use super::RegistryEntry;

pub fn cleanup_projections(home: &Path, entry: &RegistryEntry) {
    let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
    let projection = RubPaths::new(home).session(&entry.session_name);

    for path in [
        runtime.socket_path(),
        runtime.pid_path(),
        runtime.lock_path(),
        runtime.post_commit_journal_path(),
    ] {
        let _ = std::fs::remove_file(path);
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

    cleanup_socket_projection_if_matches(
        &projection.canonical_socket_path(),
        &runtime.socket_path(),
    );
    cleanup_pid_projection_if_matches(&projection.canonical_pid_path(), entry.pid);
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
}
