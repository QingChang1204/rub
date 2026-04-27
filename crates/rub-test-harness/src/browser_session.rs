mod cleanup;
mod external_chrome;
mod profile_env;

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, Once, OnceLock};

pub use self::cleanup::{
    browser_processes_for_daemon_pid, cleanup, daemon_pid_matches_home_in_snapshot,
    daemon_processes_for_home, default_session_pid_path, e2e_home_owner_pid, observe_home_cleanup,
    prepare_home, session_pid_path, verify_home_cleanup_complete, wait_for_home_processes_to_exit,
};
pub use self::external_chrome::{
    external_chrome_pid_matches_profile_in_snapshot, spawn_external_chrome,
    terminate_external_chrome, verify_external_chrome_cleanup_complete, wait_until,
};
pub use self::profile_env::prepare_fake_profile_env;

static CLEANUP_HOOK: Once = Once::new();
static REGISTERED_HOMES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static REGISTERED_EXTERNAL_CHROMES: OnceLock<Mutex<Vec<(u32, PathBuf)>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct HomeCleanupObservation {
    pub daemon_root_pids: Vec<u32>,
    pub managed_profile_dirs: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupVerification {
    Verified,
    VerifiedWithHarnessFallback,
    SkippedDuringPanic,
}

impl CleanupVerification {
    pub fn product_teardown_verified(self) -> bool {
        matches!(self, Self::Verified)
    }

    pub fn used_harness_fallback(self) -> bool {
        matches!(self, Self::VerifiedWithHarnessFallback)
    }
}

pub fn registered_homes() -> &'static Mutex<Vec<String>> {
    REGISTERED_HOMES.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn registered_external_chromes() -> &'static Mutex<Vec<(u32, PathBuf)>> {
    REGISTERED_EXTERNAL_CHROMES.get_or_init(|| Mutex::new(Vec::new()))
}

fn install_cleanup_hook() {
    CLEANUP_HOOK.call_once(|| {
        cleanup::sweep_stale_test_homes();
        register_process_exit_cleanup();
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            cleanup_registered_artifacts();
            previous(info);
        }));
    });
}

pub fn register_home(home: &str) {
    install_cleanup_hook();
    let mut homes = registered_homes().lock().unwrap();
    if !homes.iter().any(|existing| existing == home) {
        homes.push(home.to_string());
    }
}

pub fn unregister_home(home: &str) {
    let mut homes = registered_homes().lock().unwrap();
    homes.retain(|existing| existing != home);
}

fn register_external_chrome(pid: u32, profile_dir: &Path) {
    install_cleanup_hook();
    let mut entries = registered_external_chromes().lock().unwrap();
    if !entries.iter().any(|(existing_pid, _)| *existing_pid == pid) {
        entries.push((pid, profile_dir.to_path_buf()));
    }
}

fn unregister_external_chrome(pid: u32) {
    let mut entries = registered_external_chromes().lock().unwrap();
    entries.retain(|(existing_pid, _)| *existing_pid != pid);
}

fn cleanup_registered_artifacts_with<F, G>(
    homes: Vec<String>,
    external: Vec<(u32, PathBuf)>,
    mut cleanup_home: F,
    mut cleanup_external: G,
) -> (Vec<String>, Vec<(u32, PathBuf)>)
where
    F: FnMut(&str) -> Result<CleanupVerification, String>,
    G: FnMut(u32, &Path) -> Result<CleanupVerification, String>,
{
    let mut failed_homes = Vec::new();
    for home in homes {
        match cleanup_home(&home) {
            Ok(CleanupVerification::Verified) => {}
            Ok(CleanupVerification::VerifiedWithHarnessFallback) => {
                failed_homes.push(home);
            }
            Ok(CleanupVerification::SkippedDuringPanic) | Err(_) => {
                failed_homes.push(home);
            }
        }
    }

    let mut failed_external = Vec::new();
    for (pid, profile_dir) in external {
        match cleanup_external(pid, &profile_dir) {
            Ok(CleanupVerification::Verified) => {}
            Ok(CleanupVerification::VerifiedWithHarnessFallback) => {
                failed_external.push((pid, profile_dir));
            }
            Ok(CleanupVerification::SkippedDuringPanic) | Err(_) => {
                failed_external.push((pid, profile_dir));
            }
        }
    }

    (failed_homes, failed_external)
}

pub fn cleanup_registered_artifacts() {
    let homes = registered_homes().lock().unwrap().clone();
    let external = registered_external_chromes().lock().unwrap().clone();
    let (failed_homes, failed_external) = cleanup_registered_artifacts_with(
        homes,
        external,
        cleanup::try_cleanup_home_allow_harness_fallback,
        external_chrome::try_cleanup_external_chrome,
    );

    *registered_homes().lock().unwrap() = failed_homes;
    *registered_external_chromes().lock().unwrap() = failed_external;
}

#[cfg(unix)]
extern "C" fn cleanup_registered_artifacts_atexit() {
    cleanup_registered_artifacts();
}

#[cfg(unix)]
fn register_process_exit_cleanup() {
    unsafe {
        libc::atexit(cleanup_registered_artifacts_atexit);
    }
}

