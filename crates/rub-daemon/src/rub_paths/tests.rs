use super::{
    RubPaths, default_rub_home, is_temp_owned_home, is_temp_root_path, read_temp_home_owner_pid,
    validate_session_id_component, validate_session_name,
};
use std::path::PathBuf;

#[test]
fn rub_paths_projects_canonical_home_artifacts() {
    let paths = RubPaths::new("/tmp/rub-home");
    assert_eq!(paths.logs_dir(), PathBuf::from("/tmp/rub-home/logs"));
    assert_eq!(
        paths.sessions_dir(),
        PathBuf::from("/tmp/rub-home/sessions")
    );
    assert!(
        paths
            .socket_runtime_dir()
            .to_string_lossy()
            .starts_with("/tmp/rub-sock-"),
        "{}",
        paths.socket_runtime_dir().display()
    );
    assert_eq!(
        paths.workflows_dir(),
        PathBuf::from("/tmp/rub-home/workflows")
    );
    assert_eq!(
        paths.orchestrations_dir(),
        PathBuf::from("/tmp/rub-home/orchestrations")
    );
    assert_eq!(paths.cache_dir(), PathBuf::from("/tmp/rub-home/cache"));
    assert_eq!(paths.history_dir(), PathBuf::from("/tmp/rub-home/history"));
    assert_eq!(
        paths.registry_path(),
        PathBuf::from("/tmp/rub-home/registry.json")
    );
    assert_eq!(
        paths.bindings_path(),
        PathBuf::from("/tmp/rub-home/bindings.json")
    );
    assert_eq!(
        paths.bindings_lock_path(),
        PathBuf::from("/tmp/rub-home/bindings.lock")
    );
    assert_eq!(
        paths.remembered_bindings_path(),
        PathBuf::from("/tmp/rub-home/remembered-bindings.json")
    );
    assert_eq!(
        paths.remembered_bindings_lock_path(),
        PathBuf::from("/tmp/rub-home/remembered-bindings.lock")
    );
    assert_eq!(
        paths.daemon_log_path(),
        PathBuf::from("/tmp/rub-home/logs/daemon.log")
    );
    assert_eq!(
        paths.config_path(),
        PathBuf::from("/tmp/rub-home/config.toml")
    );
    assert_eq!(
        paths.secrets_env_path(),
        PathBuf::from("/tmp/rub-home/secrets.env")
    );
    assert_eq!(
        paths.secrets_env_lock_path(),
        PathBuf::from("/tmp/rub-home/secrets.lock")
    );
}

#[test]
fn session_paths_project_socket_pid_lock_and_ready_files() {
    let session = RubPaths::new("/tmp/rub-home").session("default");
    assert_eq!(
        session.session_dir(),
        PathBuf::from("/tmp/rub-home/sessions/default")
    );
    assert_eq!(
        session.canonical_socket_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/daemon.sock")
    );
    assert_eq!(
        session.socket_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/daemon.sock")
    );
    assert_eq!(
        session.pid_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/daemon.pid")
    );
    assert_eq!(
        session.canonical_pid_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/daemon.pid")
    );
    assert_eq!(
        session.lock_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/startup.lock")
    );
    assert_eq!(
        session.startup_ready_path("abc"),
        PathBuf::from("/tmp/rub-home/sessions/default/startup.abc.ready")
    );
    assert_eq!(
        session.startup_error_path("abc"),
        PathBuf::from("/tmp/rub-home/sessions/default/startup.abc.error")
    );
    assert_eq!(
        session.startup_cleanup_path("abc"),
        PathBuf::from("/tmp/rub-home/sessions/default/startup.abc.cleanup")
    );
    assert_eq!(
        session.startup_committed_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/startup.committed")
    );
    assert_eq!(
        session.post_commit_journal_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/post-commit.journal.ndjson")
    );
    assert_eq!(
        session.download_dir(),
        PathBuf::from("/tmp/rub-home/downloads/default")
    );
    assert_eq!(
        session.socket_paths(),
        vec![PathBuf::from("/tmp/rub-home/sessions/default/daemon.sock")]
    );
    assert_eq!(
        session.actual_socket_paths(),
        vec![PathBuf::from("/tmp/rub-home/sessions/default/daemon.sock")]
    );
    assert_eq!(
        session.pid_paths(),
        vec![PathBuf::from("/tmp/rub-home/sessions/default/daemon.pid")]
    );
    assert_eq!(
        session.lock_paths(),
        vec![PathBuf::from("/tmp/rub-home/sessions/default/startup.lock")]
    );
}

