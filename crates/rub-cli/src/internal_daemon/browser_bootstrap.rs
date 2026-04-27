use crate::commands::EffectiveCli;
use crate::session_policy::ConnectionRequest;
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::ConnectionTarget;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) fn build_browser_manager(
    cli: &EffectiveCli,
    connection_request: &ConnectionRequest,
    effective_user_data_dir: Option<String>,
    download_dir: PathBuf,
) -> Arc<rub_cdp::browser::BrowserManager> {
    Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: !cli.headed,
            ignore_cert_errors: cli.ignore_cert_errors,
            user_data_dir: effective_user_data_dir.map(PathBuf::from),
            managed_profile_ephemeral: matches!(connection_request, ConnectionRequest::None),
            download_dir: Some(download_dir),
            profile_directory: match connection_request {
                ConnectionRequest::Profile { dir_name, .. } => Some(dir_name.clone()),
                _ => None,
            },
            hide_infobars: cli.hide_infobars,
            stealth: !cli.no_stealth,
        },
    ))
}

pub(super) fn resolve_humanize_speed(
    cli: &EffectiveCli,
) -> Result<rub_cdp::humanize::HumanizeSpeed, ErrorEnvelope> {
    rub_cdp::humanize::HumanizeSpeed::from_str_opt(&cli.humanize_speed).ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            format!(
                "Unsupported humanize speed '{}'; use fast, normal, or slow",
                cli.humanize_speed
            ),
        )
    })
}

pub(super) async fn attach_browser(
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    state: &Arc<rub_daemon::session::SessionState>,
    connection_request: &ConnectionRequest,
) -> Result<(), ErrorEnvelope> {
    if let ConnectionRequest::Profile {
        name,
        resolved_path,
        ..
    } = connection_request
    {
        state.set_managed_profile_ephemeral(false).await;
        state
            .set_connection_target(Some(ConnectionTarget::Profile {
                name: name.clone(),
                resolved_path: resolved_path.clone(),
            }))
            .await;
        browser_manager
            .set_connection_target(ConnectionTarget::Profile {
                name: name.clone(),
                resolved_path: resolved_path.clone(),
            })
            .await;
    }

    let browser_result = match connection_request {
        ConnectionRequest::CdpUrl { url } => {
            let canonical_url = rub_cdp::attachment::canonical_external_browser_identity(url)
                .await
                .map_err(|error| error.into_envelope())?;
            state.set_managed_profile_ephemeral(false).await;
            state
                .set_connection_target(Some(ConnectionTarget::CdpUrl {
                    url: canonical_url.clone(),
                }))
                .await;
            browser_manager
                .connect_to_external(
                    &canonical_url,
                    ConnectionTarget::CdpUrl {
                        url: canonical_url.clone(),
                    },
                )
                .await
        }
        ConnectionRequest::Profile { .. }
        | ConnectionRequest::UserDataDir { .. }
        | ConnectionRequest::None => {
            if matches!(
                connection_request,
                ConnectionRequest::UserDataDir { .. } | ConnectionRequest::None
            ) {
                state
                    .set_managed_profile_ephemeral(matches!(
                        connection_request,
                        ConnectionRequest::None
                    ))
                    .await;
                state
                    .set_connection_target(Some(ConnectionTarget::Managed))
                    .await;
                browser_manager
                    .set_connection_target(ConnectionTarget::Managed)
                    .await;
            }
            browser_manager.ensure_browser().await
        }
        ConnectionRequest::AutoDiscover => {
            unreachable!("auto-discover requests are materialized before browser attach")
        }
    };

    browser_result.map_err(|error| error.into_envelope())
}

pub(super) async fn set_handoff_projection(
    state: &Arc<rub_daemon::session::SessionState>,
    browser_manager: &Arc<rub_cdp::browser::BrowserManager>,
    headed: bool,
) {
    if headed || browser_manager.is_external().await {
        state.set_handoff_available(true).await;
    } else {
        state
            .set_human_verification_handoff(rub_core::model::HumanVerificationHandoffInfo {
                unavailable_reason: Some("session_not_user_accessible".to_string()),
                ..rub_core::model::HumanVerificationHandoffInfo::default()
            })
            .await;
    }
}
