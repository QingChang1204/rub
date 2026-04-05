use std::fs::{File, OpenOptions, create_dir_all, rename};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

static TEMP_PATH_COUNTER: AtomicU64 = AtomicU64::new(0);

const TEMP_PATH_COLLISION_RETRIES: u32 = 16;

#[cfg(test)]
static FORCE_TEMP_PATH_COLLISION_ONCE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
static FORCE_SYNC_PARENT_DIR_FAILURE_ONCE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
> = std::sync::OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCommitOutcome {
    Durable,
    Published,
}

impl FileCommitOutcome {
    pub fn durability_confirmed(self) -> bool {
        matches!(self, Self::Durable)
    }
}

pub fn atomic_write_bytes(
    path: &Path,
    contents: &[u8],
    mode: u32,
) -> io::Result<FileCommitOutcome> {
    let created_directories = path
        .parent()
        .map(ensure_directory_chain)
        .transpose()?
        .unwrap_or_default();
    let (temp_path, mut temp) = create_unique_temporary_file(path, mode)?;
    temp.write_all(contents)?;
    temp.sync_all()?;
    let outcome = {
        #[cfg(unix)]
        {
            commit_temporary_file_from_synced_handle(&temp, &temp_path, path)
        }
        #[cfg(not(unix))]
        {
            drop(temp);
            commit_temporary_file(&temp_path, path)
        }
    }?;
    confirm_created_directory_chain(path, &created_directories, outcome)
}

pub fn commit_temporary_file(temp_path: &Path, final_path: &Path) -> io::Result<FileCommitOutcome> {
    let temp = File::open(temp_path)?;
    temp.sync_all()?;
    #[cfg(unix)]
    {
        commit_temporary_file_from_synced_handle(&temp, temp_path, final_path)
    }
    #[cfg(not(unix))]
    {
        if final_path.exists() {
            return Err(io::Error::new(
                ErrorKind::Unsupported,
                format!(
                    "atomic replacement is not supported for existing file {} on this platform",
                    final_path.display()
                ),
            ));
        }

        rename(temp_path, final_path)?;
        finalize_published_file(final_path)
    }
}

pub fn commit_temporary_file_no_clobber(
    temp_path: &Path,
    final_path: &Path,
) -> io::Result<FileCommitOutcome> {
    let temp = File::open(temp_path)?;
    temp.sync_all()?;
    #[cfg(unix)]
    ensure_temp_path_matches_file_authority(&temp, temp_path)?;
    match std::fs::hard_link(temp_path, final_path) {
        Ok(()) => {
            let remove_error = std::fs::remove_file(temp_path).err();
            let parent_sync = sync_parent_dir(final_path);
            if remove_error.is_some() || parent_sync.is_err() {
                return Ok(FileCommitOutcome::Published);
            }
            Ok(FileCommitOutcome::Durable)
        }
        Err(error) => Err(error),
    }
}

pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    if consume_sync_parent_dir_failure_once(path) {
        return Err(io::Error::other("forced parent sync failure"));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent_dir = File::open(parent)?;
    parent_dir.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn commit_temporary_file_from_synced_handle(
    temp: &File,
    temp_path: &Path,
    final_path: &Path,
) -> io::Result<FileCommitOutcome> {
    ensure_temp_path_matches_file_authority(temp, temp_path)?;
    rename(temp_path, final_path)?;
    finalize_published_file(final_path)
}

#[cfg(unix)]
fn ensure_temp_path_matches_file_authority(temp: &File, temp_path: &Path) -> io::Result<()> {
    let file_metadata = temp.metadata()?;
    let path_metadata = std::fs::symlink_metadata(temp_path)?;
    if file_metadata.dev() != path_metadata.dev() || file_metadata.ino() != path_metadata.ino() {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Refusing to commit temporary file {} because its path authority changed after the write fence",
                temp_path.display()
            ),
        ));
    }
    Ok(())
}

fn finalize_published_file(final_path: &Path) -> io::Result<FileCommitOutcome> {
    match sync_parent_dir(final_path) {
        Ok(()) => Ok(FileCommitOutcome::Durable),
        Err(_) => Ok(FileCommitOutcome::Published),
    }
}

