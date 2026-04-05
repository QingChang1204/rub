use std::path::PathBuf;
use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::model::{
    HumanVerificationHandoffStatus, InterferenceKind, InterferenceRuntimeInfo, LaunchPolicyInfo,
    OrchestrationSessionInfo, TabInfo,
};
use rub_core::port::BrowserPort;

use crate::session::SessionState;

pub(crate) async fn refresh_live_runtime_state(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    let sequence = state.allocate_runtime_state_sequence();
    match browser.probe_runtime_state().await {
        Ok(runtime_state) => {
            state
                .publish_runtime_state_snapshot(sequence, runtime_state)
                .await;
        }
        Err(error) => {
            state
                .mark_runtime_state_probe_degraded(sequence, error.to_string())
                .await;
        }
    }
}

pub(crate) async fn refresh_live_dialog_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    match browser.dialog_runtime().await {
        Ok(runtime) => {
            state.set_dialog_projection(0, runtime).await;
        }
        Err(error) => {
            state
                .mark_dialog_runtime_degraded(0, format!("dialog_probe_failed:{error}"))
                .await;
        }
    }
}

pub(crate) async fn refresh_live_frame_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    match browser.list_frames().await {
        Ok(frames) => {
            state.apply_frame_inventory(&frames).await;
        }
        Err(error) => {
            state
                .mark_frame_runtime_degraded(format!("frame_probe_failed:{error}"))
                .await;
        }
    }
}

pub(crate) async fn refresh_takeover_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    let launch_policy = browser.launch_policy();
    state.refresh_takeover_runtime(&launch_policy).await;
}

pub(crate) async fn refresh_orchestration_runtime(state: &Arc<SessionState>) {
    let sequence = state.allocate_orchestration_runtime_sequence();
    match load_registry_authority_snapshot(state.rub_home.clone()).await {
        Ok(snapshot) => {
            let mut known_sessions = Vec::new();
            for entry in snapshot.active_entries() {
                let current = entry.session_id == state.session_id;
                known_sessions.push(OrchestrationSessionInfo {
                    current,
                    session_id: entry.session_id,
                    session_name: entry.session_name,
                    pid: entry.pid,
                    socket_path: entry.socket_path,
                    ipc_protocol_version: entry.ipc_protocol_version,
                    user_data_dir: entry.user_data_dir,
                });
            }
            let degraded_reason = if known_sessions.is_empty() {
                Some("live_registry_empty".to_string())
            } else if known_sessions.iter().any(|session| session.current) {
                None
            } else {
                Some("current_session_missing_from_live_registry".to_string())
            };
            state
                .set_orchestration_runtime(sequence, known_sessions, degraded_reason)
                .await;
        }
        Err(error) => {
            state
                .mark_orchestration_runtime_degraded(
                    sequence,
                    format!("registry_read_failed:{error}"),
                )
                .await;
        }
    }
}

async fn load_registry_authority_snapshot(
    rub_home: PathBuf,
) -> Result<crate::session::RegistryAuthoritySnapshot, String> {
    tokio::task::spawn_blocking(move || crate::session::registry_authority_snapshot(&rub_home))
        .await
        .map_err(|error| format!("registry_refresh_join_failed:{error}"))?
        .map_err(|error| error.to_string())
}

pub(crate) async fn refresh_live_storage_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) {
    let selected_frame_id = state.selected_frame_id().await;
    match browser
        .storage_snapshot(selected_frame_id.as_deref(), None)
        .await
    {
        Ok(snapshot) => {
            state.set_storage_snapshot(snapshot).await;
        }
        Err(error) => {
            state
                .mark_storage_runtime_degraded(format!("storage_probe_failed:{error}"))
                .await;
        }
    }
}

pub(crate) async fn refresh_live_trigger_runtime(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) -> Result<Vec<TabInfo>, RubError> {
    match browser.list_tabs().await {
        Ok(tabs) => {
            state.reconcile_trigger_runtime(&tabs).await;
            state.clear_trigger_runtime_degraded().await;
            Ok(tabs)
        }
        Err(error) => {
            state
                .mark_trigger_runtime_degraded(format!("tab_probe_failed:{error}"))
                .await;
            Err(error)
        }
    }
}

pub(crate) async fn refresh_live_interference_state(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) -> Result<Vec<TabInfo>, RubError> {
    let launch_policy = browser.launch_policy();
    match browser.list_tabs().await {
        Ok(tabs) => {
            let runtime = state.classify_interference_runtime(&tabs).await;
            apply_policy_driven_handoff(state, &runtime, &launch_policy).await;
            Ok(tabs)
        }
        Err(error) => {
            state
                .mark_interference_runtime_degraded(format!("tab_probe_failed:{error}"))
                .await;
            Err(error)
        }
    }
}

