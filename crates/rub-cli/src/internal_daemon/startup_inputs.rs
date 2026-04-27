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
    let authoritative_startup_inputs = crate::daemon_ctl::read_authoritative_startup_inputs()
        .map_err(|error| error.into_envelope())?;
    let connection_request =
        if let Some(authoritative_startup_inputs) = authoritative_startup_inputs.as_ref() {
            authoritative_startup_inputs.connection_request.clone()
        } else {
            let connection_request =
                parse_connection_request(cli).map_err(|error| error.into_envelope())?;
            materialize_connection_request(&connection_request)
                .await
                .map_err(|error| error.into_envelope())?
        };
    let effective_user_data_dir = requested_user_data_dir(cli, &connection_request).or_else(|| {
        matches!(connection_request, ConnectionRequest::None).then(|| {
            rub_cdp::projected_managed_profile_path_for_session(session_id)
                .display()
                .to_string()
        })
    });
    let attachment_identity = if let Some(authoritative_startup_inputs) =
        authoritative_startup_inputs.as_ref()
    {
        authoritative_startup_inputs.attachment_identity.clone()
    } else {
        effective_attachment_identity(cli, &connection_request, effective_user_data_dir.as_deref())
            .await
            .map_err(|error| error.into_envelope())?
    };

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

#[cfg(test)]
mod tests {
    use super::resolve_startup_inputs;
    use crate::commands::{Commands, EffectiveCli, RequestedLaunchPolicy};
    use crate::daemon_ctl::AuthoritativeStartupInputs;
    use crate::session_policy::ConnectionRequest;
    use std::path::PathBuf;
    use tokio::sync::Mutex;

    static STARTUP_INPUTS_ENV_LOCK: Mutex<()> = Mutex::const_new(());

    fn cli_with(command: Commands) -> EffectiveCli {
        EffectiveCli {
            session: "default".to_string(),
            session_id: None,
            rub_home: PathBuf::from("/tmp/rub-test"),
            timeout: 30_000,
            headed: false,
            ignore_cert_errors: false,
            user_data_dir: None,
            hide_infobars: true,
            json_pretty: false,
            verbose: false,
            trace: false,
            command,
            cdp_url: None,
            connect: false,
            profile: None,
            profile_resolved_path: None,
            use_alias: None,
            no_stealth: false,
            humanize: false,
            humanize_speed: "normal".to_string(),
            requested_launch_policy: RequestedLaunchPolicy::default(),
            effective_launch_policy: RequestedLaunchPolicy::default(),
        }
    }

    struct StartupInputsEnvGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl StartupInputsEnvGuard {
        fn install(raw: String) -> Self {
            let previous = std::env::var_os("RUB_STARTUP_INPUTS");
            unsafe { std::env::set_var("RUB_STARTUP_INPUTS", raw) };
            Self { previous }
        }

        fn unset() -> Self {
            let previous = std::env::var_os("RUB_STARTUP_INPUTS");
            unsafe { std::env::remove_var("RUB_STARTUP_INPUTS") };
            Self { previous }
        }
    }

