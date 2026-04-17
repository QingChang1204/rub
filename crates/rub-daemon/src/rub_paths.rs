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

    pub fn bindings_path(&self) -> PathBuf {
        self.home.join("bindings.json")
    }

    pub fn bindings_lock_path(&self) -> PathBuf {
        self.home.join("bindings.lock")
    }

    pub fn remembered_bindings_path(&self) -> PathBuf {
        self.home.join("remembered-bindings.json")
    }

    pub fn remembered_bindings_lock_path(&self) -> PathBuf {
        self.home.join("remembered-bindings.lock")
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

    pub fn secrets_env_lock_path(&self) -> PathBuf {
        self.home.join("secrets.lock")
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
    let mut roots = Vec::new();
    for root in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        push_temp_root_variant(&mut roots, root.clone());
        if let Ok(canonical) = root.canonicalize() {
            push_temp_root_variant(&mut roots, canonical);
        }
    }
    roots
}

pub fn is_temp_root_path(path: &Path) -> bool {
    let mut candidates = vec![path.to_path_buf()];
    if let Some(alias) = strip_private_prefix(path) {
        candidates.push(alias);
    }

    temp_roots().into_iter().any(|root| {
        candidates
            .iter()
            .any(|candidate| candidate.starts_with(&root))
    })
}

pub fn is_temp_owned_home(path: &Path) -> bool {
    is_temp_root_path(path) && RubPaths::new(path).temp_home_owner_marker_path().exists()
}

pub fn read_temp_home_owner_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(RubPaths::new(path).temp_home_owner_marker_path()).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn push_temp_root_variant(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.iter().any(|existing| existing == &root) {
        roots.push(root.clone());
    }
    if let Some(alias) = strip_private_prefix(&root)
        && !roots.iter().any(|existing| existing == &alias)
    {
        roots.push(alias);
    }
}

fn strip_private_prefix(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::RootDir), Some(Component::Normal(component)))
            if component == "private" =>
        {
            let mut stripped = PathBuf::from("/");
            for component in components {
                stripped.push(component.as_os_str());
            }
            Some(stripped)
        }
        _ => None,
    }
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

    pub fn startup_stderr_path(&self, startup_id: &str) -> PathBuf {
        self.session_dir()
            .join(format!("startup.{startup_id}.stderr"))
    }

    pub fn startup_cleanup_path(&self, startup_id: &str) -> PathBuf {
        self.session_dir()
            .join(format!("startup.{startup_id}.cleanup"))
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
mod tests;