#[cfg(not(unix))]
fn register_process_exit_cleanup() {}

#[cfg(unix)]
pub fn write_secure_secrets_env(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
pub fn write_secure_secrets_env(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

pub fn resolve_test_rub_binary_path(
    explicit_test_binary: Option<&str>,
    cargo_bin_exe: Option<&str>,
    manifest_dir: Option<&str>,
) -> String {
    if let Some(path) = explicit_test_binary
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return path.to_string();
    }
    if let Some(path) = cargo_bin_exe.map(str::trim).filter(|path| !path.is_empty()) {
        return path.to_string();
    }
    let manifest = manifest_dir.unwrap_or_default();
    let workspace = std::path::Path::new(&manifest)
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(std::path::Path::new("."));
    let path = workspace.join("target/debug/rub");
    path.to_string_lossy().to_string()
}

pub(super) fn rub_binary() -> String {
    resolve_test_rub_binary_path(
        std::env::var("RUB_TEST_BINARY").ok().as_deref(),
        std::env::var("CARGO_BIN_EXE_rub").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
    )
}

pub(super) fn rub_cmd(rub_home: &str) -> Command {
    let mut cmd = Command::new(rub_binary());
    cmd.arg("--rub-home").arg(rub_home);
    cmd
}

pub fn unique_home() -> String {
    install_cleanup_hook();
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir());
    let home = temp_root
        .join(format!(
            "rub-temp-owned-e2e-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
        .to_string_lossy()
        .to_string();
    register_home(&home);
    home
}

fn acquire_managed_browser_lock() -> File {
    let lock_path = std::env::temp_dir().join("rub-e2e-browser.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|error| panic!("failed to open browser lock {:?}: {error}", lock_path));
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(
        rc,
        0,
        "failed to acquire browser lock {:?}: {}",
        lock_path,
        std::io::Error::last_os_error()
    );
    file
}

pub struct ManagedBrowserSession {
    home: String,
    _browser_lock: File,
}

impl Default for ManagedBrowserSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ManagedBrowserSession {
    pub fn new() -> Self {
        let browser_lock = acquire_managed_browser_lock();
        let home = unique_home();
        prepare_home(&home);
        Self {
            home,
            _browser_lock: browser_lock,
        }
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    pub fn cmd(&self) -> Command {
        rub_cmd(self.home())
    }
}

impl Drop for ManagedBrowserSession {
    fn drop(&mut self) {
        cleanup(&self.home);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CleanupVerification, cleanup_registered_artifacts_with, resolve_test_rub_binary_path,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn resolve_test_rub_binary_path_prefers_explicit_test_binary() {
        let resolved = resolve_test_rub_binary_path(
            Some("/tmp/target/release/rub"),
            Some("/tmp/target/debug/rub"),
            Some("/workspace/crates/rub-test-harness"),
        );
        assert_eq!(resolved, "/tmp/target/release/rub");
    }

    #[test]
    fn resolve_test_rub_binary_path_falls_back_to_cargo_bin_exe() {
        let resolved = resolve_test_rub_binary_path(
            None,
            Some("/tmp/target/debug/rub"),
            Some("/workspace/crates/rub-test-harness"),
        );
        assert_eq!(resolved, "/tmp/target/debug/rub");
    }

    #[test]
    fn registered_artifact_cleanup_attempts_best_effort_work_even_for_skipped_panic_verification() {
        let home_calls = AtomicUsize::new(0);
        let external_calls = AtomicUsize::new(0);
        let homes = vec!["/tmp/rub-home-a".to_string()];
        let external = vec![(41, std::path::PathBuf::from("/tmp/rub-profile-a"))];

        let (failed_homes, failed_external) = cleanup_registered_artifacts_with(
            homes.clone(),
            external.clone(),
            |_: &str| {
                home_calls.fetch_add(1, Ordering::SeqCst);
                Ok(CleanupVerification::SkippedDuringPanic)
            },
            |_: u32, _: &Path| {
                external_calls.fetch_add(1, Ordering::SeqCst);
                Ok(CleanupVerification::SkippedDuringPanic)
            },
        );

        assert_eq!(home_calls.load(Ordering::SeqCst), 1);
        assert_eq!(external_calls.load(Ordering::SeqCst), 1);
        assert_eq!(failed_homes, homes);
        assert_eq!(failed_external, external);
    }

    #[test]
    fn registered_artifact_cleanup_retains_harness_fallback_for_retry_tracking() {
        let homes = vec!["/tmp/rub-home-a".to_string()];
        let external = vec![(41, std::path::PathBuf::from("/tmp/rub-profile-a"))];

        let (failed_homes, failed_external) = cleanup_registered_artifacts_with(
            homes.clone(),
            external.clone(),
            |_: &str| Ok(CleanupVerification::VerifiedWithHarnessFallback),
            |_: u32, _: &Path| Ok(CleanupVerification::VerifiedWithHarnessFallback),
        );

        assert_eq!(failed_homes, homes);
        assert_eq!(failed_external, external);
    }
}
