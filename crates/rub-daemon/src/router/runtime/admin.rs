use super::*;

pub(crate) async fn cmd_close(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    router.browser.close().await?;
    state.request_shutdown();
    Ok(serde_json::json!({
        "subject": {
            "kind": "session_browser",
        },
        "result": {
            "closed": true,
            "daemon_stopped": false,
            "daemon_exit_policy": "shutdown_requested_by_close",
        }
    }))
}

pub(crate) async fn cmd_handshake(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let runtime_state = state.runtime_state_snapshot().await;
    let automation_scheduler = state.automation_scheduler_metrics().await;
    let browser_event_ingress = state.browser_event_ingress_metrics().await;
    let launch_identity = state.launch_identity().await;
    Ok(serde_json::json!({
        "daemon_session_id": state.session_id,
        "ipc_protocol_version": IPC_PROTOCOL_VERSION,
        "in_flight_count": state.in_flight_count.load(std::sync::atomic::Ordering::SeqCst),
        "connected_client_count": state.connected_client_count.load(std::sync::atomic::Ordering::SeqCst),
        "browser_event_ingress_drop_count": state.browser_event_ingress_drop_count(),
        "browser_event_ingress": browser_event_ingress,
        "launch_policy": router.browser.launch_policy(),
        "attachment_identity": launch_identity.attachment_identity,
        "integration_runtime": state.integration_runtime().await,
        "dialog_runtime": state.dialog_runtime().await,
        "download_runtime": state.download_runtime().await,
        "frame_runtime": state.frame_runtime().await,
        "interference_runtime": state.interference_runtime().await,
        "storage_runtime": state.storage_runtime().await,
        "takeover_runtime": state.takeover_runtime().await,
        "orchestration_runtime": state.orchestration_runtime().await,
        "trigger_runtime": state.trigger_runtime().await,
        "runtime_observatory": state.observatory().await,
        "state_inspector": runtime_state.state_inspector,
        "readiness_state": runtime_state.readiness_state,
        "human_verification_handoff": state.human_verification_handoff().await,
        "automation_scheduler": automation_scheduler,
        "capabilities": agent_capabilities(),
    }))
}

pub(crate) async fn cmd_upgrade_check(
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let active_trigger_count = state.active_trigger_count().await;
    let active_orchestration_count = state.active_orchestration_count().await;
    let human_control_active = state.has_active_human_control().await;
    let automation_scheduler = state.automation_scheduler_metrics().await;
    let browser_event_ingress = state.browser_event_ingress_metrics().await;
    Ok(serde_json::json!({
        "idle": state.is_idle_for_upgrade().await && active_trigger_count == 0 && active_orchestration_count == 0,
        "in_flight_count": state.in_flight_count.load(std::sync::atomic::Ordering::SeqCst),
        "connected_client_count": state.connected_client_count.load(std::sync::atomic::Ordering::SeqCst),
        "active_trigger_count": active_trigger_count,
        "active_orchestration_count": active_orchestration_count,
        "human_control_active": human_control_active,
        "automation_scheduler": automation_scheduler,
        "browser_event_ingress": browser_event_ingress,
    }))
}
