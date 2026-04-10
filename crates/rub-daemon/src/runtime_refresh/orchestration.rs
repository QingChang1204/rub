use super::*;

pub(crate) async fn refresh_orchestration_runtime(state: &Arc<SessionState>) {
    let sequence = state.allocate_orchestration_runtime_sequence();
    match load_registry_authority_snapshot(state.rub_home.clone()).await {
        Ok(snapshot) => {
            let mut known_sessions = Vec::new();
            for entry in snapshot.active_entries() {
                let current = entry.session_id == state.session_id;
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
