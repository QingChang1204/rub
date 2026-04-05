use crate::commands::EffectiveCli;
use crate::daemon_ctl;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{ConnectionTarget, LaunchPolicyInfo};
use std::path::{Component, Path, PathBuf};

#[cfg(test)]
use crate::commands::Commands;
#[cfg(test)]
use crate::commands::RequestedLaunchPolicy;

#[derive(Debug, Clone)]
pub(crate) enum ConnectionRequest {
    None,
    CdpUrl {
        url: String,
    },
    AutoDiscover,
    Profile {
        name: String,
        dir_name: String,
        resolved_path: String,
        user_data_root: String,
    },
}

pub(crate) fn parse_connection_request(
    cli: &EffectiveCli,
) -> Result<ConnectionRequest, rub_core::error::RubError> {
    let connect_flag_count = [cli.cdp_url.is_some(), cli.connect, cli.profile.is_some()]
        .into_iter()
        .filter(|flag| *flag)
        .count();
    if connect_flag_count > 1 {
        return Err(rub_core::error::RubError::domain(
            ErrorCode::ConflictingConnectOptions,
            "Use only one of --cdp-url, --connect, or --profile per command",
        ));
    }

    if cli.profile.is_some() && cli.requested_launch_policy.user_data_dir.is_some() {
        return Err(rub_core::error::RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Use either --profile or --user-data-dir, not both",
            serde_json::json!({
                "profile": cli.profile,
                "user_data_dir": cli.user_data_dir,
                "reason": "profile_user_data_dir_conflict",
            }),
        ));
    }

    if let Some(url) = &cli.cdp_url {
        return Ok(ConnectionRequest::CdpUrl {
            url: normalize_cdp_identity(url),
        });
    }
    if cli.connect {
        return Ok(ConnectionRequest::AutoDiscover);
    }
    if let Some(name) = &cli.profile {
        let profile = rub_cdp::profile::resolve_profile(name)?;
        let user_data_root = profile
            .path
            .parent()
            .ok_or_else(|| {
                rub_core::error::RubError::domain(
                    ErrorCode::ProfileNotFound,
                    format!(
                        "Resolved profile path {} has no parent user data directory",
                        profile.path.display()
                    ),
                )
            })?
            .display()
            .to_string();
        return Ok(ConnectionRequest::Profile {
            name: name.clone(),
            dir_name: profile.dir_name,
            resolved_path: profile.path.display().to_string(),
            user_data_root,
        });
    }

    Ok(ConnectionRequest::None)
}

fn materialized_auto_discover_request(
    candidate: &rub_cdp::attachment::CdpCandidate,
) -> ConnectionRequest {
    ConnectionRequest::CdpUrl {
        url: rub_cdp::attachment::normalize_external_connect_url(&candidate.ws_url),
    }
}

pub(crate) async fn materialize_connection_request(
    request: &ConnectionRequest,
) -> Result<ConnectionRequest, rub_core::error::RubError> {
    match request {
        // `--connect` must resolve to one concrete external browser authority
        // for locking, validation, bootstrap argv, and final attach. Re-running
        // local discovery later would let those layers drift onto different
        // browsers under concurrent startups or short-lived local Chrome churn.
        ConnectionRequest::AutoDiscover => {
            let candidate = rub_cdp::attachment::resolve_unique_local_cdp_candidate().await?;
            Ok(materialized_auto_discover_request(&candidate))
        }
        _ => Ok(request.clone()),
    }
}

