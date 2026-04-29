use std::path::PathBuf;
use std::sync::Arc;

use rub_core::error::RubError;
use rub_core::model::{
    HumanVerificationHandoffStatus, InterferenceKind, InterferenceRuntimeInfo, LaunchPolicyInfo,
    ReadinessInfo, TabInfo,
};
use rub_core::port::BrowserPort;

use crate::orchestration_runtime::projected_orchestration_session;
use crate::session::SessionState;

mod interference;
mod orchestration;
mod surfaces;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefreshOutcomeStatus {
    Refreshed,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RefreshOutcome {
    pub(crate) surface: &'static str,
    pub(crate) status: RefreshOutcomeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) degraded_reason: Option<String>,
}

impl RefreshOutcome {
    pub(crate) fn refreshed(surface: &'static str) -> Self {
        Self {
            surface,
            status: RefreshOutcomeStatus::Refreshed,
            degraded_reason: None,
        }
    }

    pub(crate) fn degraded(surface: &'static str, reason: impl Into<String>) -> Self {
        Self {
            surface,
            status: RefreshOutcomeStatus::Degraded,
            degraded_reason: Some(reason.into()),
        }
    }
}

#[cfg(test)]
use interference::apply_policy_driven_handoff;
pub(crate) use interference::{
    InterferenceRefreshIntent, InterferenceRefreshSnapshot, refresh_live_interference_state,
    refresh_live_runtime_and_interference, refresh_live_runtime_and_interference_snapshot,
};
pub(crate) use orchestration::refresh_orchestration_runtime;
pub(crate) use surfaces::{
    refresh_live_dialog_runtime, refresh_live_frame_runtime, refresh_live_runtime_state,
    refresh_live_storage_runtime, refresh_live_trigger_runtime, refresh_takeover_runtime,
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{
        InterferenceRefreshIntent, apply_policy_driven_handoff, refresh_orchestration_runtime,
    };
    use crate::rub_paths::RubPaths;
    use crate::session::SessionState;
    use crate::session::{RegistryData, RegistryEntry, write_registry};
    use rub_core::model::{
        HumanVerificationHandoffStatus, InterferenceKind, InterferenceMode,
        InterferenceObservation, InterferenceRuntimeInfo, InterferenceRuntimeStatus,
        LaunchPolicyInfo, OrchestrationRuntimeStatus, OrchestrationSessionAvailability,
        TakeoverRuntimeStatus,
    };
    use rub_ipc::protocol::IPC_PROTOCOL_VERSION;
    use uuid::Uuid;

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("rub-runtime-refresh-test-{}", Uuid::now_v7()))
    }

    struct LiveRegistryFixture {
        socket_path: PathBuf,
    }

    #[derive(Clone, Copy)]
    enum RegistryHandshakeBehavior {
        Live,
        ProtocolIncompatible,
        ProbeContractFailure,
        BusyOrUnknown,
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
            let _ = std::fs::remove_file(self.socket_path);
        }
    }

    fn install_registry_entry_with_handshake(
        home: &PathBuf,
        entry: &RegistryEntry,
        behavior: RegistryHandshakeBehavior,
    ) -> LiveRegistryFixture {
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
        if let Some(parent) = runtime.socket_path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(runtime.socket_path(), b"socket").unwrap();
        match behavior {
            RegistryHandshakeBehavior::Live => {
                crate::session::force_live_registry_socket_probe_once_for_test(
                    &runtime.socket_path(),
                );
            }
            RegistryHandshakeBehavior::ProtocolIncompatible => {
                crate::session::force_protocol_incompatible_registry_socket_probe_once_for_test(
                    &runtime.socket_path(),
                );
            }
            RegistryHandshakeBehavior::ProbeContractFailure => {
                crate::session::force_probe_contract_failure_registry_socket_probe_once_for_test(
                    &runtime.socket_path(),
                );
            }
            RegistryHandshakeBehavior::BusyOrUnknown => {
                crate::session::force_busy_registry_socket_probe_once_for_test(
                    &runtime.socket_path(),
                );
            }
        }

        LiveRegistryFixture {
            socket_path: runtime.socket_path(),
        }
    }

    fn install_live_registry_entry(home: &PathBuf, entry: &RegistryEntry) -> LiveRegistryFixture {
        install_registry_entry_with_handshake(home, entry, RegistryHandshakeBehavior::Live)
    }

    fn install_protocol_incompatible_registry_entry(
        home: &PathBuf,
        entry: &RegistryEntry,
    ) -> LiveRegistryFixture {
        install_registry_entry_with_handshake(
            home,
            entry,
            RegistryHandshakeBehavior::ProtocolIncompatible,
        )
    }

    fn install_busy_registry_entry(home: &PathBuf, entry: &RegistryEntry) -> LiveRegistryFixture {
        install_registry_entry_with_handshake(home, entry, RegistryHandshakeBehavior::BusyOrUnknown)
    }

    fn install_probe_contract_failure_registry_entry(
        home: &PathBuf,
        entry: &RegistryEntry,
    ) -> LiveRegistryFixture {
        install_registry_entry_with_handshake(
            home,
            entry,
            RegistryHandshakeBehavior::ProbeContractFailure,
        )
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
        assert_eq!(
            runtime.known_sessions[0]
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("registry_authority_snapshot")
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
        assert_eq!(runtime.known_sessions.len(), 2);
        assert!(
            runtime
                .known_sessions
                .iter()
                .any(|session| session.session_id == "sess-other" && !session.current)
        );
        assert!(
            runtime
                .known_sessions
                .iter()
                .any(|session| session.session_id == state.session_id && session.current)
        );
        assert!(runtime.execution_supported);
        let current = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == state.session_id)
            .expect("current fallback session should remain visible");
        assert_eq!(
            current
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("current_session_runtime_authority")
        );

        let _ = std::fs::remove_dir_all(home);
        fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_prioritizes_missing_current_session_over_non_addressable_remote_sessions()
     {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let remote_entry = RegistryEntry {
            session_id: "sess-remote".to_string(),
            session_name: "remote".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("remote", "sess-remote")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let remote_fixture = install_protocol_incompatible_registry_entry(&home, &remote_entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![remote_entry],
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
        assert_eq!(
            runtime.current_session_id.as_deref(),
            Some(state.session_id.as_str())
        );
        assert_eq!(
            runtime.current_session_name.as_deref(),
            Some(state.session_name.as_str())
        );
        let remote = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == "sess-remote")
            .expect("remote session should remain visible");
        assert_eq!(
            remote.availability,
            OrchestrationSessionAvailability::ProtocolIncompatible
        );
        let current = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == state.session_id)
            .expect("current fallback session should remain visible");
        assert_eq!(
            current
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("current_session_runtime_authority")
        );

        let _ = std::fs::remove_dir_all(home);
        remote_fixture.join();
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
        assert_eq!(
            runtime.known_sessions[0]
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("registry_authority_snapshot")
        );

        let _ = std::fs::remove_dir_all(home);
        fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_fails_closed_when_live_registry_is_empty() {
        let home = temp_home();
        let state = Arc::new(SessionState::new(
            "default",
            home.clone(),
            Some("/tmp/rub-current-profile".to_string()),
        ));
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
        assert!(runtime.execution_supported);
        assert_eq!(runtime.known_sessions.len(), 1);
        assert!(runtime.known_sessions[0].current);
        assert_eq!(runtime.known_sessions[0].session_id, state.session_id);
        assert_eq!(
            runtime.known_sessions[0].user_data_dir.as_deref(),
            Some("/tmp/rub-current-profile")
        );
        assert_eq!(
            runtime.known_sessions[0]
                .user_data_dir_state
                .as_ref()
                .map(|state| state.path_kind.as_str()),
            Some("managed_user_data_directory")
        );
        assert_eq!(
            runtime.known_sessions[0]
                .socket_path_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("current_session_runtime_authority")
        );
        assert_eq!(
            runtime.known_sessions[0]
                .user_data_dir_state
                .as_ref()
                .map(|state| state.upstream_truth.as_str()),
            Some("current_session_runtime_authority")
        );
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("live_registry_empty")
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_protocol_incompatible_sessions_visible_and_non_addressable()
     {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let current_entry = RegistryEntry {
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
        let remote_entry = RegistryEntry {
            session_id: "sess-remote".to_string(),
            session_name: "remote".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("remote", "sess-remote")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let current_fixture = install_live_registry_entry(&home, &current_entry);
        let remote_fixture = install_protocol_incompatible_registry_entry(&home, &remote_entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![current_entry, remote_entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;

        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("registry_contains_non_addressable_sessions")
        );
        assert_eq!(runtime.known_sessions.len(), 2);
        let remote = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == "sess-remote")
            .expect("protocol-incompatible session should remain visible");
        assert_eq!(
            remote.availability,
            OrchestrationSessionAvailability::ProtocolIncompatible
        );
        assert!(!remote.addressing_supported);

        let _ = std::fs::remove_dir_all(&home);
        current_fixture.join();
        remote_fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_busy_sessions_visible_and_non_addressable() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let current_entry = RegistryEntry {
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
        let remote_entry = RegistryEntry {
            session_id: "sess-busy".to_string(),
            session_name: "busy".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("busy", "sess-busy")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let current_fixture = install_live_registry_entry(&home, &current_entry);
        let remote_fixture = install_busy_registry_entry(&home, &remote_entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![current_entry, remote_entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;

        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("registry_contains_non_addressable_sessions")
        );
        let remote = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == "sess-busy")
            .expect("busy session should remain visible");
        assert_eq!(
            remote.availability,
            OrchestrationSessionAvailability::BusyOrUnknown
        );
        assert!(!remote.addressing_supported);

        let _ = std::fs::remove_dir_all(&home);
        current_fixture.join();
        remote_fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_large_busy_registries_within_one_probe_budget_window() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let current_entry = RegistryEntry {
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
        let busy_entries = (0..9)
            .map(|index| RegistryEntry {
                session_id: format!("sess-busy-{index}"),
                session_name: format!("busy-{index}"),
                pid: std::process::id(),
                socket_path: RubPaths::new(&home)
                    .session_runtime(format!("busy-{index}"), format!("sess-busy-{index}"))
                    .socket_path()
                    .display()
                    .to_string(),
                created_at: format!("2026-04-01T00:00:{index:02}Z"),
                ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            })
            .collect::<Vec<_>>();
        let current_fixture = install_live_registry_entry(&home, &current_entry);
        let busy_fixtures = busy_entries
            .iter()
            .map(|entry| install_busy_registry_entry(&home, entry))
            .collect::<Vec<_>>();
        let mut sessions = vec![current_entry];
        sessions.extend(busy_entries);
        write_registry(&home, &RegistryData { sessions }).unwrap();

        let started = Instant::now();
        refresh_orchestration_runtime(&state).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(1_600),
            "registry liveness probing should stay within a single busy-socket budget window even for larger registries instead of serializing busy sessions inside one refresh: {elapsed:?}"
        );

        let _ = std::fs::remove_dir_all(&home);
        current_fixture.join();
        for fixture in busy_fixtures {
            fixture.join();
        }
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_late_live_entries_addressable_beyond_old_probe_cap() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let current_entry = RegistryEntry {
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
        let busy_entries = (0..8)
            .map(|index| RegistryEntry {
                session_id: format!("sess-busy-{index}"),
                session_name: format!("busy-{index}"),
                pid: std::process::id(),
                socket_path: RubPaths::new(&home)
                    .session_runtime(format!("busy-{index}"), format!("sess-busy-{index}"))
                    .socket_path()
                    .display()
                    .to_string(),
                created_at: format!("2026-04-01T00:00:{index:02}Z"),
                ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
                user_data_dir: None,
                attachment_identity: None,
                connection_target: None,
            })
            .collect::<Vec<_>>();
        let remote_entry = RegistryEntry {
            session_id: "sess-remote-live".to_string(),
            session_name: "remote-live".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("remote-live", "sess-remote-live")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:01:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };

        let current_fixture = install_live_registry_entry(&home, &current_entry);
        let busy_fixtures = busy_entries
            .iter()
            .map(|entry| install_busy_registry_entry(&home, entry))
            .collect::<Vec<_>>();
        let remote_fixture = install_live_registry_entry(&home, &remote_entry);

        let mut sessions = vec![current_entry];
        sessions.extend(busy_entries);
        sessions.push(remote_entry);
        write_registry(&home, &RegistryData { sessions }).unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;
        let remote = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == "sess-remote-live")
            .expect("late live session should remain visible");
        assert_eq!(
            remote.availability,
            OrchestrationSessionAvailability::Addressable
        );
        assert!(remote.addressing_supported);

        let _ = std::fs::remove_dir_all(&home);
        current_fixture.join();
        for fixture in busy_fixtures {
            fixture.join();
        }
        remote_fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_probe_contract_failure_sessions_visible_and_non_addressable()
     {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let current_entry = RegistryEntry {
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
        let remote_entry = RegistryEntry {
            session_id: "sess-probe-failure".to_string(),
            session_name: "probe-failure".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("probe-failure", "sess-probe-failure")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let current_fixture = install_live_registry_entry(&home, &current_entry);
        let remote_fixture = install_probe_contract_failure_registry_entry(&home, &remote_entry);
        write_registry(
            &home,
            &RegistryData {
                sessions: vec![current_entry, remote_entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;

        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("registry_contains_non_addressable_sessions")
        );
        let remote = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == "sess-probe-failure")
            .expect("probe-contract-failure session should remain visible");
        assert_eq!(
            remote.availability,
            OrchestrationSessionAvailability::BusyOrUnknown
        );
        assert!(!remote.addressing_supported);

        let _ = std::fs::remove_dir_all(&home);
        current_fixture.join();
        remote_fixture.join();
    }

    #[tokio::test]
    async fn orchestration_refresh_keeps_pending_startup_sessions_visible_and_non_addressable() {
        let home = temp_home();
        let state = Arc::new(SessionState::new("default", home.clone(), None));
        std::fs::create_dir_all(&home).unwrap();

        let current_entry = RegistryEntry {
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
        let pending_entry = RegistryEntry {
            session_id: "sess-pending".to_string(),
            session_name: "pending".to_string(),
            pid: std::process::id(),
            socket_path: RubPaths::new(&home)
                .session_runtime("pending", "sess-pending")
                .socket_path()
                .display()
                .to_string(),
            created_at: "2026-04-01T00:00:00Z".to_string(),
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            user_data_dir: None,
            attachment_identity: None,
            connection_target: None,
        };
        let current_fixture = install_live_registry_entry(&home, &current_entry);
        let pending_runtime = RubPaths::new(&home).session_runtime("pending", "sess-pending");
        std::fs::create_dir_all(pending_runtime.session_dir()).unwrap();
        std::fs::write(pending_runtime.pid_path(), std::process::id().to_string()).unwrap();
        std::fs::write(pending_runtime.socket_path(), b"socket").unwrap();

        write_registry(
            &home,
            &RegistryData {
                sessions: vec![current_entry, pending_entry],
            },
        )
        .unwrap();

        refresh_orchestration_runtime(&state).await;
        let runtime = state.orchestration_runtime().await;

        assert_eq!(runtime.status, OrchestrationRuntimeStatus::Degraded);
        assert!(runtime.addressing_supported);
        assert!(runtime.execution_supported);
        assert_eq!(
            runtime.degraded_reason.as_deref(),
            Some("registry_contains_non_addressable_sessions")
        );
        let pending = runtime
            .known_sessions
            .iter()
            .find(|session| session.session_id == "sess-pending")
            .expect("pending-startup session should remain visible");
        assert_eq!(
            pending.availability,
            OrchestrationSessionAvailability::PendingStartup
        );
        assert!(!pending.addressing_supported);

        let _ = std::fs::remove_dir_all(&home);
        current_fixture.join();
    }
}
