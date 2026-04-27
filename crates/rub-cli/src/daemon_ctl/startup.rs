use super::{DaemonCtlPathContext, daemon_ctl_path_error};
use rub_core::error::{ErrorCode, RubError};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;
use std::path::Path;

const READY_FILE_ENV: &str = "RUB_DAEMON_READY_FILE";
const ERROR_FILE_ENV: &str = "RUB_DAEMON_ERROR_FILE";
const STDERR_FILE_ENV: &str = "RUB_DAEMON_STDERR_FILE";
const CLEANUP_FILE_ENV: &str = "RUB_DAEMON_CLEANUP_FILE";
const SESSION_ID_ENV: &str = "RUB_SESSION_ID";
pub(super) const STARTUP_INPUTS_ENV: &str = "RUB_STARTUP_INPUTS";

mod readiness;

#[cfg(all(test, unix))]
pub(crate) use self::readiness::detach_daemon_session;
#[cfg(test)]
pub(crate) use self::readiness::{
    acquire_startup_lock, read_startup_error, startup_ready_retry_timeout_failure, wait_for_ready,
};
pub(crate) use self::readiness::{
    acquire_startup_lock_until, start_daemon, upgrade_startup_lock_to_canonical_attachment_until,
    wait_for_ready_until,
};

pub struct StartupSignalFiles {
    pub ready_file: std::path::PathBuf,
    pub error_file: std::path::PathBuf,
    pub stderr_file: std::path::PathBuf,
    pub cleanup_file: std::path::PathBuf,
    pub daemon_pid: u32,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupCleanupAuthorityKind {
    ManagedBrowserProfileFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupCleanupProof {
    /// Recovery-only fallback authority for a startup path that died before the
    /// owning `rub-cdp` launch transaction could report or clean itself up.
    pub kind: StartupCleanupAuthorityKind,
    pub managed_user_data_dir: String,
    pub managed_profile_directory: Option<String>,
    pub ephemeral: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritativeStartupInputs {
    pub connection_request: crate::session_policy::ConnectionRequest,
    pub attachment_identity: Option<String>,
}

pub fn startup_signal_paths() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    (
        std::env::var_os(READY_FILE_ENV).map(std::path::PathBuf::from),
        std::env::var_os(ERROR_FILE_ENV).map(std::path::PathBuf::from),
    )
}

pub(crate) fn startup_cleanup_signal_path() -> Option<std::path::PathBuf> {
    std::env::var_os(CLEANUP_FILE_ENV).map(std::path::PathBuf::from)
}

pub(crate) fn read_authoritative_startup_inputs()
-> Result<Option<AuthoritativeStartupInputs>, RubError> {
    let Some(raw) = std::env::var_os(STARTUP_INPUTS_ENV) else {
        return Ok(None);
    };
    serde_json::from_str(&raw.to_string_lossy())
        .map(Some)
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::DaemonStartFailed,
                format!("Failed to parse authoritative startup inputs: {error}"),
                serde_json::json!({
                    "reason": "authoritative_startup_inputs_parse_failed",
                }),
            )
        })
}

pub(crate) fn write_startup_cleanup_proof_at(
    path: &Path,
    proof: &StartupCleanupProof,
) -> std::io::Result<rub_core::fs::FileCommitOutcome> {
    let json = serde_json::to_vec(proof).map_err(|error| {
        std::io::Error::other(format!("serialize startup cleanup proof: {error}"))
    })?;
    rub_core::fs::atomic_write_bytes(path, &json, 0o600)
}

pub(crate) fn clear_startup_cleanup_proof(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn read_startup_cleanup_proof(path: &Path) -> Result<StartupCleanupProof, RubError> {
    let raw = std::fs::read(path).map_err(|error| {
        daemon_ctl_path_error(
            ErrorCode::DaemonStartFailed,
            format!(
                "Failed to read startup cleanup proof {}: {error}",
                path.display()
            ),
            DaemonCtlPathContext {
                path_key: "cleanup_file",
                path,
                path_authority: "daemon_ctl.startup.cleanup_file",
                upstream_truth: "startup_cleanup_signal_file",
                path_kind: "startup_cleanup_file",
                reason: "startup_cleanup_file_read_failed",
            },
        )
    })?;
    serde_json::from_slice(&raw).map_err(|error| {
        daemon_ctl_path_error(
            ErrorCode::DaemonStartFailed,
            format!(
                "Failed to parse startup cleanup proof {}: {error}",
                path.display()
            ),
            DaemonCtlPathContext {
                path_key: "cleanup_file",
                path,
                path_authority: "daemon_ctl.startup.cleanup_file",
                upstream_truth: "startup_cleanup_signal_file",
                path_kind: "startup_cleanup_file",
                reason: "startup_cleanup_file_parse_failed",
            },
        )
    })
}

pub(super) fn startup_lock_scope_keys(
    session_name: &str,
    attachment_identity: Option<&str>,
) -> Vec<String> {
    let mut keys = vec![format!("session-{session_name}")];
    if let Some(identity) = attachment_identity {
        keys.push(format!("attachment-{identity}"));
    }
    keys
}

pub(super) fn try_lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(super) fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