pub(crate) fn requested_user_data_dir(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> Option<String> {
    match request {
        ConnectionRequest::Profile { user_data_root, .. } => Some(user_data_root.clone()),
        // Only managed sessions own a local user-data-dir authority. External
        // CDP attachment must not inherit local profile state from config
        // defaults because that would pollute shutdown/profile ownership.
        ConnectionRequest::None => cli.user_data_dir.clone(),
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::AutoDiscover => None,
    }
}

#[cfg(test)]
pub(crate) fn requested_attachment_identity(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> Option<String> {
    match request {
        ConnectionRequest::Profile { resolved_path, .. } => {
            Some(format!("profile:{resolved_path}"))
        }
        ConnectionRequest::CdpUrl { url } => Some(format!("cdp:{}", normalize_cdp_identity(url))),
        ConnectionRequest::AutoDiscover => Some("auto_discover:local_cdp".to_string()),
        ConnectionRequest::None => requested_user_data_dir(cli, request)
            .map(|path| format!("user_data_dir:{}", normalize_identity_path(&path))),
    }
}

pub(crate) async fn resolve_attachment_identity(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    effective_user_data_dir: Option<&str>,
) -> Result<Option<String>, RubError> {
    match request {
        ConnectionRequest::Profile { resolved_path, .. } => {
            Ok(Some(format!("profile:{resolved_path}")))
        }
        ConnectionRequest::CdpUrl { url } => Ok(Some(format!(
            "cdp:{}",
            rub_cdp::attachment::canonical_external_browser_identity(url).await?
        ))),
        ConnectionRequest::AutoDiscover => {
            let candidate = rub_cdp::attachment::resolve_unique_local_cdp_candidate().await?;
            Ok(Some(format!(
                "cdp:{}",
                rub_cdp::attachment::canonical_external_browser_identity(&candidate.ws_url).await?
            )))
        }
        ConnectionRequest::None => {
            let effective_path = effective_user_data_dir
                .map(str::to_string)
                .or_else(|| requested_user_data_dir(cli, request));
            Ok(effective_path
                .as_deref()
                .map(|path| format!("user_data_dir:{}", normalize_identity_path(path))))
        }
    }
}

pub(crate) async fn effective_attachment_identity(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    effective_user_data_dir: Option<&str>,
) -> Result<Option<String>, RubError> {
    match request {
        ConnectionRequest::None => Ok(effective_user_data_dir
            .map(|path| format!("user_data_dir:{}", normalize_identity_path(path)))),
        _ => resolve_attachment_identity(cli, request, effective_user_data_dir).await,
    }
}

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
    let requested_attachment_identity = resolve_attachment_identity(cli, request, None).await?;
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

fn attachment_identity_matches_request(
    current_attachment_identity: &Option<String>,
    requested_attachment_identity: Option<&str>,
    _current_target: Option<&ConnectionTarget>,
    request: &ConnectionRequest,
) -> bool {
    match request {
        ConnectionRequest::None => true,
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::Profile { .. } => {
            current_attachment_identity.as_deref() == requested_attachment_identity
        }
        ConnectionRequest::AutoDiscover => requested_attachment_identity
            .is_some_and(|identity| current_attachment_identity.as_deref() == Some(identity)),
    }
}

fn requested_connection_projection(request: &ConnectionRequest) -> serde_json::Value {
    match request {
        ConnectionRequest::None => serde_json::Value::Null,
        ConnectionRequest::CdpUrl { url } => serde_json::json!({
            "source": "cdp_url",
            "url": url,
        }),
        ConnectionRequest::AutoDiscover => serde_json::json!({
            "source": "auto_discover",
        }),
        ConnectionRequest::Profile {
            name,
            resolved_path,
            ..
        } => serde_json::json!({
            "source": "profile",
            "name": name,
            "resolved_path": resolved_path,
        }),
    }
}

fn launch_policy_matches_session_policy(
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
                launch_policy
                    .user_data_dir
                    .as_deref()
                    .map(normalize_identity_path)
                    .as_deref()
                    == Some(requested_dir)
            })
        && (!requested.no_stealth || !launch_policy.stealth_default_enabled.unwrap_or(true))
        && (!requested.humanize || launch_policy.humanize_enabled.unwrap_or(false))
        && requested
            .humanize_speed
            .as_deref()
            .is_none_or(|speed: &str| launch_policy.humanize_speed.as_deref() == Some(speed))
}

fn compatibility_launch_policy(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
) -> crate::commands::RequestedLaunchPolicy {
    let mut requested = cli.effective_launch_policy.clone();
    requested.user_data_dir = match request {
        ConnectionRequest::Profile { user_data_root, .. } => {
            Some(normalize_identity_path(user_data_root))
        }
        ConnectionRequest::None => requested.user_data_dir,
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::AutoDiscover => None,
    };
    if !requested.humanize {
        requested.humanize_speed = None;
    }
    requested
}

