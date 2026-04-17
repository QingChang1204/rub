use crate::commands::EffectiveCli;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::PathReferenceState;
use serde_json::{Map, Value};
use std::path::Path;

pub(super) const SESSION_ID_ENV: &str = "RUB_SESSION_ID";

pub(super) struct InternalDaemonPathContext<'a> {
    pub path_key: &'a str,
    pub path: &'a Path,
    pub path_authority: &'a str,
    pub upstream_truth: &'a str,
    pub path_kind: &'a str,
    pub reason: &'a str,
}

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

pub(super) fn internal_daemon_local_io_error(
    rub_home: &Path,
    message: impl Into<String>,
    path_context: InternalDaemonPathContext<'_>,
) -> ErrorEnvelope {
    let mut context = Map::new();
    context.insert(
        "rub_home".to_string(),
        Value::String(rub_home.display().to_string()),
    );
    context.insert(
        "rub_home_state".to_string(),
        serde_json::to_value(internal_daemon_path_state(
            "cli.internal_daemon.rub_home",
            "cli_rub_home",
            "rub_home_directory",
        ))
        .expect("rub_home_state serializes"),
    );
    context.insert(
        path_context.path_key.to_string(),
        Value::String(path_context.path.display().to_string()),
    );
    context.insert(
        format!("{}_state", path_context.path_key),
        serde_json::to_value(internal_daemon_path_state(
            path_context.path_authority,
            path_context.upstream_truth,
            path_context.path_kind,
        ))
        .expect("internal_daemon_path_state serializes"),
    );
    context.insert(
        "reason".to_string(),
        Value::String(path_context.reason.to_string()),
    );
    ErrorEnvelope::new(ErrorCode::DaemonStartFailed, message.into())
        .with_context(Value::Object(context))
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
