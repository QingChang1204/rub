use rub_core::fs::atomic_write_bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::Path;

pub(crate) fn ensure_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
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
    atomic_write_bytes(path, &json, mode)
        .map_err(|error| map_io_error(path, "write_failed", error))
        .map(|_| ())
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
