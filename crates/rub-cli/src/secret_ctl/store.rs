use super::secret_registry_io_error;
use rub_core::error::RubError;
use rub_core::secrets_env::{
    load_secrets_env_file, remove_secrets_env_file, write_secrets_env_file,
};
use rub_daemon::rub_paths::RubPaths;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub(super) fn read_secret_store(rub_home: &Path) -> Result<BTreeMap<String, String>, RubError> {
    let _lock = open_secret_lock(rub_home, false)?;
    load_secret_store_unlocked(rub_home)
}

pub(super) fn load_secret_store_unlocked(
    rub_home: &Path,
) -> Result<BTreeMap<String, String>, RubError> {
    load_secrets_env_file(&RubPaths::new(rub_home).secrets_env_path())
}

pub(super) fn persist_secret_store_unlocked(
    rub_home: &Path,
    values: &BTreeMap<String, String>,
) -> Result<(), RubError> {
    let path = RubPaths::new(rub_home).secrets_env_path();
    if values.is_empty() {
        remove_secrets_env_file(&path)?;
    } else {
        write_secrets_env_file(&path, values)?;
    }
    Ok(())
}

pub(super) fn open_secret_lock(
    rub_home: &Path,
    exclusive: bool,
) -> Result<SecretLockGuard, RubError> {
    let paths = RubPaths::new(rub_home);
    let lock_path = paths.secrets_env_lock_path();
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            secret_registry_io_error(
                rub_home,
                &lock_path,
                "secret_registry_lock_open_failed",
                error,
            )
        })?;
    flock(&lock_file, exclusive).map_err(|error| {
        secret_registry_io_error(rub_home, &lock_path, "secret_registry_lock_failed", error)
    })?;
    Ok(SecretLockGuard {
        rub_home: rub_home.to_path_buf(),
        lock_path,
        file: Some(lock_file),
    })
}

pub(super) struct SecretLockGuard {
    rub_home: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
}

impl Drop for SecretLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = unlock(&file).map_err(|error| {
                let _ = secret_registry_io_error(
                    &self.rub_home,
                    &self.lock_path,
                    "secret_registry_unlock_failed",
                    error,
                );
            });
        }
    }
}

fn flock(file: &File, exclusive: bool) -> std::io::Result<()> {
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unlock(file: &File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