fn requested_session_policy_projection(
    request: &ConnectionRequest,
    cli: &EffectiveCli,
) -> serde_json::Value {
    let compatibility = compatibility_launch_policy(cli, request);
    serde_json::json!({
        "headed": cli.effective_launch_policy.headed,
        "ignore_cert_errors": cli.effective_launch_policy.ignore_cert_errors,
        "show_infobars": cli.effective_launch_policy.show_infobars,
        "user_data_dir": cli.effective_launch_policy.user_data_dir,
        "stealth_disabled": cli.effective_launch_policy.no_stealth,
        "humanize_enabled": cli.effective_launch_policy.humanize,
        "humanize_speed": cli.effective_launch_policy.humanize_speed,
        "effective_user_data_dir": requested_user_data_dir(cli, request),
        "compatibility_policy": {
            "headed": compatibility.headed,
            "ignore_cert_errors": compatibility.ignore_cert_errors,
            "show_infobars": compatibility.show_infobars,
            "user_data_dir": compatibility.user_data_dir,
            "stealth_disabled": compatibility.no_stealth,
            "humanize_enabled": compatibility.humanize,
            "humanize_speed": compatibility.humanize_speed,
        },
        "explicit_request": {
            "headed": cli.requested_launch_policy.headed,
            "ignore_cert_errors": cli.requested_launch_policy.ignore_cert_errors,
            "show_infobars": cli.requested_launch_policy.show_infobars,
            "user_data_dir": cli.requested_launch_policy.user_data_dir,
            "stealth_disabled": cli.requested_launch_policy.no_stealth,
            "humanize_enabled": cli.requested_launch_policy.humanize,
            "humanize_speed": cli.requested_launch_policy.humanize_speed,
        },
    })
}

pub(crate) fn normalize_identity_path(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    if let Ok(canonical) = absolute.canonicalize() {
        return canonical.to_string_lossy().into_owned();
    }

    let mut normalized = if absolute.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized.to_string_lossy().into_owned()
}

