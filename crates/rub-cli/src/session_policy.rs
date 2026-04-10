use crate::commands::EffectiveCli;

#[cfg(test)]
use std::path::PathBuf;

mod identity;
mod projection;
mod request;
mod validation;

pub(crate) use self::identity::{
    effective_attachment_identity, requested_attachment_identity, requested_user_data_dir,
};
pub(crate) use self::request::{materialize_connection_request, parse_connection_request};
pub(crate) use self::validation::{
    compatibility_launch_policy, requires_existing_session_validation,
    validate_existing_session_connection_request,
};

#[cfg(test)]
use self::identity::{normalize_identity_path, request_needs_live_attachment_resolution};
#[cfg(test)]
use self::projection::{requested_connection_projection, requested_session_policy_projection};
#[cfg(test)]
use self::request::materialized_auto_discover_request;
#[cfg(test)]
use self::validation::{attachment_identity_matches_request, launch_policy_matches_session_policy};

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

#[cfg(test)]
mod tests {
    use super::*;
    use rub_core::error::ErrorCode;
    use rub_core::model::{ConnectionTarget, LaunchPolicyInfo};

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
        let context = error.context.expect("conflict context");
        assert_eq!(context["reason"], "profile_user_data_dir_conflict");
        assert_eq!(
            context["user_data_dir_state"]["path_authority"],
            "cli.session_policy.requested.user_data_dir"
        );
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
    fn live_attachment_resolution_only_runs_for_existing_cdp_authority() {
        assert!(request_needs_live_attachment_resolution(
            Some("cdp:ws://127.0.0.1:9222/devtools/browser/current"),
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222".to_string(),
            },
        ));
        assert!(!request_needs_live_attachment_resolution(
            Some("user_data_dir:/tmp/rub-profile"),
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222".to_string(),
            },
        ));
        assert!(!request_needs_live_attachment_resolution(
            Some("profile:/tmp/profile"),
            &ConnectionRequest::Profile {
                name: "Default".to_string(),
                dir_name: "Default".to_string(),
                resolved_path: "/tmp/profile".to_string(),
                user_data_root: "/tmp".to_string(),
            },
        ));
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

    #[test]
    fn requested_connection_projection_marks_profile_path_state() {
        let projection = requested_connection_projection(&ConnectionRequest::Profile {
            name: "Default".to_string(),
            dir_name: "Default".to_string(),
            resolved_path: "/tmp/profile-root/Default".to_string(),
            user_data_root: "/tmp/profile-root".to_string(),
        });
        assert_eq!(projection["resolved_path"], "/tmp/profile-root/Default");
        assert_eq!(
            projection["resolved_path_state"]["path_authority"],
            "cli.session_policy.requested_connection.resolved_path"
        );
    }

    #[test]
    fn requested_session_policy_projection_marks_user_data_dir_states() {
        let mut cli = cli_with(Commands::Doctor);
        cli.user_data_dir = Some("/tmp/profile-root".to_string());
        cli.effective_launch_policy.user_data_dir = Some("/tmp/profile-root".to_string());
        cli.requested_launch_policy.user_data_dir = Some("/tmp/profile-root".to_string());

        let projection = requested_session_policy_projection(&ConnectionRequest::None, &cli);
        assert_eq!(
            projection["user_data_dir_state"]["path_authority"],
            "cli.session_policy.effective.user_data_dir"
        );
        assert_eq!(
            projection["effective_user_data_dir_state"]["path_authority"],
            "cli.session_policy.effective_user_data_dir"
        );
        assert_eq!(
            projection["compatibility_policy"]["user_data_dir_state"]["path_authority"],
            "cli.session_policy.compatibility.user_data_dir"
        );
        assert_eq!(
            projection["explicit_request"]["user_data_dir_state"]["path_authority"],
            "cli.session_policy.explicit_request.user_data_dir"
        );
    }
}
