use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::{fs, io};

/// Returns a stable, per-OS-user identity string used to scope the runtime
/// socket directory so that multiple accounts on the same machine never
/// share a socket directory (which would cause `Permission denied` for
/// whichever account didn't create it first).
///
/// `$USER` is set by the OS for every login session on macOS and Linux and
/// is the idiomatic way to obtain the current username in safe Rust without
/// an `unsafe` block or an additional dependency.
fn current_user_tag() -> String {
    std::env::var("USER").unwrap_or_else(|_| {
        // Fallback: if USER is somehow unset (e.g. bare daemon spawn without
        // a login shell), use the effective UID so we always produce a valid,
        // non-colliding directory name.
        std::env::var("UID").unwrap_or_else(|_| "unknown".to_string())
    })
}

/// Canonical `RUB_HOME` path authority for the current baseline layout.
#[derive(Debug, Clone)]
pub struct RubPaths {
    home: PathBuf,
}

/// Session-scoped artifact paths under `RUB_HOME`.
#[derive(Debug, Clone)]
pub struct SessionPaths {
    home: PathBuf,
    session_name: String,
    session_id: Option<String>,
}

impl RubPaths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }

    pub fn socket_runtime_dir(&self) -> PathBuf {
        // Scope the runtime socket directory to the current OS user so that
        // multiple accounts on the same machine (e.g. liuqingchang and
        // qingchang) each own their own directory.  Without this, whichever
        // account runs `rub` first creates /tmp/rub-sock with mode 755, and
        // all other accounts get `Permission denied` when they try to create
        // their own sockets inside it.
        PathBuf::from(format!("/tmp/rub-sock-{}", current_user_tag()))
    }

    pub fn workflows_dir(&self) -> PathBuf {
        self.home.join("workflows")
    }

    pub fn orchestrations_dir(&self) -> PathBuf {
        self.home.join("orchestrations")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.home.join("cache")
    }

    pub fn history_dir(&self) -> PathBuf {
        self.home.join("history")
    }

    pub fn registry_path(&self) -> PathBuf {
        self.home.join("registry.json")
    }

    pub fn registry_lock_path(&self) -> PathBuf {
        self.home.join("registry.lock")
    }

    pub fn daemon_log_path(&self) -> PathBuf {
        self.logs_dir().join("daemon.log")
    }

    pub fn temp_home_owner_marker_path(&self) -> PathBuf {
        self.home.join(".rub-temp-owned")
    }

    pub fn mark_temp_home_owner_if_applicable(&self) -> io::Result<bool> {
        if !is_temp_root_path(&self.home) {
            return Ok(false);
        }
        fs::create_dir_all(&self.home)?;
        fs::write(
            self.temp_home_owner_marker_path(),
            std::process::id().to_string(),
        )?;
        Ok(true)
    }

    pub fn config_path(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn secrets_env_path(&self) -> PathBuf {
        self.home.join("secrets.env")
    }

    pub fn downloads_root(&self) -> PathBuf {
        self.home.join("downloads")
    }

    pub fn startup_locks_dir(&self) -> PathBuf {
        self.home.join("startup-locks")
    }

    pub fn startup_lock_path(&self, scope_key: &str) -> PathBuf {
        self.startup_locks_dir()
            .join(format!("{}.lock", safe_session_path_component(scope_key)))
    }

    pub fn session(&self, session_name: impl Into<String>) -> SessionPaths {
        SessionPaths::new(self.home.clone(), session_name.into(), None)
    }

    pub fn session_runtime(
        &self,
        session_name: impl Into<String>,
        session_id: impl Into<String>,
    ) -> SessionPaths {
        SessionPaths::new(
            self.home.clone(),
            session_name.into(),
            Some(session_id.into()),
        )
    }
}

pub fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![std::env::temp_dir()];
    let explicit_tmp = PathBuf::from("/tmp");
    if !roots.iter().any(|root| root == &explicit_tmp) {
        roots.push(explicit_tmp);
    }
    roots
}

pub fn is_temp_root_path(path: &Path) -> bool {
    temp_roots().into_iter().any(|root| path.starts_with(root))
}

pub fn is_temp_owned_home(path: &Path) -> bool {
    is_temp_root_path(path) && RubPaths::new(path).temp_home_owner_marker_path().exists()
}

pub fn read_temp_home_owner_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(RubPaths::new(path).temp_home_owner_marker_path()).ok()?;
    raw.trim().parse::<u32>().ok()
}

impl SessionPaths {
    fn new(home: PathBuf, session_name: String, session_id: Option<String>) -> Self {
        Self {
            home,
            session_name,
            session_id,
        }
    }

    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn session_dir(&self) -> PathBuf {
        match self.session_id.as_deref() {
            Some(session_id) => self
                .home
                .join("sessions")
                .join("by-id")
                .join(safe_session_id_path_component(session_id)),
            None => self.home.join("sessions").join(self.session_component()),
        }
    }

    pub fn projection_dir(&self) -> PathBuf {
        self.home.join("sessions").join(self.session_component())
    }

    pub fn canonical_socket_path(&self) -> PathBuf {
        self.projection_dir().join("daemon.sock")
    }

    pub fn actual_socket_path(&self) -> PathBuf {
        match self.session_id.as_deref() {
            Some(_) => RubPaths::new(&self.home)
                .socket_runtime_dir()
                .join(format!("{}.sock", self.socket_runtime_key())),
            None => self.canonical_socket_path(),
        }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.actual_socket_path()
    }

    pub fn socket_projection_paths(&self) -> Vec<PathBuf> {
        dedup_paths([self.canonical_socket_path()])
    }

