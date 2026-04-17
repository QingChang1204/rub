use super::*;

pub(crate) async fn refresh_orchestration_runtime(state: &Arc<SessionState>) {
    let sequence = state.allocate_orchestration_runtime_sequence();
    let current_session = projected_current_orchestration_session(state);
    match load_registry_authority_snapshot(state.rub_home.clone()).await {
        Ok(snapshot) => {
            let mut known_sessions = Vec::new();
            let active_entries = snapshot.active_entries();
            let mut current_present = false;
            for entry in active_entries.iter().cloned() {
                let current = entry.session_id == state.session_id;
                current_present |= current;
                known_sessions.push(projected_orchestration_session(
                    entry.session_id,
                    entry.session_name,
                    entry.pid,
                    entry.socket_path,
                    current,
                    entry.ipc_protocol_version,
                    entry.user_data_dir,
                ));
            }
            let degraded_reason = if active_entries.is_empty() {
                Some("live_registry_empty".to_string())
            } else if current_present {
                None
            } else {
                Some("current_session_missing_from_live_registry".to_string())
            };
            if !current_present {
                known_sessions.push(current_session);
            }
            state
                .set_orchestration_runtime(sequence, known_sessions, degraded_reason)
                .await;
        }
        Err(error) => {
            state
                .mark_orchestration_runtime_degraded(
                    sequence,
                    current_session,
                    format!("registry_read_failed:{error}"),
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
        None,
    )
}

async fn load_registry_authority_snapshot(
    rub_home: PathBuf,
) -> Result<crate::session::RegistryAuthoritySnapshot, String> {
    tokio::task::spawn_blocking(move || crate::session::registry_authority_snapshot(&rub_home))
        .await
        .map_err(|error| format!("registry_refresh_join_failed:{error}"))?
        .map_err(|error| error.to_string())
}