fn normalize_cdp_identity(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/').to_string();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if trimmed.ends_with("/json/version") {
            trimmed
        } else {
            format!("{trimmed}/json/version")
        }
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            no_stealth: false,
            humanize: false,
            humanize_speed: "normal".to_string(),
            requested_launch_policy: RequestedLaunchPolicy::default(),
            effective_launch_policy: RequestedLaunchPolicy::default(),
        }
    }

    #[test]
    fn parse_connection_request_rejects_conflicting_flags() {
        let mut cli = cli_with(Commands::Doctor);
        cli.cdp_url = Some("http://127.0.0.1:9222".to_string());
        cli.connect = true;

        let error = parse_connection_request(&cli).unwrap_err().into_envelope();
        assert_eq!(error.code, ErrorCode::ConflictingConnectOptions);
    }

    #[test]
    fn launch_policy_match_requires_same_cdp_url() {
        let current_target = ConnectionTarget::CdpUrl {
            url: "ws://127.0.0.1:9222/devtools/browser/test".to_string(),
        };

        assert!(attachment_identity_matches_request(
            &Some("cdp:ws://127.0.0.1:9222/devtools/browser/test".to_string()),
            Some("cdp:ws://127.0.0.1:9222/devtools/browser/test"),
            Some(&current_target),
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222".to_string(),
            },
        ));
        assert!(!attachment_identity_matches_request(
            &Some("cdp:ws://127.0.0.1:9222/devtools/browser/test".to_string()),
            Some("cdp:ws://127.0.0.1:9333/devtools/browser/test"),
            Some(&current_target),
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9333".to_string(),
            },
        ));
    }

    #[test]
    fn parse_connection_request_normalizes_equivalent_cdp_urls() {
        let mut cli = cli_with(Commands::Doctor);
        cli.cdp_url = Some("http://127.0.0.1:9222/".to_string());

        let request = parse_connection_request(&cli).expect("cdp request should parse");
        match request {
            ConnectionRequest::CdpUrl { url } => {
                assert_eq!(url, "http://127.0.0.1:9222/json/version");
            }
            other => panic!("expected cdp request, got {other:?}"),
        }
    }

    #[test]
    fn launch_policy_match_accepts_profile_via_resolved_path() {
        let current_target = ConnectionTarget::Profile {
            name: "Default".to_string(),
            resolved_path: "/profiles/default".to_string(),
        };

        assert!(attachment_identity_matches_request(
            &Some("profile:/profiles/default".to_string()),
            Some("profile:/profiles/default"),
            Some(&current_target),
            &ConnectionRequest::Profile {
                name: "Default".to_string(),
                dir_name: "Default".to_string(),
                resolved_path: "/profiles/default".to_string(),
                user_data_root: "/profiles".to_string(),
            },
        ));
    }

    #[test]
    fn auto_discover_requires_matching_canonical_attachment_identity() {
        let current_target = ConnectionTarget::AutoDiscovered {
            url: "ws://127.0.0.1:9222/devtools/browser/browser-a".to_string(),
            port: 9222,
        };

        assert!(attachment_identity_matches_request(
            &Some("cdp:ws://127.0.0.1:9222/devtools/browser/browser-a".to_string()),
            Some("cdp:ws://127.0.0.1:9222/devtools/browser/browser-a"),
            Some(&current_target),
            &ConnectionRequest::AutoDiscover,
        ));
        assert!(!attachment_identity_matches_request(
            &Some("cdp:ws://127.0.0.1:9222/devtools/browser/browser-a".to_string()),
            Some("cdp:ws://127.0.0.1:9333/devtools/browser/browser-b"),
            Some(&current_target),
            &ConnectionRequest::AutoDiscover,
        ));
    }

    #[test]
    fn existing_session_validation_runs_for_existing_daemon_when_connect_or_session_policy_differs()
    {
        let default_cli = cli_with(Commands::Doctor);
        let mut humanize_cli = default_cli.clone();
        humanize_cli.requested_launch_policy.humanize = true;
        humanize_cli.effective_launch_policy.humanize = true;

        assert!(!requires_existing_session_validation(
            false,
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222".to_string(),
            },
            &default_cli,
        ));
        assert!(!requires_existing_session_validation(
            true,
            &ConnectionRequest::None,
            &default_cli,
        ));
        assert!(requires_existing_session_validation(
            true,
            &ConnectionRequest::AutoDiscover,
            &default_cli,
        ));
        assert!(requires_existing_session_validation(
            true,
            &ConnectionRequest::None,
            &humanize_cli,
        ));
    }

    #[test]
    fn parse_connection_request_rejects_profile_plus_user_data_dir() {
        let mut cli = cli_with(Commands::Doctor);
        cli.profile = Some("Default".to_string());
        cli.user_data_dir = Some("/tmp/profile-root".to_string());
        cli.requested_launch_policy.user_data_dir = Some("/tmp/profile-root".to_string());

        let error = parse_connection_request(&cli).unwrap_err().into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    #[serial_test::serial]
    fn parse_connection_request_allows_profile_when_user_data_dir_only_comes_from_config_default() {
        let mock_home =
            std::env::temp_dir().join(format!("rub-mock-profile-{}", std::process::id()));
        std::fs::create_dir_all(mock_home.join("google-chrome/Default")).unwrap();
        std::fs::create_dir_all(
            mock_home.join("Library/Application Support/Google/Chrome/Default"),
        )
        .unwrap();
        std::fs::create_dir_all(mock_home.join("Google/Chrome/User Data/Default")).unwrap();
        let local_state = r#"{"profile":{"info_cache":{"Default":{"name":"Default"}}}}"#;
        std::fs::write(mock_home.join("google-chrome/Local State"), local_state).unwrap();
        std::fs::write(
            mock_home.join("Library/Application Support/Google/Chrome/Local State"),
            local_state,
        )
        .unwrap();
        std::fs::write(
            mock_home.join("Google/Chrome/User Data/Local State"),
            local_state,
        )
        .unwrap();

        let old_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        let old_home = std::env::var("HOME").ok();
        let old_localappdata = std::env::var("LOCALAPPDATA").ok();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &mock_home);
            std::env::set_var("HOME", &mock_home);
            std::env::set_var("LOCALAPPDATA", &mock_home);
        }

        struct EnvGuard {
            old_xdg: Option<String>,
            old_home: Option<String>,
            old_localappdata: Option<String>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    if let Some(v) = &self.old_xdg {
                        std::env::set_var("XDG_CONFIG_HOME", v);
                    } else {
                        std::env::remove_var("XDG_CONFIG_HOME");
                    }
                    if let Some(v) = &self.old_home {
                        std::env::set_var("HOME", v);
                    } else {
                        std::env::remove_var("HOME");
                    }
                    if let Some(v) = &self.old_localappdata {
                        std::env::set_var("LOCALAPPDATA", v);
                    } else {
                        std::env::remove_var("LOCALAPPDATA");
                    }
                }
            }
        }
        let _guard = EnvGuard {
            old_xdg,
            old_home,
            old_localappdata,
        };

        let mut cli = cli_with(Commands::Doctor);
        cli.profile = Some("Default".to_string());
        cli.user_data_dir = Some("/tmp/config-default-root".to_string());
        cli.effective_launch_policy.user_data_dir = Some("/tmp/config-default-root".to_string());
        cli.requested_launch_policy.user_data_dir = None;

        let request = parse_connection_request(&cli).expect("config default should not conflict");
        assert!(matches!(request, ConnectionRequest::Profile { .. }));

        let _ = std::fs::remove_dir_all(mock_home);
    }

    #[test]
    fn launch_policy_match_rejects_session_policy_drift() {
        let mut cli = cli_with(Commands::Doctor);
        cli.requested_launch_policy.headed = true;
        cli.requested_launch_policy.ignore_cert_errors = true;
        cli.requested_launch_policy.show_infobars = true;
        cli.requested_launch_policy.user_data_dir = Some("/tmp/profile-root".to_string());
        cli.requested_launch_policy.humanize = true;
        cli.requested_launch_policy.humanize_speed = Some("slow".to_string());
        cli.effective_launch_policy = cli.requested_launch_policy.clone();

        let launch_policy = LaunchPolicyInfo {
            headless: false,
            ignore_cert_errors: true,
            hide_infobars: false,
            user_data_dir: Some("/tmp/other-profile".to_string()),
            connection_target: Some(ConnectionTarget::Managed),
            stealth_level: Some("L1".to_string()),
            stealth_patches: Some(vec![]),
            stealth_default_enabled: Some(true),
            humanize_enabled: Some(false),
            humanize_speed: Some("normal".to_string()),
            stealth_coverage: None,
        };

        assert!(!launch_policy_matches_session_policy(
            &launch_policy,
            &ConnectionRequest::None,
            &cli
        ));
    }

    #[test]
    fn existing_session_validation_ignores_omitted_launch_policy_flags() {
        let cli = cli_with(Commands::Doctor);
        assert!(!requires_existing_session_validation(
            true,
            &ConnectionRequest::None,
            &cli,
        ));
    }

    #[test]
    fn existing_session_validation_honors_file_config_effective_launch_policy() {
        let mut cli = cli_with(Commands::Doctor);
        cli.effective_launch_policy.headed = true;

        assert!(requires_existing_session_validation(
            true,
            &ConnectionRequest::None,
            &cli,
        ));
    }

    #[test]
    fn requested_user_data_dir_only_validates_when_explicitly_requested() {
        let cli = cli_with(Commands::Doctor);
        let launch_policy = LaunchPolicyInfo {
            headless: true,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: Some("/tmp/profile-root".to_string()),
            connection_target: Some(ConnectionTarget::Managed),
            stealth_level: Some("L1".to_string()),
            stealth_patches: Some(vec![]),
            stealth_default_enabled: Some(true),
            humanize_enabled: Some(false),
            humanize_speed: Some("normal".to_string()),
            stealth_coverage: None,
        };

        assert!(launch_policy_matches_session_policy(
            &launch_policy,
            &ConnectionRequest::None,
            &cli,
        ));
    }

    #[test]
    fn normalized_attachment_identity_covers_cdp_auto_discover_and_user_data_dir() {
        let mut cli = cli_with(Commands::Doctor);
        cli.user_data_dir = Some("./profiles/../profiles/default".to_string());
        assert_eq!(
            requested_attachment_identity(&cli, &ConnectionRequest::None)
                .expect("identity should exist"),
            format!(
                "user_data_dir:{}",
                normalize_identity_path("./profiles/../profiles/default")
            )
        );

        assert_eq!(
            requested_attachment_identity(
                &cli,
                &ConnectionRequest::CdpUrl {
                    url: "http://127.0.0.1:9222".to_string()
                }
            )
            .expect("identity should exist"),
            "cdp:http://127.0.0.1:9222/json/version"
        );
        assert_eq!(
            requested_attachment_identity(&cli, &ConnectionRequest::AutoDiscover)
                .expect("identity should exist"),
            "auto_discover:local_cdp"
        );
    }

    #[test]
    fn materialized_auto_discover_uses_concrete_cdp_url_authority() {
        let candidate = rub_cdp::attachment::CdpCandidate {
            port: 9222,
            url: "http://127.0.0.1:9222".to_string(),
            ws_url: "ws://127.0.0.1:9222/devtools/browser/browser-a/".to_string(),
            browser_version: "Chrome/999".to_string(),
        };

        match materialized_auto_discover_request(&candidate) {
            ConnectionRequest::CdpUrl { url } => {
                assert_eq!(url, "ws://127.0.0.1:9222/devtools/browser/browser-a");
            }
            other => panic!("unexpected materialized request: {other:?}"),
        }
    }

    #[test]
    fn external_requests_do_not_inherit_local_user_data_dir_authority() {
        let mut cli = cli_with(Commands::Doctor);
        cli.user_data_dir = Some("/tmp/local-managed-profile".to_string());
        cli.effective_launch_policy.user_data_dir = Some("/tmp/local-managed-profile".to_string());

        assert_eq!(
            requested_user_data_dir(
                &cli,
                &ConnectionRequest::CdpUrl {
                    url: "http://127.0.0.1:9222".to_string()
                }
            ),
            None
        );
        assert_eq!(
            requested_user_data_dir(&cli, &ConnectionRequest::AutoDiscover),
            None
        );
    }

    #[test]
    fn external_attach_compatibility_ignores_local_user_data_dir_defaults() {
        let mut cli = cli_with(Commands::Doctor);
        cli.effective_launch_policy.user_data_dir = Some("/tmp/config-default-root".to_string());
        let launch_policy = LaunchPolicyInfo {
            headless: true,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: None,
            connection_target: Some(ConnectionTarget::CdpUrl {
                url: "ws://127.0.0.1:9222/devtools/browser/abc".to_string(),
            }),
            stealth_level: Some("L1".to_string()),
            stealth_patches: Some(vec![]),
            stealth_default_enabled: Some(true),
            humanize_enabled: Some(false),
            humanize_speed: None,
            stealth_coverage: None,
        };

        assert!(launch_policy_matches_session_policy(
            &launch_policy,
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222/json/version".to_string()
            },
            &cli,
        ));
    }

    #[test]
    fn dormant_humanize_speed_does_not_trigger_policy_mismatch() {
        let mut cli = cli_with(Commands::Doctor);
        cli.effective_launch_policy.humanize = false;
        cli.effective_launch_policy.humanize_speed = Some("slow".to_string());
        let launch_policy = LaunchPolicyInfo {
            headless: true,
            ignore_cert_errors: false,
            hide_infobars: true,
            user_data_dir: None,
            connection_target: Some(ConnectionTarget::Managed),
            stealth_level: Some("L1".to_string()),
            stealth_patches: Some(vec![]),
            stealth_default_enabled: Some(true),
            humanize_enabled: Some(false),
            humanize_speed: Some("normal".to_string()),
            stealth_coverage: None,
        };

        assert!(launch_policy_matches_session_policy(
            &launch_policy,
            &ConnectionRequest::None,
            &cli,
        ));
    }

    #[tokio::test]
    async fn effective_attachment_identity_uses_projected_managed_profile_for_default_sessions() {
        let cli = cli_with(Commands::Doctor);
        assert_eq!(
            effective_attachment_identity(
                &cli,
                &ConnectionRequest::None,
                Some("/tmp/rub-managed-profile")
            )
            .await
            .expect("identity should exist"),
            Some(format!(
                "user_data_dir:{}",
                normalize_identity_path("/tmp/rub-managed-profile")
            ))
        );
    }
}