    pub fn actual_socket_paths(&self) -> Vec<PathBuf> {
        dedup_paths([self.actual_socket_path()])
    }

    pub fn socket_paths(&self) -> Vec<PathBuf> {
        dedup_paths([self.actual_socket_path(), self.canonical_socket_path()])
    }

    pub fn pid_path(&self) -> PathBuf {
        self.session_dir().join("daemon.pid")
    }

    pub fn canonical_pid_path(&self) -> PathBuf {
        self.projection_dir().join("daemon.pid")
    }

    pub fn pid_paths(&self) -> Vec<PathBuf> {
        dedup_paths([self.pid_path(), self.canonical_pid_path()])
    }

    pub fn lock_path(&self) -> PathBuf {
        self.session_dir().join("startup.lock")
    }

    pub fn lock_paths(&self) -> Vec<PathBuf> {
        dedup_paths([self.lock_path()])
    }

    pub fn startup_ready_path(&self, startup_id: &str) -> PathBuf {
        self.session_dir()
            .join(format!("startup.{startup_id}.ready"))
    }

    pub fn startup_error_path(&self, startup_id: &str) -> PathBuf {
        self.session_dir()
            .join(format!("startup.{startup_id}.error"))
    }

    pub fn startup_committed_path(&self) -> PathBuf {
        self.projection_dir().join("startup.committed")
    }

    pub fn post_commit_journal_path(&self) -> PathBuf {
        self.session_dir().join("post-commit.journal.ndjson")
    }

    pub fn download_dir(&self) -> PathBuf {
        match self.session_id.as_deref() {
            Some(session_id) => self
                .home
                .join("downloads")
                .join("by-id")
                .join(safe_session_id_path_component(session_id)),
            None => self.home.join("downloads").join(self.session_component()),
        }
    }

    fn socket_runtime_key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.home.hash(&mut hasher);
        match self.session_id.as_deref() {
            Some(session_id) => session_id.hash(&mut hasher),
            None => self.session_name.hash(&mut hasher),
        }
        format!("{:016x}", hasher.finish())
    }

    fn session_component(&self) -> String {
        safe_session_path_component(&self.session_name)
    }

    /// Actual socket paths that currently exist on disk.
    /// These are runtime-presence queries; callers use this to detect live daemons.
    pub fn existing_socket_paths(&self) -> Vec<PathBuf> {
        self.actual_socket_paths()
            .into_iter()
            .filter(|p| p.exists())
            .collect()
    }

    /// PID file paths that currently exist on disk.
    pub fn existing_pid_paths(&self) -> Vec<PathBuf> {
        self.pid_paths()
            .into_iter()
            .filter(|p| p.exists())
            .collect()
    }
}

pub fn validate_session_name(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("session name must not be empty".to_string());
    }
    if value.contains('/') || value.contains('\\') {
        return Err("session name must not contain path separators".to_string());
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err("session name must not be an absolute path".to_string());
    }
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err("session name must be a single path component without '.' or '..'".to_string()),
    }
}

pub fn validate_session_id_component(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("session id must not be empty".to_string());
    }
    if value.contains('/') || value.contains('\\') {
        return Err("session id must not contain path separators".to_string());
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err("session id must not be an absolute path".to_string());
    }
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err("session id must be a single path component without '.' or '..'".to_string()),
    }
}

fn safe_session_path_component(session_name: &str) -> String {
    if validate_session_name(session_name).is_ok() {
        return session_name.to_string();
    }
    let mut hasher = DefaultHasher::new();
    session_name.hash(&mut hasher);
    format!("invalid-session-{:016x}", hasher.finish())
}

fn safe_session_id_path_component(session_id: &str) -> String {
    if validate_session_id_component(session_id).is_ok() {
        return session_id.to_string();
    }
    let mut hasher = DefaultHasher::new();
    session_id.hash(&mut hasher);
    format!("invalid-session-id-{:016x}", hasher.finish())
}

fn dedup_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if unique.iter().all(|existing| existing != &path) {
            unique.push(path);
        }
    }
    unique
}

pub fn default_rub_home() -> PathBuf {
    if let Ok(dir) = std::env::var("RUB_HOME") {
        PathBuf::from(dir)
    } else {
        std::env::var("HOME")
            .map(|home| PathBuf::from(home).join(".rub"))
            .unwrap_or_else(|_| PathBuf::from("/tmp/.rub"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RubPaths, default_rub_home, is_temp_owned_home, is_temp_root_path,
        read_temp_home_owner_pid, validate_session_id_component, validate_session_name,
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
        // Socket runtime dir is now user-scoped; verify it contains the
        // current username (or UID fallback) rather than a hardcoded string.
        let tag = std::env::var("USER")
            .or_else(|_| std::env::var("UID"))
            .unwrap_or_else(|_| "unknown".to_string());
        assert_eq!(
            paths.socket_runtime_dir(),
            PathBuf::from(format!("/tmp/rub-sock-{tag}"))
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
    fn runtime_session_paths_key_actual_artifacts_by_session_id_while_preserving_name_projections()
    {
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
        let tag = std::env::var("USER")
            .or_else(|_| std::env::var("UID"))
            .unwrap_or_else(|_| "unknown".to_string());
        assert!(
            session
                .socket_path()
                .starts_with(format!("/tmp/rub-sock-{tag}"))
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
        let tag = std::env::var("USER")
            .or_else(|_| std::env::var("UID"))
            .unwrap_or_else(|_| "unknown".to_string());
        assert!(socket.starts_with(format!("/tmp/rub-sock-{tag}")));
        assert!(
            socket.as_os_str().len() < 100,
            "actual socket authority should stay below Unix socket path limits: {}",
            socket.display()
        );
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
}
