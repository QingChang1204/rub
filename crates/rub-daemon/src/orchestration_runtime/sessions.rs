use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{OrchestrationSessionAvailability, OrchestrationSessionInfo};

use super::projection::orchestration_session_path_state;

fn orchestration_session_projection_upstream_truth(
    availability: OrchestrationSessionAvailability,
) -> &'static str {
    match availability {
        OrchestrationSessionAvailability::CurrentFallback => "current_session_runtime_authority",
        OrchestrationSessionAvailability::Addressable
        | OrchestrationSessionAvailability::BusyOrUnknown
        | OrchestrationSessionAvailability::ProtocolIncompatible
        | OrchestrationSessionAvailability::HardCutReleasePending
        | OrchestrationSessionAvailability::PendingStartup => "registry_authority_snapshot",
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn projected_orchestration_session(
    session_id: String,
    session_name: String,
    pid: u32,
    socket_path: String,
    current: bool,
    ipc_protocol_version: String,
    availability: OrchestrationSessionAvailability,
    user_data_dir: Option<String>,
) -> OrchestrationSessionInfo {
    let upstream_truth = orchestration_session_projection_upstream_truth(availability);
    OrchestrationSessionInfo {
        session_id,
        session_name,
        pid,
        socket_path,
        socket_path_state: Some(orchestration_session_path_state(
            "session.orchestration_runtime.known_sessions.socket_path",
            upstream_truth,
            "session_socket_reference",
        )),
        current,
        ipc_protocol_version,
        addressing_supported: matches!(
            availability,
            OrchestrationSessionAvailability::Addressable
                | OrchestrationSessionAvailability::CurrentFallback
        ),
        availability,
        user_data_dir: user_data_dir.clone(),
        user_data_dir_state: user_data_dir.map(|_| {
            orchestration_session_path_state(
                "session.orchestration_runtime.known_sessions.user_data_dir",
                upstream_truth,
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

pub(crate) fn orchestration_session_addressability_reason(
    session: &OrchestrationSessionInfo,
) -> Option<&'static str> {
    match session.availability {
        OrchestrationSessionAvailability::Addressable
        | OrchestrationSessionAvailability::CurrentFallback => None,
        OrchestrationSessionAvailability::BusyOrUnknown => Some("busy_or_unknown"),
        OrchestrationSessionAvailability::ProtocolIncompatible => Some("protocol_incompatible"),
        OrchestrationSessionAvailability::HardCutReleasePending => Some("hard_cut_release_pending"),
        OrchestrationSessionAvailability::PendingStartup => Some("pending_startup"),
    }
}

pub(crate) fn orchestration_session_not_addressable_error(
    session: &OrchestrationSessionInfo,
    code: ErrorCode,
    message: impl Into<String>,
    reason: &'static str,
    session_id_field: &'static str,
    session_name_field: &'static str,
) -> ErrorEnvelope {
    let mut context = serde_json::json!({
        "reason": reason,
        session_id_field: session.session_id,
        session_name_field: session.session_name,
        "availability": session.availability,
        "addressability_reason": orchestration_session_addressability_reason(session),
    });
    extend_orchestration_session_path_context(&mut context, session);
    ErrorEnvelope::new(code, message).with_context(context)
}