fn ensure_directory_chain(path: &Path) -> io::Result<Vec<PathBuf>> {
    if path.as_os_str().is_empty() || path.exists() {
        return Ok(Vec::new());
    }

    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    while !cursor.exists() {
        missing.push(cursor.clone());
        let Some(parent) = cursor.parent() else {
            break;
        };
        cursor = parent.to_path_buf();
    }

    for directory in missing.iter().rev() {
        create_dir_all(directory)?;
    }
    missing.reverse();
    Ok(missing)
}

fn confirm_created_directory_chain(
    final_path: &Path,
    created_directories: &[PathBuf],
    outcome: FileCommitOutcome,
) -> io::Result<FileCommitOutcome> {
    if !matches!(outcome, FileCommitOutcome::Durable) || created_directories.is_empty() {
        return Ok(outcome);
    }
    match sync_created_directory_chain(final_path, created_directories) {
        Ok(()) => Ok(FileCommitOutcome::Durable),
        Err(_) => Ok(FileCommitOutcome::Published),
    }
}

fn sync_created_directory_chain(
    final_path: &Path,
    created_directories: &[PathBuf],
) -> io::Result<()> {
    let mut already_synced = std::collections::BTreeSet::new();
    for directory in created_directories.iter().rev() {
        let Some(parent) = directory.parent() else {
            continue;
        };
        if !already_synced.insert(parent.to_path_buf()) {
            continue;
        }
        sync_parent_dir(directory)?;
    }
    if let Some(parent) = final_path.parent()
        && already_synced.insert(parent.to_path_buf())
    {
        sync_parent_dir(final_path)?;
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("temp");
    #[cfg(test)]
    if consume_temp_path_collision_once(path) {
        return path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{file_name}.forced-collision.tmp"));
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".{file_name}.{unique}.{}.tmp",
            TEMP_PATH_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
}

fn create_unique_temporary_file(path: &Path, mode: u32) -> io::Result<(PathBuf, File)> {
    let mut last_error = None;
    for _attempt in 0..TEMP_PATH_COLLISION_RETRIES {
        let temp_path = temporary_path(path);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            options.mode(mode);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            ErrorKind::AlreadyExists,
            "failed to allocate a unique temporary file path",
        )
    }))
}

#[cfg(test)]
fn consume_temp_path_collision_once(path: &Path) -> bool {
    FORCE_TEMP_PATH_COLLISION_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("temp-path collision registry")
        .remove(path)
}

#[cfg(test)]
fn force_temp_path_collision_once_for(path: &Path) {
    FORCE_TEMP_PATH_COLLISION_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("temp-path collision registry")
        .insert(path.to_path_buf());
}

#[cfg(test)]
fn consume_sync_parent_dir_failure_once(path: &Path) -> bool {
    FORCE_SYNC_PARENT_DIR_FAILURE_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("parent-sync failure registry")
        .remove(path)
}

#[cfg(test)]
fn force_sync_parent_dir_failure_once_for(path: &Path) {
    FORCE_SYNC_PARENT_DIR_FAILURE_ONCE
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("parent-sync failure registry")
        .insert(path.to_path_buf());
}

#[cfg(test)]
mod tests {
    use super::{
        FileCommitOutcome, atomic_write_bytes, commit_temporary_file,
        commit_temporary_file_no_clobber, create_unique_temporary_file,
        force_sync_parent_dir_failure_once_for, force_temp_path_collision_once_for,
    };
    use std::io::ErrorKind;
    use std::io::Write;