pub(crate) async fn refresh_live_runtime_and_interference(
    browser: &Arc<dyn BrowserPort>,
    state: &Arc<SessionState>,
) -> Result<Vec<TabInfo>, RubError> {
    refresh_live_runtime_state(browser, state).await;
    refresh_live_frame_runtime(browser, state).await;
    refresh_live_storage_runtime(browser, state).await;
    refresh_takeover_runtime(browser, state).await;
    refresh_live_interference_state(browser, state).await
}

async fn apply_policy_driven_handoff(
    state: &Arc<SessionState>,
    runtime: &InterferenceRuntimeInfo,
    launch_policy: &LaunchPolicyInfo,
) {
    let should_escalate = matches!(
        runtime
            .current_interference
            .as_ref()
            .map(|current| current.kind),
        Some(InterferenceKind::HumanVerificationRequired)
    ) && runtime
        .active_policies
        .iter()
        .any(|policy| policy == "handoff_escalation");
    if !should_escalate {
        return;
    }

    let handoff = state.human_verification_handoff().await;
    if matches!(
        handoff.status,
        HumanVerificationHandoffStatus::Available | HumanVerificationHandoffStatus::Completed
    ) {
        state.activate_handoff().await;
        state.refresh_takeover_runtime(launch_policy).await;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use super::{apply_policy_driven_handoff, refresh_orchestration_runtime};
    use crate::rub_paths::RubPaths;
    use crate::session::SessionState;
    use crate::session::{RegistryData, RegistryEntry, write_registry};
    use rub_core::model::{
        HumanVerificationHandoffStatus, InterferenceKind, InterferenceMode,
        InterferenceObservation, InterferenceRuntimeInfo, InterferenceRuntimeStatus,
        LaunchPolicyInfo, OrchestrationRuntimeStatus, TakeoverRuntimeStatus,
    };
    use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest, IpcResponse};
    use uuid::Uuid;

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("rub-runtime-refresh-test-{}", Uuid::now_v7()))
    }

    struct LiveRegistryFixture {
        socket_path: PathBuf,
        server: JoinHandle<()>,
    }

    fn headed_launch_policy() -> LaunchPolicyInfo {
        LaunchPolicyInfo {
            headless: false,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: None,
            connection_target: None,
            stealth_level: None,
            stealth_patches: None,
            stealth_default_enabled: None,
            humanize_enabled: None,
            humanize_speed: None,
            stealth_coverage: None,
        }
    }

    impl LiveRegistryFixture {
        fn join(self) {
            self.server.join().unwrap();
            let _ = std::fs::remove_file(self.socket_path);
        }
    }

    fn install_live_registry_entry(home: &PathBuf, entry: &RegistryEntry) -> LiveRegistryFixture {
        let runtime = RubPaths::new(home).session_runtime(&entry.session_name, &entry.session_id);
        let projection = RubPaths::new(home).session(&entry.session_name);
        std::fs::create_dir_all(runtime.session_dir()).unwrap();
        std::fs::create_dir_all(projection.projection_dir()).unwrap();
        std::fs::write(runtime.pid_path(), entry.pid.to_string()).unwrap();
        std::fs::write(projection.canonical_pid_path(), entry.pid.to_string()).unwrap();
        std::fs::write(
            projection.startup_committed_path(),
            entry.session_id.as_bytes(),
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(runtime.socket_path(), projection.canonical_socket_path())
            .unwrap();

        let _ = std::fs::remove_file(runtime.socket_path());
        let listener = std::os::unix::net::UnixListener::bind(runtime.socket_path()).unwrap();
        listener.set_nonblocking(false).unwrap();
        let session_id = entry.session_id.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let decoded: IpcRequest = serde_json::from_str(request.trim_end()).unwrap();
            assert_eq!(decoded.command, "_handshake");
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": session_id,
                }),
            );
            serde_json::to_writer(&mut stream, &response).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        LiveRegistryFixture {
            socket_path: runtime.socket_path(),
            server,
        }
    }

    #[tokio::test]
    async fn policy_driven_handoff_activates_when_available() {
        let state = Arc::new(SessionState::new("default", temp_home(), None));
        state.set_handoff_available(true).await;

        let runtime = InterferenceRuntimeInfo {
            mode: InterferenceMode::PublicWebStable,
            status: InterferenceRuntimeStatus::Active,
            current_interference: Some(InterferenceObservation {
                kind: InterferenceKind::HumanVerificationRequired,
                summary: "human verification checkpoint detected".to_string(),
                current_url: Some("https://example.com/challenge".to_string()),
                primary_url: Some("https://example.com".to_string()),
            }),
            active_policies: vec![
                "safe_recovery".to_string(),
                "handoff_escalation".to_string(),
            ],
            handoff_required: true,
            ..InterferenceRuntimeInfo::default()
        };

        apply_policy_driven_handoff(&state, &runtime, &headed_launch_policy()).await;

        let handoff = state.human_verification_handoff().await;
        let takeover = state.takeover_runtime().await;
        assert_eq!(handoff.status, HumanVerificationHandoffStatus::Active);
        assert!(handoff.automation_paused);
        assert_eq!(takeover.status, TakeoverRuntimeStatus::Active);
        assert!(takeover.automation_paused);
    }

    #[tokio::test]
    async fn policy_driven_handoff_does_not_activate_when_unavailable() {
        let state = Arc::new(SessionState::new("default", temp_home(), None));

        let runtime = InterferenceRuntimeInfo {
            mode: InterferenceMode::PublicWebStable,
            status: InterferenceRuntimeStatus::Active,
            current_interference: Some(InterferenceObservation {
                kind: InterferenceKind::HumanVerificationRequired,
                summary: "human verification checkpoint detected".to_string(),
                current_url: Some("https://example.com/challenge".to_string()),
                primary_url: Some("https://example.com".to_string()),
            }),
            active_policies: vec![
                "safe_recovery".to_string(),
                "handoff_escalation".to_string(),
            ],
            handoff_required: true,
            ..InterferenceRuntimeInfo::default()
        };

        apply_policy_driven_handoff(&state, &runtime, &headed_launch_policy()).await;

        let handoff = state.human_verification_handoff().await;
        assert_eq!(handoff.status, HumanVerificationHandoffStatus::Unavailable);
        assert!(!handoff.automation_paused);
    }

    #[tokio::test]
    async fn policy_driven_handoff_respects_mode_without_handoff_escalation() {
        let state = Arc::new(SessionState::new("default", temp_home(), None));
        state.set_handoff_available(true).await;

        let runtime = InterferenceRuntimeInfo {
            mode: InterferenceMode::Normal,
            status: InterferenceRuntimeStatus::Active,
            current_interference: Some(InterferenceObservation {
                kind: InterferenceKind::HumanVerificationRequired,
                summary: "human verification checkpoint detected".to_string(),
                current_url: Some("https://example.com/challenge".to_string()),
                primary_url: Some("https://example.com".to_string()),
            }),
            active_policies: Vec::new(),
            handoff_required: true,
            ..InterferenceRuntimeInfo::default()
        };

        apply_policy_driven_handoff(&state, &runtime, &headed_launch_policy()).await;

        let handoff = state.human_verification_handoff().await;
        assert_eq!(handoff.status, HumanVerificationHandoffStatus::Available);
        assert!(!handoff.automation_paused);
    }

    #[tokio::test]
    async fn orchestration_refresh_projects_registry_backed_session_identity() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();
        let entry = RegistryEntry {
            session_id: state.session_id.clone(),
            session_name: state.session_name.clone(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime(&state.session_name, &state.session_id)
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-03-31T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let fixture = install_live_registry_entry(&home, &entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Active);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(runtime.session_count, 1);
        assert_eq!(
            runtime.current_session_id.as_deref(),
            Some(state.session_id.as_str())
        );
        assert_eq!(runtime.known_sessions[0].session_id, state.session_id);
        assert!(runtime.known_sessions[0].current);

        let _ = std::fs::remove_dir_all(home);
        fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_degrades_when_current_session_is_missing() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();
        let entry = RegistryEntry {
            session_id: "sess-other".to_string(),
            session_name: "other".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("other", "sess-other")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-03-31T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let fixture = install_live_registry_entry(&home, &entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("current_session_missing_from_live_registry")
        );
        assert_eq!(runtime.known_sessions.len(), 1);
        assert_eq!(runtime.known_sessions[0].session_id, "sess-other");
        assert!(!runtime.known_sessions[0].current);

        let _ = std::fs::remove_dir_all(home);
        fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_current_session_when_live_authority_is_committed() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();
        let entry = RegistryEntry {
            session_id: state.session_id.clone(),
            session_name: state.session_name.clone(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime(&state.session_name, &state.session_id)
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-03-31T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let fixture = install_live_registry_entry(&home, &entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Active);
        assert_eq!(runtime.session_count, 1);
        assert_eq!(runtime.known_sessions.len(), 1);
        assert!(runtime.known_sessions[0].current);
        assert_eq!(runtime.known_sessions[0].session_id, state.session_id);

        let _ = std::fs::remove_dir_all(home);
        fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_fails_closed_when_live_registry_is_empty() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();
        write_registry(
            &home,
            &RegistryData {
                sessions: Vec::new(),
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;
        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert!(!runtime.addressing_supported);
        assert!(!runtime.execution_supported);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("live_registry_empty")
        );

        let _ = std::fs::remove_dir_all(home);
    }
}
