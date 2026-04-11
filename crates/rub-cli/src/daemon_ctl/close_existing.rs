use crate::timeout_budget::helpers::mutating_request;
use rub_core::error::RubError;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    DaemonConnection, ExistingCloseOutcome, TransientSocketPolicy, detect_or_connect_hardened,
    send_existing_request_with_replay_recovery,
};

fn augment_close_existing_error(
    error: RubError,
    session_name: &str,
    daemon_session_id: Option<&str>,
    command_id: Option<&str>,
) -> RubError {
    let mut envelope = error.into_envelope();
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert("session".to_string(), serde_json::json!(session_name));
    context.insert(
        "daemon_session_id".to_string(),
        serde_json::json!(daemon_session_id),
    );
    context.insert("command_id".to_string(), serde_json::json!(command_id));
    envelope.message = format!(
        "Failed to close existing session '{session_name}': {}",
        envelope.message
    );
    envelope.context = Some(serde_json::Value::Object(context));
    RubError::Domain(envelope)
}

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
        augment_close_existing_error(
            error,
            session_name,
            daemon_session_id.as_deref(),
            request.command_id.as_deref(),
        )
    })?;
    Ok(ExistingCloseOutcome::Closed(Box::new(response)))
}

#[cfg(test)]
mod tests {
    use super::augment_close_existing_error;
    use rub_core::error::{ErrorCode, RubError};

    #[test]
    fn close_existing_error_preserves_original_error_code() {
        let error = RubError::domain_with_context(
            ErrorCode::IpcTimeout,
            "IPC timeout: replay send exhausted budget",
            serde_json::json!({
                "reason": "ipc_replay_budget_exhausted",
            }),
        );

        let augmented =
            augment_close_existing_error(error, "default", Some("sess-1"), Some("cmd-1"));
        let envelope = augmented.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason")),
            Some(&serde_json::json!("ipc_replay_budget_exhausted"))
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("daemon_session_id")),
            Some(&serde_json::json!("sess-1"))
        );
        assert!(
            envelope
                .message
                .contains("Failed to close existing session 'default'"),
            "{}",
            envelope.message
        );
    }
}
