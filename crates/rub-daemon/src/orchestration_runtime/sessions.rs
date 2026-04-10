use rub_core::model::OrchestrationSessionInfo;

use super::projection::orchestration_session_path_state;

pub(crate) fn projected_orchestration_session(
    session_id: String,
    session_name: String,
    pid: u32,
    socket_path: String,
    current: bool,
    ipc_protocol_version: String,
    user_data_dir: Option<String>,
) -> OrchestrationSessionInfo {
    OrchestrationSessionInfo {
        session_id,
        session_name,
        pid,
        socket_path,
        socket_path_state: Some(orchestration_session_path_state(
            "session.orchestration_runtime.known_sessions.socket_path",
            "registry_authority_snapshot",
            "session_socket_reference",
        )),
        current,
        ipc_protocol_version,
        user_data_dir: user_data_dir.clone(),
        user_data_dir_state: user_data_dir.map(|_| {
            orchestration_session_path_state(
                "session.orchestration_runtime.known_sessions.user_data_dir",
                "registry_authority_snapshot",
                "managed_user_data_directory",
            )
        }),
    }
}

pub(crate) fn extend_orchestration_session_path_context(
    context: &mut serde_json::Value,
    session: &OrchestrationSessionInfo,
) {
    let Some(object) = context.as_object_mut() else {
        return;
    };
    object.insert(
        "socket_path".to_string(),
        serde_json::Value::String(session.socket_path.clone()),
    );
    if let Some(state) = session.socket_path_state.as_ref() {
        object.insert(
            "socket_path_state".to_string(),
            serde_json::to_value(state).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(user_data_dir) = session.user_data_dir.as_ref() {
        object.insert(
            "user_data_dir".to_string(),
            serde_json::Value::String(user_data_dir.clone()),
        );
    }
    if let Some(state) = session.user_data_dir_state.as_ref() {
        object.insert(
            "user_data_dir_state".to_string(),
            serde_json::to_value(state).unwrap_or(serde_json::Value::Null),
        );
    }
}
