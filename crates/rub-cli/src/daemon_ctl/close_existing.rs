use crate::timeout_budget::helpers::mutating_request;
use rub_core::error::{ErrorCode, RubError};
use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    DaemonConnection, ExistingCloseOutcome, TransientSocketPolicy, detect_or_connect_hardened,
    send_existing_request_with_replay_recovery,
};

pub async fn close_existing_session(
    rub_home: &Path,
    session_name: &str,
    timeout_ms: u64,
) -> Result<ExistingCloseOutcome, RubError> {
    if !rub_home.exists() {
        return Ok(ExistingCloseOutcome::Noop);
    }

    let (mut client, daemon_session_id) = match detect_or_connect_hardened(
        rub_home,
        session_name,
        TransientSocketPolicy::FailAfterLock,
    )
    .await?
    {
        DaemonConnection::Connected {
            client,
            daemon_session_id,
        } => (client, daemon_session_id),
        DaemonConnection::NeedStart => return Ok(ExistingCloseOutcome::Noop),
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let request = mutating_request("close", serde_json::json!({}), timeout_ms.max(1));
    let response = send_existing_request_with_replay_recovery(
        &mut client,
        &request,
        deadline,
        rub_home,
        session_name,
        daemon_session_id.as_deref(),
    )
    .await
    .map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!("Failed to close existing session '{session_name}': {error}"),
            serde_json::json!({
                "session": session_name,
                "daemon_session_id": daemon_session_id,
                "command_id": request.command_id,
            }),
        )
    })?;
    Ok(ExistingCloseOutcome::Closed(Box::new(response)))
}