#[test]
fn runtime_session_paths_key_actual_artifacts_by_session_id_while_preserving_name_projections() {
    let session = RubPaths::new("/tmp/rub-home").session_runtime("default", "sess-123");
    assert_eq!(
        session.session_dir(),
        PathBuf::from("/tmp/rub-home/sessions/by-id/sess-123")
    );
    assert_eq!(
        session.projection_dir(),
        PathBuf::from("/tmp/rub-home/sessions/default")
    );
    assert_eq!(
        session.canonical_socket_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/daemon.sock")
    );
    assert!(
        session
            .socket_path()
            .to_string_lossy()
            .starts_with("/tmp/rub-sock-"),
        "{}",
        session.socket_path().display()
    );
    assert_eq!(
        session.pid_path(),
        PathBuf::from("/tmp/rub-home/sessions/by-id/sess-123/daemon.pid")
    );
    assert_eq!(
        session.lock_path(),
        PathBuf::from("/tmp/rub-home/sessions/by-id/sess-123/startup.lock")
    );
    assert_eq!(
        session.startup_ready_path("abc"),
        PathBuf::from("/tmp/rub-home/sessions/by-id/sess-123/startup.abc.ready")
    );
    assert_eq!(
        session.startup_cleanup_path("abc"),
        PathBuf::from("/tmp/rub-home/sessions/by-id/sess-123/startup.abc.cleanup")
    );
    assert_eq!(
        session.startup_committed_path(),
        PathBuf::from("/tmp/rub-home/sessions/default/startup.committed")
    );
    assert_eq!(
        session.post_commit_journal_path(),
        PathBuf::from("/tmp/rub-home/sessions/by-id/sess-123/post-commit.journal.ndjson")
    );
    assert_eq!(
        session.download_dir(),
        PathBuf::from("/tmp/rub-home/downloads/by-id/sess-123")
    );
    assert_eq!(session.actual_socket_paths(), vec![session.socket_path()]);
}

#[test]
fn session_socket_authority_stays_short_under_deep_rub_home() {
    let deep_home = PathBuf::from("/tmp")
        .join("rub-very-deep-home")
        .join("nested")
        .join("structure")
        .join("that")
        .join("would")
        .join("otherwise")
        .join("overflow");
    let session = RubPaths::new(&deep_home).session_runtime("default", "sess-123");
    let socket = session.socket_path();
    assert!(
        socket.to_string_lossy().starts_with("/tmp/rub-sock-"),
        "{}",
        socket.display()
    );
    assert!(
        socket.as_os_str().len() < 100,
        "actual socket authority should stay below Unix socket path limits: {}",
        socket.display()
    );
}

