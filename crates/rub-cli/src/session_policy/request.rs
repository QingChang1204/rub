use crate::commands::EffectiveCli;
use crate::timeout_budget::{ensure_remaining_budget, run_with_remaining_budget};
use rub_core::error::ErrorCode;
use std::time::Instant;

use super::ConnectionRequest;
use super::identity::{normalize_cdp_identity, normalize_identity_path};

pub(crate) fn parse_connection_request(
    cli: &EffectiveCli,
) -> Result<ConnectionRequest, rub_core::error::RubError> {
    if cli.profile.is_some() && cli.requested_launch_policy.user_data_dir.is_some() {
        return Err(rub_core::error::RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Use either --profile or --user-data-dir, not both",
            serde_json::json!({
                "profile": cli.profile,
                "user_data_dir": cli.user_data_dir,
                "user_data_dir_state": cli.user_data_dir.as_ref().map(|_| {
                    super::projection::session_policy_path_state(
                        "cli.session_policy.requested.user_data_dir",
                        "cli_user_data_dir_option",
                        "managed_user_data_dir",
                    )
                }),
                "reason": "profile_user_data_dir_conflict",
            }),
        ));
    }

    let connect_flag_count = [
        cli.cdp_url.is_some(),
        cli.connect,
        cli.profile.is_some(),
        cli.requested_launch_policy.user_data_dir.is_some(),
        cli.use_alias.is_some(),
    ]
    .into_iter()
    .filter(|flag| *flag)
    .count();
    if connect_flag_count > 1 {
        return Err(rub_core::error::RubError::domain(
            ErrorCode::ConflictingConnectOptions,
            "Use only one browser attachment selector per command",
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
    if cli.requested_launch_policy.user_data_dir.is_some() {
        let path = cli.user_data_dir.clone().ok_or_else(|| {
            rub_core::error::RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "Explicit user-data-dir request is missing an effective path",
                serde_json::json!({
                    "reason": "explicit_user_data_dir_missing_effective_path",
                }),
            )
        })?;
        return Ok(ConnectionRequest::UserDataDir { path });
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

pub(crate) fn materialized_auto_discover_request(
    candidate: &rub_cdp::attachment::CdpCandidate,
) -> ConnectionRequest {
    ConnectionRequest::CdpUrl {
        url: rub_cdp::attachment::normalize_external_connect_url(&candidate.ws_url),
    }
}

pub(crate) async fn materialize_connection_request(
    request: &ConnectionRequest,
) -> Result<ConnectionRequest, rub_core::error::RubError> {
    materialize_connection_request_with_deadline(request, None, None).await
}

pub(crate) async fn materialize_connection_request_with_deadline(
    request: &ConnectionRequest,
    deadline: Option<Instant>,
    timeout_ms: Option<u64>,
) -> Result<ConnectionRequest, rub_core::error::RubError> {
    match request {
        ConnectionRequest::AutoDiscover => {
            let candidate = match (deadline, timeout_ms) {
                (Some(deadline), Some(timeout_ms)) => {
                    run_with_remaining_budget(
                        deadline,
                        timeout_ms,
                        "connection_request_materialization",
                        async { rub_cdp::attachment::resolve_unique_local_cdp_candidate().await },
                    )
                    .await?
                }
                _ => rub_cdp::attachment::resolve_unique_local_cdp_candidate().await?,
            };
            Ok(materialized_auto_discover_request(&candidate))
        }
        ConnectionRequest::UserDataDir { path } => {
            if let (Some(deadline), Some(timeout_ms)) = (deadline, timeout_ms) {
                ensure_remaining_budget(
                    deadline,
                    timeout_ms,
                    "connection_request_materialization",
                )?;
            }
            let normalized = normalize_identity_path(path);
            if let (Some(deadline), Some(timeout_ms)) = (deadline, timeout_ms) {
                ensure_remaining_budget(
                    deadline,
                    timeout_ms,
                    "connection_request_materialization",
                )?;
            }
            Ok(ConnectionRequest::UserDataDir { path: normalized })
        }
        ConnectionRequest::Profile {
            name,
            dir_name,
            resolved_path,
            user_data_root,
        } => {
            if let (Some(deadline), Some(timeout_ms)) = (deadline, timeout_ms) {
                ensure_remaining_budget(
                    deadline,
                    timeout_ms,
                    "connection_request_materialization",
                )?;
            }
            let normalized_user_data_root = normalize_identity_path(user_data_root);
            if let (Some(deadline), Some(timeout_ms)) = (deadline, timeout_ms) {
                ensure_remaining_budget(
                    deadline,
                    timeout_ms,
                    "connection_request_materialization",
                )?;
            }
            Ok(ConnectionRequest::Profile {
                name: name.clone(),
                dir_name: dir_name.clone(),
                resolved_path: resolved_path.clone(),
                user_data_root: normalized_user_data_root,
            })
        }
        _ => Ok(request.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rub_core::error::ErrorCode;
    use std::path::Path;
    use std::time::Duration;

    #[tokio::test]
    async fn materialize_connection_request_times_out_when_deadline_is_exhausted() {
        let request = ConnectionRequest::UserDataDir {
            path: "./profile".to_string(),
        };
        let error = materialize_connection_request_with_deadline(
            &request,
            Some(Instant::now() - Duration::from_millis(1)),
            Some(1_500),
        )
        .await
        .expect_err("expired deadline should fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(|value| value.as_str()),
            Some("connection_request_materialization")
        );
    }

    #[tokio::test]
    async fn materialize_connection_request_normalizes_path_within_deadline() {
        let request = ConnectionRequest::UserDataDir {
            path: "./profile".to_string(),
        };
        let materialized = materialize_connection_request_with_deadline(
            &request,
            Some(Instant::now() + Duration::from_secs(1)),
            Some(1_500),
        )
        .await
        .expect("live deadline should allow normalization");
        match materialized {
            ConnectionRequest::UserDataDir { path } => {
                assert!(path.ends_with("profile"));
                assert!(Path::new(&path).is_absolute());
            }
            other => panic!("unexpected materialized request: {other:?}"),
        }
    }
}
