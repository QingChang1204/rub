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

pub fn force_kill_process(pid: u32) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if result == 0 || !is_process_alive(pid) {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub async fn wait_for_process_exit(pid: u32, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if !is_process_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !is_process_alive(pid)
}
