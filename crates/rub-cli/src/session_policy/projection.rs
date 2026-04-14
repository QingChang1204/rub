use super::{
    ConnectionRequest, EffectiveCli, compatibility_launch_policy, requested_user_data_dir,
};
use rub_core::model::PathReferenceState;

pub(super) fn session_policy_path_state(
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

pub(super) fn requested_connection_projection(request: &ConnectionRequest) -> serde_json::Value {
    match request {
        ConnectionRequest::None => serde_json::Value::Null,
        ConnectionRequest::CdpUrl { url } => serde_json::json!({
            "source": "cdp_url",
            "url": url,
        }),
        ConnectionRequest::AutoDiscover => serde_json::json!({
            "source": "auto_discover",
        }),
        ConnectionRequest::UserDataDir { path } => serde_json::json!({
            "source": "user_data_dir",
            "path": path,
            "path_state": session_policy_path_state(
                "cli.session_policy.requested_connection.path",
                "requested_user_data_dir",
                "managed_user_data_dir",
            ),
        }),
        ConnectionRequest::Profile {
            name,
            resolved_path,
            ..
        } => serde_json::json!({
            "source": "profile",
            "name": name,
            "resolved_path": resolved_path,
            "resolved_path_state": session_policy_path_state(
                "cli.session_policy.requested_connection.resolved_path",
                "profile_resolution",
                "profile_directory",
            ),
        }),
    }
}

pub(super) fn requested_session_policy_projection(
    request: &ConnectionRequest,
    cli: &EffectiveCli,
) -> serde_json::Value {
    let compatibility = compatibility_launch_policy(cli, request);
    let effective_user_data_dir = requested_user_data_dir(cli, request);
    serde_json::json!({
        "headed": cli.effective_launch_policy.headed,
        "ignore_cert_errors": cli.effective_launch_policy.ignore_cert_errors,
        "show_infobars": cli.effective_launch_policy.show_infobars,
        "user_data_dir": cli.effective_launch_policy.user_data_dir,
        "user_data_dir_state": cli.effective_launch_policy.user_data_dir.as_ref().map(|_| {
            session_policy_path_state(
                "cli.session_policy.effective.user_data_dir",
                "cli_effective_launch_policy",
                "managed_user_data_dir",
            )
        }),
        "stealth_disabled": cli.effective_launch_policy.no_stealth,
        "humanize_enabled": cli.effective_launch_policy.humanize,
        "humanize_speed": cli.effective_launch_policy.humanize_speed,
        "effective_user_data_dir": effective_user_data_dir,
        "effective_user_data_dir_state": requested_user_data_dir(cli, request).as_ref().map(|_| {
            session_policy_path_state(
                "cli.session_policy.effective_user_data_dir",
                "requested_user_data_dir_projection",
                "managed_user_data_dir",
            )
        }),
        "compatibility_policy": {
            "headed": compatibility.headed,
            "ignore_cert_errors": compatibility.ignore_cert_errors,
            "show_infobars": compatibility.show_infobars,
            "user_data_dir": compatibility.user_data_dir,
            "user_data_dir_state": compatibility.user_data_dir.as_ref().map(|_| {
                session_policy_path_state(
                    "cli.session_policy.compatibility.user_data_dir",
                    "compatibility_launch_policy",
                    "managed_user_data_dir",
                )
            }),
            "stealth_disabled": compatibility.no_stealth,
            "humanize_enabled": compatibility.humanize,
            "humanize_speed": compatibility.humanize_speed,
        },
        "explicit_request": {
            "headed": cli.requested_launch_policy.headed,
            "ignore_cert_errors": cli.requested_launch_policy.ignore_cert_errors,
            "show_infobars": cli.requested_launch_policy.show_infobars,
            "user_data_dir": cli.requested_launch_policy.user_data_dir,
            "user_data_dir_state": cli.requested_launch_policy.user_data_dir.as_ref().map(|_| {
                session_policy_path_state(
                    "cli.session_policy.explicit_request.user_data_dir",
                    "cli_requested_launch_policy",
                    "managed_user_data_dir",
                )
            }),
            "stealth_disabled": cli.requested_launch_policy.no_stealth,
            "humanize_enabled": cli.requested_launch_policy.humanize,
            "humanize_speed": cli.requested_launch_policy.humanize_speed,
        },
    })
}