#[test]
fn runtime_socket_authority_ignores_user_environment_drift() {
    let home =
        std::env::temp_dir().join(format!("rub-runtime-socket-env-{}", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let previous_user = std::env::var_os("USER");
    let previous_uid = std::env::var_os("UID");
    unsafe {
        std::env::set_var("USER", "rub-user-a");
        std::env::set_var("UID", "1111");
    }
    let first = RubPaths::new(&home)
        .session_runtime("default", "sess-123")
        .socket_path();
    unsafe {
        std::env::set_var("USER", "rub-user-b");
        std::env::set_var("UID", "2222");
    }
    let second = RubPaths::new(&home)
        .session_runtime("default", "sess-123")
        .socket_path();

    match previous_user {
        Some(value) => unsafe { std::env::set_var("USER", value) },
        None => unsafe { std::env::remove_var("USER") },
    }
    match previous_uid {
        Some(value) => unsafe { std::env::set_var("UID", value) },
        None => unsafe { std::env::remove_var("UID") },
    }

    assert_eq!(
        first, second,
        "runtime socket authority must not drift with mutable USER/UID projection"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn runtime_socket_authority_canonicalizes_existing_home_aliases() {
    let root =
        std::env::temp_dir().join(format!("rub-runtime-socket-alias-{}", uuid::Uuid::now_v7()));
    let real_home = root.join("real-home");
    let alias_home = root.join("alias-home");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&real_home).unwrap();
    std::os::unix::fs::symlink(&real_home, &alias_home).unwrap();

    let real_socket = RubPaths::new(&real_home)
        .session_runtime("default", "sess-123")
        .socket_path();
    let alias_socket = RubPaths::new(&alias_home)
        .session_runtime("default", "sess-123")
        .socket_path();

    assert_eq!(
        real_socket, alias_socket,
        "runtime socket authority must derive from canonical RUB_HOME rather than path spelling"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn runtime_socket_authority_preserves_symlink_semantics_before_dotdot_collapse() {
    let root = std::env::temp_dir().join(format!(
        "rub-runtime-socket-dotdot-{}",
        uuid::Uuid::now_v7()
    ));
    let nested = root.join("actual").join("nested");
    let target_home = root.join("actual").join("home");
    let lexical_home = root.join("home");
    let alias = root.join("alias-nested");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(&target_home).unwrap();
    std::fs::create_dir_all(&lexical_home).unwrap();
    std::os::unix::fs::symlink(&nested, &alias).unwrap();

    let via_symlink_dotdot = alias.join("..").join("home");
    let resolved_socket = RubPaths::new(&via_symlink_dotdot)
        .session_runtime("default", "sess-123")
        .socket_path();
    let target_socket = RubPaths::new(&target_home)
        .session_runtime("default", "sess-123")
        .socket_path();
    let lexical_socket = RubPaths::new(&lexical_home)
        .session_runtime("default", "sess-123")
        .socket_path();

    assert_eq!(
        resolved_socket, target_socket,
        "runtime socket authority must canonicalize through symlinks before applying '..'"
    );
    assert_ne!(
        resolved_socket, lexical_socket,
        "lexical '..' collapse before canonicalization would bind the wrong RUB_HOME authority"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn default_rub_home_prefers_env_and_falls_back_to_home() {
    let previous = std::env::var_os("RUB_HOME");
    unsafe {
        std::env::set_var("RUB_HOME", "/tmp/rub-env-home");
    }
    assert_eq!(default_rub_home(), PathBuf::from("/tmp/rub-env-home"));
    match previous {
        Some(value) => unsafe { std::env::set_var("RUB_HOME", value) },
        None => unsafe { std::env::remove_var("RUB_HOME") },
    }
}

#[test]
fn temp_owned_home_marker_tracks_owner_pid_inside_temp_root() {
    let home = std::env::temp_dir().join(format!("rub-temp-owned-{}", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_dir_all(&home);
    let paths = RubPaths::new(&home);
    assert!(is_temp_root_path(&home));
    assert!(!is_temp_owned_home(&home));
    assert!(paths.mark_temp_home_owner_if_applicable().unwrap());
    assert!(is_temp_owned_home(&home));
    assert_eq!(read_temp_home_owner_pid(&home), Some(std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn canonicalized_private_temp_root_is_still_treated_as_temp_owned() {
    let canonical_temp = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir should canonicalize");
    if canonical_temp == std::env::temp_dir() {
        return;
    }

    let home = canonical_temp.join(format!("rub-temp-owned-{}", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_dir_all(&home);
    let paths = RubPaths::new(&home);

    assert!(is_temp_root_path(&home));
    assert!(paths.mark_temp_home_owner_if_applicable().unwrap());
    assert!(is_temp_owned_home(&home));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn arbitrary_temp_root_is_not_auto_marked_temp_owned() {
    let home =
        std::env::temp_dir().join(format!("rub-runtime-socket-env-{}", uuid::Uuid::now_v7()));
    let _ = std::fs::remove_dir_all(&home);
    let paths = RubPaths::new(&home);

    assert!(is_temp_root_path(&home));
    assert!(!paths.mark_temp_home_owner_if_applicable().unwrap());
    assert!(!is_temp_owned_home(&home));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn invalid_session_names_are_rejected_by_validator() {
    for value in ["../x", "a/b", "/tmp/x", ".", "..", "a\\b"] {
        assert!(validate_session_name(value).is_err(), "{value}");
    }
    assert!(validate_session_name("default").is_ok());
}

#[test]
fn invalid_session_ids_are_rejected_by_validator() {
    for value in ["../x", "a/b", "/tmp/x", ".", "..", "a\\b"] {
        assert!(validate_session_id_component(value).is_err(), "{value}");
    }
    assert!(validate_session_id_component("sess-123").is_ok());
}

#[test]
fn invalid_session_names_do_not_escape_path_namespace() {
    let session = RubPaths::new("/tmp/rub-home").session("../x");
    assert!(
        session
            .projection_dir()
            .starts_with(PathBuf::from("/tmp/rub-home/sessions")),
        "{}",
        session.projection_dir().display()
    );
}

#[test]
fn invalid_runtime_session_ids_do_not_escape_by_id_namespace() {
    let session = RubPaths::new("/tmp/rub-home").session_runtime("default", "../x");
    assert!(
        session
            .session_dir()
            .starts_with(PathBuf::from("/tmp/rub-home/sessions/by-id")),
        "{}",
        session.session_dir().display()
    );
    assert!(
        session
            .download_dir()
            .starts_with(PathBuf::from("/tmp/rub-home/downloads/by-id")),
        "{}",
        session.download_dir().display()
    );
}
