use rub_core::process::is_process_alive;
use std::time::Duration;

pub fn terminate_spawned_daemon(pid: u32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub async fn terminate_spawned_daemon_force(pid: u32) -> std::io::Result<()> {
    let _ = terminate_spawned_daemon(pid);
    for _ in 0..20 {
        if !is_process_alive(pid) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if result == 0 || !is_process_alive(pid) {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
