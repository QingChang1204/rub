use super::projection::{annotate_doctor_operator_path_states, runtime_projection_state};
use super::surface::runtime_summary;
use super::*;
use crate::runtime_refresh::{
    InterferenceRefreshIntent, refresh_live_dialog_runtime, refresh_live_frame_runtime,
    refresh_live_interference_state, refresh_live_runtime_state, refresh_live_storage_runtime,
    refresh_live_trigger_runtime, refresh_orchestration_runtime, refresh_takeover_runtime,
};

pub(crate) async fn cmd_doctor(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let browser_healthy = router.browser.health_check().await.is_ok();
    let refresh_outcomes = vec![
        refresh_live_runtime_state(&router.browser, state).await,
        refresh_live_dialog_runtime(&router.browser, state).await,
        refresh_live_frame_runtime(&router.browser, state).await,
        refresh_live_storage_runtime(&router.browser, state).await,
        refresh_takeover_runtime(&router.browser, state).await,
    ];
    refresh_orchestration_runtime(state).await;
    let _ = refresh_live_trigger_runtime(&router.browser, state).await;
    let _ = refresh_live_interference_state(
        &router.browser,
        state,
        InterferenceRefreshIntent::ReadOnly,
    )
    .await;
    let launch_policy = router.browser.launch_policy();
    let report = crate::health::build_report(
        &state.session_id,
        &state.session_name,
        &state.rub_home,
        true,
    );
    let detection_risks = detection_risks(&launch_policy);
    let automation_scheduler = state.automation_scheduler_metrics().await;
    let browser_event_ingress = state.browser_event_ingress_metrics().await;
    let mut result = serde_json::json!({
        "browser": {
            "found": report.browser_found,
            "path": report.browser_path,
            "version": report.browser_version,
            "healthy": browser_healthy,
        },
        "daemon": {
            "running": true,
            "pid": std::process::id(),
            "session_id": report.session_id,
            "session_name": report.session_name,
            "uptime_seconds": state.uptime_seconds(),
            "in_flight": state.in_flight_count.load(std::sync::atomic::Ordering::SeqCst),
        },
        "socket": {
            "path": state.socket_path(),
            "healthy": true,
        },
        "disk": {
            "rub_home": report.rub_home,
            "log_size_mb": report.daemon_log_size_mb,
        },
        "versions": {
            "rub": report.rub_version,
            "ipc_protocol_version": report.ipc_protocol_version,
        },
        "launch_policy": launch_policy,
        "capabilities": agent_capabilities(),
        "dom_epoch": state.current_epoch(),
        "detection_risks": detection_risks,
        "automation_scheduler": automation_scheduler,
        "browser_event_ingress": browser_event_ingress,
        "post_commit_journal": state.post_commit_journal_projection(),
        "refresh_outcomes": refresh_outcomes,
    });
    annotate_doctor_operator_path_states(&mut result);

    Ok(serde_json::json!({
        "subject": {
            "kind": "session_diagnostics",
            "session_id": report.session_id,
            "session_name": report.session_name,
        },
        "result": result,
        "runtime": runtime_summary(state).await,
        "runtime_projection_state": runtime_projection_state("summary", "session.runtime_summary"),
    }))
}
