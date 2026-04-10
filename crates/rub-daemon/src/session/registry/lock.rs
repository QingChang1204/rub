use std::os::fd::AsRawFd;

pub(super) fn flock(file: &std::fs::File, exclusive: bool) -> std::io::Result<()> {
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

pub(super) fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
