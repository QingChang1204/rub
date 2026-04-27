use rub_core::fs::{FileCommitOutcome, atomic_write_bytes};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

#[cfg(test)]
static FORCE_DIRECTORY_SYNC_FAILURE_ONCE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
static FORCE_PUBLISHED_WRITE_OUTCOME_ONCE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
> = std::sync::OnceLock::new();

pub(crate) fn ensure_directory(path: &Path) -> io::Result<()> {
    let created_directories = missing_directory_chain(path);
    std::fs::create_dir_all(path)?;
    for directory in created_directories.iter().rev() {
        sync_created_directory_authority(directory)?;
    }
    Ok(())
}

pub(crate) fn open_text_file_with_create(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

pub(crate) fn read_text_file(file: &mut File) -> io::Result<String> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

pub(crate) fn load_json_file_with_create<T, E, MIo, MParse>(
    path: &Path,
    map_io_error: MIo,
    map_parse_error: MParse,
) -> Result<T, E>
where
    T: Default + DeserializeOwned,
    MIo: Fn(&Path, &'static str, io::Error) -> E,
    MParse: Fn(&Path, serde_json::Error) -> E,
{
    let mut file = open_text_file_with_create(path)
        .map_err(|error| map_io_error(path, "open_failed", error))?;
    let contents =
        read_text_file(&mut file).map_err(|error| map_io_error(path, "read_failed", error))?;
    if contents.trim().is_empty() {
        Ok(T::default())
    } else {
        serde_json::from_str::<T>(&contents).map_err(|error| map_parse_error(path, error))
    }
}

pub(crate) fn write_pretty_json_file<T, E, MIo>(
    path: &Path,
    value: &T,
    mode: u32,
    map_io_error: MIo,
) -> Result<(), E>
where
    T: Serialize,
    MIo: Fn(&Path, &'static str, io::Error) -> E,
{
    let json = serde_json::to_vec_pretty(value)
        .map_err(|error| map_io_error(path, "serialize_failed", io::Error::other(error)))?;
    let outcome = atomic_write_bytes(path, &json, mode)
        .map_err(|error| map_io_error(path, "write_failed", error))?;
    let outcome = local_registry_commit_outcome_for_path(path, outcome);
    require_durable_local_registry_commit(path, outcome)
        .map_err(|error| map_io_error(path, "write_failed", error))
}

fn require_durable_local_registry_commit(
    path: &Path,
    outcome: FileCommitOutcome,
) -> io::Result<()> {
    if outcome.durability_confirmed() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "Local registry commit for {} was published but durability was not confirmed",
        path.display()
    )))
}

fn local_registry_commit_outcome_for_path(
    path: &Path,
    outcome: FileCommitOutcome,
) -> FileCommitOutcome {
    #[cfg(test)]
    if consume_published_write_outcome_once(path) {
        return FileCommitOutcome::Published;
    }
    #[cfg(not(test))]
    let _ = path;
    outcome
}

pub(crate) struct FileLockGuard {
    file: Option<File>,
}

impl FileLockGuard {
    pub(crate) fn open_lock_file(lock_path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
    }

    pub(crate) fn lock(file: File, exclusive: bool) -> io::Result<Self> {
        flock(&file, exclusive)?;
        Ok(Self { file: Some(file) })
    }
    pub(crate) fn release(mut self) -> io::Result<()> {
        if let Some(file) = self.file.take() {
            unlock(&file)
        } else {
            Ok(())
        }
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = unlock(&file);
        }
    }
}

pub(crate) fn with_file_lock<T, E, F, M>(
    lock_path: &Path,
    exclusive: bool,
    open_reason: &'static str,
    lock_reason: &'static str,
    unlock_reason: &'static str,
    map_io_error: M,
    f: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    M: Fn(&Path, &'static str, io::Error) -> E,
{
    let file = FileLockGuard::open_lock_file(lock_path)
        .map_err(|error| map_io_error(lock_path, open_reason, error))?;
    let guard = FileLockGuard::lock(file, exclusive)
        .map_err(|error| map_io_error(lock_path, lock_reason, error))?;
    let result = f();
    let unlock_result = guard.release();

    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(map_io_error(lock_path, unlock_reason, error)),
        (Err(error), Err(_)) => Err(error),
    }
}

fn flock(file: &File, exclusive: bool) -> io::Result<()> {
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unlock(file: &File) -> io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn missing_directory_chain(path: &Path) -> Vec<PathBuf> {
    if path.as_os_str().is_empty() || path.exists() {
        return Vec::new();
    }

    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    while !cursor.exists() {
        missing.push(cursor.clone());
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent == cursor {
            break;
        }
        cursor = parent.to_path_buf();
    }
    missing
}

fn sync_created_directory_authority(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    if consume_directory_sync_failure_once(path) {
        return Err(io::Error::other(format!(
            "forced local registry directory sync failure for {}",
            path.display()
        )));
    }
    rub_core::fs::sync_parent_dir(path)
}

#[cfg(test)]
fn consume_directory_sync_failure_once(path: &Path) -> bool {
    FORCE_DIRECTORY_SYNC_FAILURE_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("local registry directory sync failure registry")
        .remove(path)
}

#[cfg(test)]
pub(crate) fn force_directory_sync_failure_once_for_test(path: &Path) {
    FORCE_DIRECTORY_SYNC_FAILURE_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("local registry directory sync failure registry")
        .insert(path.to_path_buf());
}

#[cfg(test)]
fn consume_published_write_outcome_once(path: &Path) -> bool {
    FORCE_PUBLISHED_WRITE_OUTCOME_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("local registry published write outcome registry")
        .remove(path)
}

#[cfg(test)]
pub(crate) fn force_published_write_outcome_once_for_test(path: &Path) {
    FORCE_PUBLISHED_WRITE_OUTCOME_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("local registry published write outcome registry")
        .insert(path.to_path_buf());
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_directory, force_directory_sync_failure_once_for_test,
        require_durable_local_registry_commit,
    };
    use rub_core::fs::FileCommitOutcome;
    use std::path::Path;

    #[test]
    fn ensure_directory_rejects_unconfirmed_created_directory_fence() {
        let root =
            std::env::temp_dir().join(format!("rub-local-registry-dir-{}", uuid::Uuid::now_v7()));
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        force_directory_sync_failure_once_for_test(&home);

        let error = ensure_directory(&home)
            .expect_err("unconfirmed directory creation fence must fail local registry setup");
        assert!(
            error
                .to_string()
                .contains("forced local registry directory sync failure"),
            "directory durability error should explain the missing hard fence: {error}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn durable_local_registry_commit_accepts_durable_outcome() {
        require_durable_local_registry_commit(
            Path::new("/tmp/bindings.json"),
            FileCommitOutcome::Durable,
        )
        .expect("durable local registry outcome should remain a valid commit fence");
    }

    #[test]
    fn durable_local_registry_commit_rejects_published_only_outcome() {
        let error = require_durable_local_registry_commit(
            Path::new("/tmp/bindings.json"),
            FileCommitOutcome::Published,
        )
        .expect_err("published-only local registry outcome must not count as durable");
        assert!(
            error.to_string().contains("durability was not confirmed"),
            "local registry durability error should explain the missing hard fence: {error}"
        );
    }
}
