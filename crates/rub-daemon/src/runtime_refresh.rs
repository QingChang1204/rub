use std::path::PathBuf;
use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::model::{
    HumanVerificationHandoffStatus, InterferenceKind, InterferenceRuntimeInfo, LaunchPolicyInfo,
    TabInfo,
};
use rub_core::port::BrowserPort;

use crate::orchestration_runtime::projected_orchestration_session;
use crate::session::SessionState;

mod interference;
mod orchestration;
mod surfaces;

#[cfg(test)]
use interference::apply_policy_driven_handoff;
pub(crate) use interference::{
    InterferenceRefreshIntent, refresh_live_interference_state,
    refresh_live_runtime_and_interference,
};
pub(crate) use orchestration::refresh_orchestration_runtime;
pub(crate) use surfaces::{
    refresh_live_dialog_runtime, refresh_live_frame_runtime, refresh_live_runtime_state,
    refresh_live_storage_runtime, refresh_live_trigger_runtime, refresh_takeover_runtime,
};

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use super::{
        InterferenceRefreshIntent, apply_policy_driven_handoff, refresh_orchestration_runtime,
    };
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

        apply_policy_driven_handoff(
            InterferenceRefreshIntent::PolicyDriven,
            &state,
            &runtime,
            &headed_launch_policy(),
        )
        .await;

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

        apply_policy_driven_handoff(
            InterferenceRefreshIntent::PolicyDriven,
            &state,
            &runtime,
            &headed_launch_policy(),
        )
        .await;

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

        apply_policy_driven_handoff(
            InterferenceRefreshIntent::PolicyDriven,
            &state,
            &runtime,
            &headed_launch_policy(),
        )
        .await;

        let handoff = state.human_verification_handoff().await;
        assert_eq!(handoff.status, HumanVerificationHandoffStatus::Available);
        assert!(!handoff.automation_paused);
    }

    #[tokio::test]
    async fn read_only_interference_refresh_does_not_activate_policy_handoff() {
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

        apply_policy_driven_handoff(
            InterferenceRefreshIntent::ReadOnly,
            &state,
            &runtime,
            &headed_launch_policy(),
        )
        .await;

        let handoff = state.human_verification_handoff().await;
        let takeover = state.takeover_runtime().await;
        assert_eq!(handoff.status, HumanVerificationHandoffStatus::Available);
        assert!(!handoff.automation_paused);
        assert_eq!(takeover.status, TakeoverRuntimeStatus::Unavailable);
        assert!(!takeover.automation_paused);
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
        assert_eq!(
            runtime.known_sessions[0]
                .socket_path_state
                .as_ref()
                .map(|state| state.path_kind.as_str()),
            Some("session_socket_reference")
        );

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
        assert_eq!(
            runtime.known_sessions[0]
                .socket_path_state
                .as_ref()
                .map(|state| state.path_authority.as_str()),
            Some("session.orchestration_runtime.known_sessions.socket_path")
        );

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
