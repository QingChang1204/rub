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
    daemon_processes_for_home, default_session_pid_path, e2e_home_owner_pid,
    managed_browser_profile_dir_for_daemon, observe_home_cleanup, prepare_home, session_pid_path,
    verify_home_cleanup_complete, wait_for_home_processes_to_exit,
};
pub use self::external_chrome::{
    browser_binary_for_external_tests, external_chrome_pid_matches_profile_in_snapshot,
    free_tcp_port, probe_cdp_http_ready_once, spawn_external_chrome, terminate_external_chrome,
    verify_external_chrome_cleanup_complete, wait_for_cdp_http_ready, wait_for_tcp_endpoint,
    wait_until,
};
pub use self::profile_env::prepare_fake_profile_env;

static CLEANUP_HOOK: Once = Once::new();
static REGISTERED_HOMES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static REGISTERED_EXTERNAL_CHROMES: OnceLock<Mutex<Vec<(u32, PathBuf)>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct HomeCleanupObservation {
    pub daemon_root_pids: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupVerification {
    Verified,
    SkippedDuringPanic,
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

pub fn register_external_chrome(pid: u32, profile_dir: &Path) {
    install_cleanup_hook();
    let mut entries = registered_external_chromes().lock().unwrap();
    if !entries.iter().any(|(existing_pid, _)| *existing_pid == pid) {
        entries.push((pid, profile_dir.to_path_buf()));
    }
}

pub fn unregister_external_chrome(pid: u32) {
    let mut entries = registered_external_chromes().lock().unwrap();
    entries.retain(|(existing_pid, _)| *existing_pid != pid);
}

pub fn cleanup_registered_artifacts() {
    if std::thread::panicking() {
        return;
    }

    let homes = registered_homes().lock().unwrap().clone();
    let mut failed_homes = Vec::new();
    for home in homes {
        match cleanup::try_cleanup_home(&home) {
            Ok(CleanupVerification::Verified) => {}
            Ok(CleanupVerification::SkippedDuringPanic) | Err(_) => {
                failed_homes.push(home);
            }
        }
    }

    let external = registered_external_chromes().lock().unwrap().clone();
    let mut failed_external = Vec::new();
    for (pid, profile_dir) in external {
        match external_chrome::try_cleanup_external_chrome(pid, &profile_dir) {
            Ok(CleanupVerification::Verified) => {}
            Ok(CleanupVerification::SkippedDuringPanic) | Err(_) => {
                failed_external.push((pid, profile_dir));
            }
        }
    }

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

pub(super) fn rub_binary() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_rub") {
        return path;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let workspace = std::path::Path::new(&manifest)
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(std::path::Path::new("."));
    let path = workspace.join("target/debug/rub");
    path.to_string_lossy().to_string()
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
            "rub-e2e-{}-{}",
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