    impl Drop for StartupInputsEnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                unsafe { std::env::set_var("RUB_STARTUP_INPUTS", previous) };
            } else {
                unsafe { std::env::remove_var("RUB_STARTUP_INPUTS") };
            }
        }
    }

    #[tokio::test]
    async fn default_managed_startup_inputs_use_session_scoped_profile_authority() {
        let _env_lock = STARTUP_INPUTS_ENV_LOCK.lock().await;
        let _guard = StartupInputsEnvGuard::unset();
        let cli = cli_with(Commands::Doctor);
        let expected_path = rub_cdp::projected_managed_profile_path_for_session("sess-123")
            .to_string_lossy()
            .into_owned();
        let expected_identity = format!(
            "user_data_dir:{}",
            crate::session_policy::normalize_identity_path(
                rub_cdp::projected_managed_profile_path_for_session("sess-123")
            )
        );

        let inputs = resolve_startup_inputs(&cli, "sess-123")
            .await
            .expect("default managed startup inputs should resolve");

        assert_eq!(
            inputs.effective_user_data_dir.as_deref(),
            Some(expected_path.as_str())
        );
        assert_eq!(
            inputs.attachment_identity.as_deref(),
            Some(expected_identity.as_str())
        );
    }

    #[tokio::test]
    async fn explicit_user_data_dir_startup_inputs_preserve_non_ephemeral_authority() {
        let _env_lock = STARTUP_INPUTS_ENV_LOCK.lock().await;
        let _guard = StartupInputsEnvGuard::unset();
        let mut cli = cli_with(Commands::Doctor);
        cli.user_data_dir = Some("/tmp/explicit-profile-root".to_string());
        cli.requested_launch_policy.user_data_dir = Some("/tmp/explicit-profile-root".to_string());

        let inputs = resolve_startup_inputs(&cli, "sess-123")
            .await
            .expect("explicit user-data-dir startup inputs should resolve");

        assert!(
            inputs
                .effective_user_data_dir
                .as_deref()
                .is_some_and(|path| path == "/tmp/explicit-profile-root")
        );
        assert!(
            inputs
                .attachment_identity
                .as_deref()
                .is_some_and(|identity| identity == "user_data_dir:/tmp/explicit-profile-root")
        );
    }

    #[tokio::test]
    async fn internal_profile_resolved_path_preserves_exact_profile_authority() {
        let _env_lock = STARTUP_INPUTS_ENV_LOCK.lock().await;
        let _guard = StartupInputsEnvGuard::unset();
        let mut cli = cli_with(Commands::Doctor);
        cli.profile = Some("Work".to_string());
        cli.profile_resolved_path = Some("/tmp/bindings/Profile 3".to_string());

        let inputs = resolve_startup_inputs(&cli, "sess-123")
            .await
            .expect("internal resolved profile authority should survive startup input parsing");

        assert_eq!(
            inputs.connection_request,
            ConnectionRequest::Profile {
                name: "Work".to_string(),
                dir_name: "Profile 3".to_string(),
                resolved_path: "/tmp/bindings/Profile 3".to_string(),
                user_data_root: "/tmp/bindings".to_string(),
            }
        );
        assert_eq!(
            inputs.effective_user_data_dir.as_deref(),
            Some("/tmp/bindings")
        );
        assert_eq!(
            inputs.attachment_identity.as_deref(),
            Some("profile:/tmp/bindings/Profile 3")
        );
    }

    #[tokio::test]
    async fn authoritative_startup_inputs_override_child_reparse_and_resolution() {
        let _env_lock = STARTUP_INPUTS_ENV_LOCK.lock().await;
        let cli = cli_with(Commands::Doctor);
        let _guard = StartupInputsEnvGuard::install(
            serde_json::to_string(&AuthoritativeStartupInputs {
                connection_request: ConnectionRequest::Profile {
                    name: "Work".to_string(),
                    dir_name: "Profile 3".to_string(),
                    resolved_path: "/tmp/bindings/Profile 3".to_string(),
                    user_data_root: "/tmp/bindings".to_string(),
                },
                attachment_identity: Some("profile:/tmp/bindings/Profile 3".to_string()),
            })
            .expect("authoritative startup inputs should serialize"),
        );

        let inputs = resolve_startup_inputs(&cli, "sess-123")
            .await
            .expect("authoritative startup inputs should bypass child re-materialization");

        assert_eq!(
            inputs.connection_request,
            ConnectionRequest::Profile {
                name: "Work".to_string(),
                dir_name: "Profile 3".to_string(),
                resolved_path: "/tmp/bindings/Profile 3".to_string(),
                user_data_root: "/tmp/bindings".to_string(),
            }
        );
        assert_eq!(
            inputs.effective_user_data_dir.as_deref(),
            Some("/tmp/bindings")
        );
        assert_eq!(
            inputs.attachment_identity.as_deref(),
            Some("profile:/tmp/bindings/Profile 3")
        );
    }
}
