use crate::commands::{EffectiveCli, RequestedLaunchPolicy};
use crate::daemon_ctl;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{ConnectionTarget, LaunchPolicyInfo};

use super::ConnectionRequest;
use super::identity::{
    request_needs_live_attachment_resolution, requested_attachment_identity,
    resolve_attachment_identity,
};
use super::projection::{requested_connection_projection, requested_session_policy_projection};

pub(crate) async fn validate_existing_session_connection_request(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> Result<(), rub_core::error::RubError> {
    if !requires_existing_session_validation(true, request, cli) {
        return Ok(());
    }

    let launch_policy =
        daemon_ctl::fetch_launch_policy_for_session(&cli.rub_home, &cli.session).await?;
    let current_attachment_identity =
        rub_daemon::session::authoritative_entry_by_session_name(&cli.rub_home, &cli.session)
            .map_err(|error| {
                RubError::domain(
                    ErrorCode::InternalError,
                    format!(
                        "Failed to read current session authority for '{}': {error}",
                        cli.session
                    ),
                )
            })?
            .and_then(|entry| entry.attachment_identity);
    let requested_attachment_identity = if request_needs_live_attachment_resolution(
        current_attachment_identity.as_deref(),
        request,
    ) {
        resolve_attachment_identity(cli, request, None).await?
    } else {
        requested_attachment_identity(cli, request)
    };
    if attachment_identity_matches_request(
        &current_attachment_identity,
        requested_attachment_identity.as_deref(),
        launch_policy.connection_target.as_ref(),
        request,
    ) && launch_policy_matches_session_policy(&launch_policy, request, cli)
    {
        return Ok(());
    }

    Err(rub_core::error::RubError::domain_with_context(
        ErrorCode::InvalidInput,
        format!(
            "Session '{}' is already running with a different browser attachment policy. Use a different --session or close the existing daemon first.",
            cli.session
        ),
        serde_json::json!({
            "requested_attachment_identity": requested_attachment_identity,
            "current_attachment_identity": current_attachment_identity,
            "requested_connection": requested_connection_projection(request),
            "requested_session_policy": requested_session_policy_projection(request, cli),
            "current_launch_policy": launch_policy,
        }),
    ))
}

pub(crate) fn requires_existing_session_validation(
    connected_to_existing_daemon: bool,
    request: &ConnectionRequest,
    cli: &EffectiveCli,
) -> bool {
    connected_to_existing_daemon
        && (!matches!(request, ConnectionRequest::None)
            || compatibility_launch_policy(cli, request).has_any())
}

pub(crate) fn attachment_identity_matches_request(
    current_attachment_identity: &Option<String>,
    requested_attachment_identity: Option<&str>,
    _current_target: Option<&ConnectionTarget>,
    request: &ConnectionRequest,
) -> bool {
    match request {
        ConnectionRequest::None => true,
        ConnectionRequest::CdpUrl { .. }
        | ConnectionRequest::Profile { .. }
        | ConnectionRequest::UserDataDir { .. } => {
            current_attachment_identity.as_deref() == requested_attachment_identity
        }
        ConnectionRequest::AutoDiscover => requested_attachment_identity
            .is_some_and(|identity| current_attachment_identity.as_deref() == Some(identity)),
    }
}

pub(crate) fn launch_policy_matches_session_policy(
    launch_policy: &LaunchPolicyInfo,
    request: &ConnectionRequest,
    cli: &EffectiveCli,
) -> bool {
    let requested = compatibility_launch_policy(cli, request);
    let requested_user_data_dir = requested.user_data_dir.clone();

    (!requested.headed || !launch_policy.headless)
        && (!requested.ignore_cert_errors || launch_policy.ignore_cert_errors)
        && (!requested.show_infobars || !launch_policy.hide_infobars)
        && requested_user_data_dir
            .as_deref()
            .is_none_or(|requested_dir: &str| {
                launch_policy.user_data_dir.as_deref() == Some(requested_dir)
            })
        && (!requested.no_stealth || !launch_policy.stealth_default_enabled.unwrap_or(true))
        && (!requested.humanize || launch_policy.humanize_enabled.unwrap_or(false))
        && requested
            .humanize_speed
            .as_deref()
            .is_none_or(|speed: &str| launch_policy.humanize_speed.as_deref() == Some(speed))
}

pub(crate) fn compatibility_launch_policy(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> RequestedLaunchPolicy {
    let mut requested = cli.effective_launch_policy.clone();
    requested.user_data_dir = match request {
        ConnectionRequest::Profile { user_data_root, .. } => Some(user_data_root.clone()),
        ConnectionRequest::UserDataDir { path } => Some(path.clone()),
        ConnectionRequest::None => requested.user_data_dir,
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::AutoDiscover => None,
    };
    if !requested.humanize {
        requested.humanize_speed = None;
    }
    requested
}
