use super::*;
use crate::session::{RegistryEntryLiveness, RegistryEntrySnapshot, RegistrySessionSnapshot};
use rub_core::model::OrchestrationSessionAvailability;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryAuthoritySnapshotRefreshFailure {
    RegistryReadFailed,
}

impl RegistryAuthoritySnapshotRefreshFailure {
    fn degraded_reason(self) -> &'static str {
        "registry_read_failed"
    }
}

pub(crate) async fn refresh_orchestration_runtime(state: &Arc<SessionState>) {
    let sequence = state.allocate_orchestration_runtime_sequence();
    let current_session = projected_current_orchestration_session(state);
    match load_registry_authority_snapshot(state.rub_home.clone(), state.session_id.clone()).await {
        Ok(snapshot) => {
            let mut known_sessions = Vec::new();
            let mut current_present = false;
            let mut has_non_addressable_sessions = false;
            for session in &snapshot.sessions {
                let Some(entry_snapshot) = projected_registry_session_snapshot(session) else {
                    continue;
                };
                let current = entry_snapshot.entry.session_id == state.session_id;
                current_present |= current;
                let availability = orchestration_session_availability(entry_snapshot);
                has_non_addressable_sessions |=
                    !matches!(availability, OrchestrationSessionAvailability::Addressable);
                known_sessions.push(projected_orchestration_session(
                    entry_snapshot.entry.session_id.clone(),
                    entry_snapshot.entry.session_name.clone(),
                    entry_snapshot.entry.pid,
                    entry_snapshot.entry.socket_path.clone(),
                    current,
                    entry_snapshot.entry.ipc_protocol_version.clone(),
                    availability,
                    entry_snapshot.entry.user_data_dir.clone(),
                ));
            }
            let degraded_reason = if known_sessions.is_empty() {
                Some("live_registry_empty".to_string())
            } else if !current_present {
                Some("current_session_missing_from_live_registry".to_string())
            } else if has_non_addressable_sessions {
                Some("registry_contains_non_addressable_sessions".to_string())
            } else {
                None
            };
            if !current_present {
                known_sessions.push(current_session);
            }
            let addressing_supported = current_present;
            let execution_supported = true;
            state
                .set_orchestration_runtime(
                    sequence,
                    known_sessions,
                    addressing_supported,
                    execution_supported,
                    degraded_reason,
                )
                .await;
        }
        Err(error) => {
            state
                .mark_orchestration_runtime_degraded(
                    sequence,
                    current_session,
                    error.degraded_reason(),
                )
                .await;
        }
    }
}

fn projected_current_orchestration_session(
    state: &Arc<SessionState>,
) -> rub_core::model::OrchestrationSessionInfo {
    projected_orchestration_session(
        state.session_id.clone(),
        state.session_name.clone(),
        std::process::id(),
        state.socket_path().display().to_string(),
        true,
        rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        OrchestrationSessionAvailability::CurrentFallback,
        state.user_data_dir.clone(),
    )
}

fn projected_registry_session_snapshot(
    session: &RegistrySessionSnapshot,
) -> Option<&RegistryEntrySnapshot> {
    session.authoritative_entry().or_else(|| {
        session
            .entries
            .iter()
            .rev()
            .find(|entry| entry.is_pending_startup())
    })
}

fn orchestration_session_availability(
    entry_snapshot: &RegistryEntrySnapshot,
) -> OrchestrationSessionAvailability {
    match entry_snapshot.liveness {
        RegistryEntryLiveness::Live => OrchestrationSessionAvailability::Addressable,
        RegistryEntryLiveness::BusyOrUnknown => OrchestrationSessionAvailability::BusyOrUnknown,
        RegistryEntryLiveness::ProbeContractFailure => {
            OrchestrationSessionAvailability::BusyOrUnknown
        }
        RegistryEntryLiveness::ProtocolIncompatible => {
            OrchestrationSessionAvailability::ProtocolIncompatible
        }
        RegistryEntryLiveness::HardCutReleasePending => {
            OrchestrationSessionAvailability::HardCutReleasePending
        }
        RegistryEntryLiveness::PendingStartup => OrchestrationSessionAvailability::PendingStartup,
        RegistryEntryLiveness::Dead => OrchestrationSessionAvailability::CurrentFallback,
    }
}

async fn load_registry_authority_snapshot(
    rub_home: PathBuf,
    current_session_id: String,
) -> Result<crate::session::RegistryAuthoritySnapshot, RegistryAuthoritySnapshotRefreshFailure> {
    crate::session::registry_authority_snapshot_async(rub_home, Some(current_session_id))
        .await
        .map_err(|_| RegistryAuthoritySnapshotRefreshFailure::RegistryReadFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_snapshot_load_failure_uses_stable_reason() {
        let home = std::env::temp_dir().join(format!(
            "rub-registry-refresh-error-{}",
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_file(&home);
        std::fs::write(&home, b"not-a-directory").expect("create sentinel file");

        let error = load_registry_authority_snapshot(home.clone(), "current-session".to_string())
            .await
            .expect_err("file-backed home should fail registry refresh");
        assert_eq!(error.degraded_reason(), "registry_read_failed");

        std::fs::remove_file(&home).expect("cleanup sentinel file");
    }
}
