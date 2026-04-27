use std::sync::Arc;

use tokio::task::JoinHandle;
use tracing::{error, info};

use crate::session::SessionState;

use super::{SHUTDOWN_DRAIN_POLL_INTERVAL, SHUTDOWN_DRAIN_TIMEOUT};

pub(super) async fn wait_for_transaction_drain(state: &Arc<SessionState>) {
    wait_for_transaction_drain_with_timeout(
        state,
        SHUTDOWN_DRAIN_TIMEOUT,
        SHUTDOWN_DRAIN_POLL_INTERVAL,
    )
    .await;
}

pub(super) async fn wait_for_transaction_drain_with_timeout(
    state: &Arc<SessionState>,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut live_timeout_logged = false;
    let mut post_commit_timeout_logged = false;
    loop {
        let now = tokio::time::Instant::now();
        let in_flight = state
            .in_flight_count
            .load(std::sync::atomic::Ordering::SeqCst);
        let connected = state
            .connected_client_count
            .load(std::sync::atomic::Ordering::SeqCst);
        let pre_request_response_fences = state
            .pre_request_response_fence_count
            .load(std::sync::atomic::Ordering::SeqCst);
        let post_commit_followups = state.pending_post_commit_followup_count();
        if in_flight == 0
            && connected == 0
            && pre_request_response_fences == 0
            && post_commit_followups == 0
        {
            break;
        }
        state.record_shutdown_drain_wait(in_flight, connected, pre_request_response_fences);
        if now >= deadline
            && !live_timeout_logged
            && (in_flight != 0 || connected != 0 || pre_request_response_fences != 0)
        {
            state.record_shutdown_drain_soft_timeout(
                in_flight,
                connected,
                pre_request_response_fences,
            );
            live_timeout_logged = true;
            info!(
                in_flight_count = in_flight,
                connected_client_count = connected,
                pre_request_response_fence_count = pre_request_response_fences,
                "Shutdown drain exceeded the soft budget; continuing to wait because active request or response fences are still authoritative"
            );
        }
        if now >= deadline
            && !post_commit_timeout_logged
            && in_flight == 0
            && connected == 0
            && pre_request_response_fences == 0
            && post_commit_followups > 0
        {
            post_commit_timeout_logged = true;
            info!(
                post_commit_followup_count = post_commit_followups,
                "Shutdown drain exceeded the soft budget; continuing to wait because downstream post-commit recovery followups are still running"
            );
        }
        tokio::time::sleep(poll_interval).await;
    }

    if state.pending_post_commit_projection_count() > 0 {
        state.drain_post_commit_projections().await;
    }
}

pub(super) async fn wait_for_worker_shutdown(handle: JoinHandle<()>, worker_name: &str) {
    wait_for_worker_shutdown_with_timeout(handle, worker_name, SHUTDOWN_DRAIN_TIMEOUT).await;
}

pub(super) async fn wait_for_worker_shutdown_with_timeout(
    mut handle: JoinHandle<()>,
    worker_name: &str,
    timeout: std::time::Duration,
) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            error!(worker = worker_name, error = %error, "Shutdown worker task exited with join error");
        }
        Err(_) => {
            error!(
                worker = worker_name,
                "Shutdown worker exceeded the soft budget; continuing to wait because aborting it could drop an in-flight automation transaction guard"
            );
            match handle.await {
                Ok(()) => {}
                Err(error) => {
                    error!(
                        worker = worker_name,
                        error = %error,
                        "Shutdown worker task exited with join error after the soft budget"
                    );
                }
            }
        }
    }
}

/// Wait for SIGTERM or SIGINT.
pub(super) async fn wait_for_shutdown_signal()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = sigterm.recv() => { info!("Received SIGTERM"); }
            _ = sigint.recv() => { info!("Received SIGINT"); }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        info!("Received Ctrl-C");
    }
    Ok(())
}