    #[test]
    fn atomic_write_bytes_replaces_existing_file() {
        let root =
            std::env::temp_dir().join(format!("rub-core-atomic-write-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp root");
        let path = root.join("asset.json");
        std::fs::write(&path, b"old").expect("seed file");
        let outcome = atomic_write_bytes(&path, b"new", 0o600).expect("atomic overwrite");
        assert_eq!(outcome, FileCommitOutcome::Durable);
        assert_eq!(std::fs::read(&path).expect("read file"), b"new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_bytes_creates_missing_parent_directories() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-atomic-write-parent-{}",
            std::process::id()
        ));
        let path = root.join("nested").join("deeper").join("asset.json");
        let outcome = atomic_write_bytes(&path, b"{}", 0o600).expect("atomic write");
        assert_eq!(outcome, FileCommitOutcome::Durable);
        assert_eq!(std::fs::read(&path).expect("read file"), b"{}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_reports_published_when_created_directory_chain_cannot_be_synced() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-atomic-write-created-chain-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let nested = root.join("nested");
        let path = nested.join("deeper").join("asset.json");
        force_sync_parent_dir_failure_once_for(&nested);

        let outcome = atomic_write_bytes(&path, b"{}", 0o600).expect("atomic write");
        assert_eq!(outcome, FileCommitOutcome::Published);
        assert_eq!(std::fs::read(&path).expect("read file"), b"{}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn no_clobber_commit_refuses_existing_destination() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-no-clobber-existing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let final_path = root.join("asset.json");
        let temp_path = root.join(".asset.json.tmp");
        std::fs::write(&final_path, b"live").expect("seed final");
        std::fs::write(&temp_path, b"new").expect("seed temp");

        let error = commit_temporary_file_no_clobber(&temp_path, &final_path)
            .expect_err("must not clobber");
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&final_path).expect("read final"), b"live");
        assert_eq!(std::fs::read(&temp_path).expect("read temp"), b"new");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_write_retries_after_temp_path_collision() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-atomic-write-collision-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let final_path = root.join("asset.json");
        let first_temp = root.join(".asset.json.forced-collision.tmp");
        std::fs::write(&first_temp, b"collision").expect("seed colliding temp file");
        force_temp_path_collision_once_for(&final_path);

        let outcome = atomic_write_bytes(&final_path, b"new", 0o600).expect("atomic write retries");
        assert_eq!(outcome, FileCommitOutcome::Durable);
        assert_eq!(std::fs::read(&final_path).expect("read final"), b"new");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn commit_reports_published_when_parent_sync_fails_after_rename() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-atomic-write-parent-sync-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let final_path = root.join("asset.json");
        let temp_path = root.join(".asset.json.tmp");
        std::fs::write(&temp_path, b"new").expect("seed temp");
        force_sync_parent_dir_failure_once_for(&final_path);

        let outcome = commit_temporary_file(&temp_path, &final_path).expect("commit outcome");
        assert_eq!(outcome, FileCommitOutcome::Published);
        assert_eq!(std::fs::read(&final_path).expect("read final"), b"new");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(unix)]
    fn commit_rejects_temp_path_authority_replacement_after_write_fence() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-atomic-write-authority-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");
        let final_path = root.join("asset.json");
        let (temp_path, mut temp) =
            create_unique_temporary_file(&final_path, 0o600).expect("create temp");
        temp.write_all(b"original").expect("write temp");
        temp.sync_all().expect("sync temp");

        std::fs::remove_file(&temp_path).expect("unlink original temp path");
        std::fs::write(&temp_path, b"replacement").expect("replace temp path");

        let error = super::commit_temporary_file_from_synced_handle(&temp, &temp_path, &final_path)
            .expect_err("replaced temp path must be rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_commit_fault_injection_is_path_scoped() {
        let root = std::env::temp_dir().join(format!(
            "rub-core-atomic-write-injection-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp root");

        let collision_path = root.join("collision.json");
        let collision_temp = root.join(".collision.json.forced-collision.tmp");
        std::fs::write(&collision_temp, b"collision").expect("seed colliding temp");
        force_temp_path_collision_once_for(&collision_path);

        let unaffected_path = root.join("unaffected.json");
        let unaffected_outcome =
            atomic_write_bytes(&unaffected_path, b"{}", 0o600).expect("unaffected write");
        assert_eq!(unaffected_outcome, FileCommitOutcome::Durable);
        let collision_outcome =
            atomic_write_bytes(&collision_path, b"{}", 0o600).expect("collision write");
        assert_eq!(collision_outcome, FileCommitOutcome::Durable);

        let published_path = root.join("published.json");
        let durable_path = root.join("durable.json");
        let published_temp = root.join(".published.json.tmp");
        let durable_temp = root.join(".durable.json.tmp");
        std::fs::write(&published_temp, b"published").expect("seed published temp");
        std::fs::write(&durable_temp, b"durable").expect("seed durable temp");
        force_sync_parent_dir_failure_once_for(&published_path);

        let durable_outcome =
            commit_temporary_file(&durable_temp, &durable_path).expect("durable commit");
        let published_outcome =
            commit_temporary_file(&published_temp, &published_path).expect("published commit");
        assert_eq!(durable_outcome, FileCommitOutcome::Durable);
        assert_eq!(published_outcome, FileCommitOutcome::Published);

        let _ = std::fs::remove_dir_all(root);
    }
}
