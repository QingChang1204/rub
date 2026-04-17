use crate::local_registry::{FileLockGuard, ensure_directory};

use super::{secret_registry_error, secret_registry_io_error};
use rub_core::error::RubError;
use rub_core::secrets_env::{
    SecretsEnvPersistOutcome, load_secrets_env_file, remove_secrets_env_file,
    write_secrets_env_file,
};
use rub_daemon::rub_paths::RubPaths;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn load_secret_store_unlocked(
    rub_home: &Path,
) -> Result<BTreeMap<String, String>, RubError> {
    let path = RubPaths::new(rub_home).secrets_env_path();
    load_secrets_env_file(&path).map_err(|error| {
        secret_registry_error(
            rub_home,
            &path,
            "cli.secret.subject.secrets_path",
            "secrets_env_file",
            "secret_registry_load_failed",
            error,
        )
    })
}

pub(super) fn with_secret_store<T>(
    rub_home: &Path,
    f: impl FnOnce(&BTreeMap<String, String>) -> Result<T, RubError>,
) -> Result<T, RubError> {
    let _lock = open_secret_lock(rub_home, false)?;
    let values = load_secret_store_unlocked(rub_home)?;
    f(&values)
}

pub(super) fn persist_secret_store_unlocked(
    rub_home: &Path,
    values: &BTreeMap<String, String>,
) -> Result<SecretsEnvPersistOutcome, RubError> {
    let path = RubPaths::new(rub_home).secrets_env_path();
    if values.is_empty() {
        remove_secrets_env_file(&path).map_err(|error| {
            secret_registry_error(
                rub_home,
                &path,
                "cli.secret.subject.secrets_path",
                "secrets_env_file",
                "secret_registry_remove_failed",
                error,
            )
        })
    } else {
        write_secrets_env_file(&path, values).map_err(|error| {
            secret_registry_error(
                rub_home,
                &path,
                "cli.secret.subject.secrets_path",
                "secrets_env_file",
                "secret_registry_persist_failed",
                error,
            )
        })
    }
}

pub(super) fn update_secret_store<T>(
    rub_home: &Path,
    f: impl FnOnce(&mut BTreeMap<String, String>) -> Result<T, RubError>,
) -> Result<(T, SecretsEnvPersistOutcome), RubError> {
    let _lock = open_secret_lock(rub_home, true)?;
    let mut values = load_secret_store_unlocked(rub_home)?;
    let result = f(&mut values)?;
    let persist_outcome = persist_secret_store_unlocked(rub_home, &values)?;
    Ok((result, persist_outcome))
}

pub(super) fn open_secret_lock(
    rub_home: &Path,
    exclusive: bool,
) -> Result<SecretLockGuard, RubError> {
    ensure_directory(rub_home).map_err(|error| {
        secret_registry_io_error(
            rub_home,
            rub_home,
            "cli.secret.subject.rub_home",
            "rub_home_directory",
            "secret_registry_rub_home_create_failed",
            error,
        )
    })?;
    let paths = RubPaths::new(rub_home);
    let lock_path = paths.secrets_env_lock_path();
    let file = FileLockGuard::open_lock_file(&lock_path).map_err(|error| {
        secret_registry_io_error(
            rub_home,
            &lock_path,
            "cli.secret.subject.lock_path",
            "secrets_env_lock",
            "secret_registry_lock_open_failed",
            error,
        )
    })?;
    FileLockGuard::lock(file, exclusive).map_err(|error| {
        secret_registry_io_error(
            rub_home,
            &lock_path,
            "cli.secret.subject.lock_path",
            "secrets_env_lock",
            "secret_registry_lock_failed",
            error,
        )
    })
}

pub(super) type SecretLockGuard = FileLockGuard;
