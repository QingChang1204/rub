use super::RegistryData;
use super::lock::{flock, unlock};
use super::validation::validate_registry_data_for_home;
use crate::rub_paths::RubPaths;
use rub_core::fs::{FileCommitOutcome, atomic_write_bytes};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

fn registry_path(home: &Path) -> PathBuf {
    RubPaths::new(home).registry_path()
}

fn registry_lock_path(home: &Path) -> PathBuf {
    RubPaths::new(home).registry_lock_path()
}

pub(super) fn ensure_rub_home(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)
}

pub(super) fn read_registry(home: &Path) -> std::io::Result<RegistryData> {
    if !home.exists() {
        return Ok(RegistryData::default());
    }
    with_registry_lock(home, false, |path| load_registry_for_home(home, path))
}

pub(super) fn write_registry(home: &Path, data: &RegistryData) -> std::io::Result<()> {
    with_registry_lock(home, true, |path| store_registry_for_home(home, path, data))
}

pub(crate) fn with_registry_lock<T>(
    home: &Path,
    exclusive: bool,
    f: impl FnOnce(&Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    ensure_rub_home(home)?;
    let registry_path = registry_path(home);
    let lock_path = registry_lock_path(home);
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;

    flock(&lock_file, exclusive)?;
    let result = f(&registry_path);
    let unlock_result = unlock(&lock_file);

    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(err)) => Err(err),
        (Err(err), Err(_)) => Err(err),
    }
}

pub(crate) fn load_registry_for_home(home: &Path, path: &Path) -> std::io::Result<RegistryData> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if contents.trim().is_empty() {
        return Ok(RegistryData::default());
    }

    let data = serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    validate_registry_data_for_home(home, &data)?;
    Ok(data)
}

pub(crate) fn store_registry_for_home(
    home: &Path,
    path: &Path,
    data: &RegistryData,
) -> std::io::Result<()> {
    validate_registry_data_for_home(home, data)?;
    let json = serde_json::to_string_pretty(&data).map_err(std::io::Error::other)?;
    let outcome = atomic_write_bytes(path, json.as_bytes(), 0o600)?;
    require_durable_registry_commit(path, outcome)
}

fn require_durable_registry_commit(path: &Path, outcome: FileCommitOutcome) -> std::io::Result<()> {
    if outcome.durability_confirmed() {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "Registry commit for {} was published but durability was not confirmed",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::require_durable_registry_commit;
    use rub_core::fs::FileCommitOutcome;
    use std::path::Path;

    #[test]
    fn durable_registry_commit_accepts_durable_outcome() {
        require_durable_registry_commit(
            Path::new("/tmp/registry.json"),
            FileCommitOutcome::Durable,
        )
        .expect("durable registry outcome should remain a valid commit fence");
    }

    #[test]
    fn durable_registry_commit_rejects_published_only_outcome() {
        let error = require_durable_registry_commit(
            Path::new("/tmp/registry.json"),
            FileCommitOutcome::Published,
        )
        .expect_err("published-only registry outcome must not count as a durable authority commit");
        assert!(
            error.to_string().contains("durability was not confirmed"),
            "registry durability error should explain the missing hard fence: {error}"
        );
    }
}
