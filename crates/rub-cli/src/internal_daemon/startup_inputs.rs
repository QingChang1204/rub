use crate::commands::EffectiveCli;
use crate::session_policy::{
    ConnectionRequest, effective_attachment_identity, materialize_connection_request,
    parse_connection_request, requested_user_data_dir,
};
use rub_core::error::{ErrorCode, ErrorEnvelope};

use super::browser_bootstrap::resolve_humanize_speed;
use super::startup_bootstrap::internal_daemon_path_state;

pub(super) struct StartupInputs {
    pub(super) connection_request: ConnectionRequest,
    pub(super) effective_user_data_dir: Option<String>,
    pub(super) attachment_identity: Option<String>,
    pub(super) humanize_speed: rub_cdp::humanize::HumanizeSpeed,
}

pub(super) async fn resolve_startup_inputs(
    cli: &EffectiveCli,
    session_id: &str,
) -> Result<StartupInputs, ErrorEnvelope> {
    let connection_request =
        parse_connection_request(cli).map_err(|error| error.into_envelope())?;
    let connection_request = materialize_connection_request(&connection_request)
        .await
        .map_err(|error| error.into_envelope())?;
    let effective_user_data_dir = requested_user_data_dir(cli, &connection_request).or_else(|| {
        matches!(connection_request, ConnectionRequest::None).then(|| {
            rub_cdp::projected_managed_profile_path(None)
                .display()
                .to_string()
        })
    });
    let attachment_identity =
        effective_attachment_identity(cli, &connection_request, effective_user_data_dir.as_deref())
            .await
            .map_err(|error| error.into_envelope())?;

    if let Some(attachment_identity) = attachment_identity.as_deref() {
        match rub_daemon::session::check_profile_in_use(
            &cli.rub_home,
            attachment_identity,
            Some(session_id),
        ) {
            Ok(Some(conflicting_session)) => {
                return Err(ErrorEnvelope::new(
                    ErrorCode::ProfileInUse,
                    format!(
                        "Browser attachment {attachment_identity} is already used by session {conflicting_session}"
                    ),
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return Err(ErrorEnvelope::new(
                    ErrorCode::DaemonStartFailed,
                    format!(
                        "Failed to verify browser attachment ownership for {attachment_identity}: {error}"
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "attachment_ownership_check_failed",
                    "rub_home": cli.rub_home.display().to_string(),
                    "rub_home_state": internal_daemon_path_state(
                        "cli.internal_daemon.rub_home",
                        "cli_rub_home",
                        "rub_home_directory",
                    ),
                    "attachment_identity": attachment_identity,
                })));
            }
        }
    }

    let humanize_speed = resolve_humanize_speed(cli)?;

    Ok(StartupInputs {
        connection_request,
        effective_user_data_dir,
        attachment_identity,
        humanize_speed,
    })
}
