use crate::commands::EffectiveCli;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::PathReferenceState;

pub(super) const SESSION_ID_ENV: &str = "RUB_SESSION_ID";

pub(super) fn internal_daemon_path_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "local_runtime_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(super) fn rub_home_startup_error(
    rub_home: &std::path::Path,
    error: &std::io::Error,
) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::DaemonStartFailed,
        format!("Cannot create RUB_HOME {}: {error}", rub_home.display()),
    )
    .with_context(serde_json::json!({
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": internal_daemon_path_state(
            "cli.internal_daemon.rub_home",
            "cli_rub_home",
            "rub_home_directory",
        ),
        "reason": "rub_home_create_failed",
    }))
}

pub(super) fn daemon_runtime_error(rub_home: &std::path::Path, message: String) -> ErrorEnvelope {
    ErrorEnvelope::new(ErrorCode::DaemonStartFailed, message).with_context(serde_json::json!({
        "rub_home": rub_home.display().to_string(),
        "rub_home_state": internal_daemon_path_state(
            "cli.internal_daemon.rub_home",
            "cli_rub_home",
            "rub_home_directory",
        ),
        "reason": "daemon_runtime_failed",
    }))
}

pub(super) fn resolve_startup_session_id() -> Result<String, ErrorEnvelope> {
    let Some(session_id) = std::env::var(SESSION_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(rub_daemon::session::new_session_id());
    };

    rub_daemon::rub_paths::validate_session_id_component(&session_id).map_err(|reason| {
        ErrorEnvelope::new(
            ErrorCode::DaemonStartFailed,
            format!("Invalid {SESSION_ID_ENV}: {reason}"),
        )
        .with_context(serde_json::json!({
            "env": SESSION_ID_ENV,
            "session_id": session_id,
            "reason": "invalid_session_id_component",
        }))
    })?;

    Ok(session_id)
}

pub(super) fn resolve_cli_or_env_session_id(cli: &EffectiveCli) -> Result<String, ErrorEnvelope> {
    if let Some(session_id) = cli.session_id.as_deref() {
        rub_daemon::rub_paths::validate_session_id_component(session_id).map_err(|reason| {
            ErrorEnvelope::new(
                ErrorCode::DaemonStartFailed,
                format!("Invalid --session-id: {reason}"),
            )
            .with_context(serde_json::json!({
                "flag": "--session-id",
                "session_id": session_id,
                "reason": "invalid_session_id_component",
            }))
        })?;
        return Ok(session_id.to_string());
    }
    resolve_startup_session_id()
}
