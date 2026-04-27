use crate::commands::{EffectiveCli, RequestedLaunchPolicy};
use crate::daemon_ctl;
use crate::main_support::command_timeout_error;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{ConnectionTarget, LaunchPolicyInfo};
use rub_ipc::client::IpcClient;
use std::path::Path;
use std::time::Instant;

use super::ConnectionRequest;
#[cfg(test)]
use super::identity::resolve_attachment_identity;
use super::identity::{
    request_needs_live_attachment_resolution, requested_attachment_identity,
    resolve_attachment_identity_with_deadline,
};
use super::projection::{requested_connection_projection, requested_session_policy_projection};

#[derive(Debug, Clone)]
struct ExistingSessionAuthority {
    daemon_session_id: String,
    launch_policy: LaunchPolicyInfo,
    attachment_identity: Option<String>,
}

#[cfg(test)]
pub(crate) async fn validate_existing_session_connection_request(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    client: &mut IpcClient,
    expected_daemon_session_id: Option<&str>,
) -> Result<(), rub_core::error::RubError> {
    validate_existing_session_connection_request_with_deadline(
        cli,
        request,
        client,
        expected_daemon_session_id,
        None,
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn validate_existing_session_connection_request_with_deadline(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    client: &mut IpcClient,
    expected_daemon_session_id: Option<&str>,
    deadline: Option<Instant>,
    timeout_ms: Option<u64>,
) -> Result<(), rub_core::error::RubError> {
    if !requires_existing_session_validation(true, request, cli) {
        return Ok(());
    }

    let authority = fetch_existing_session_authority(
        client,
        expected_daemon_session_id,
        &cli.session,
        deadline,
        timeout_ms,
    )
    .await?;
    let requested_attachment_identity = if request_needs_live_attachment_resolution(
        authority.attachment_identity.as_deref(),
        request,
    ) {
        match (deadline, timeout_ms) {
            (Some(deadline), Some(timeout_ms)) => {
                resolve_attachment_identity_with_deadline(
                    cli,
                    request,
                    None,
                    deadline,
                    timeout_ms,
                    "existing_session_validation_attachment_identity_resolution",
                )
                .await?
            }
            _ => resolve_attachment_identity(cli, request, None).await?,
        }
    } else {
        requested_attachment_identity(cli, request)
    };
    if attachment_identity_matches_request(
        &authority.attachment_identity,
        requested_attachment_identity.as_deref(),
        authority.launch_policy.connection_target.as_ref(),
        request,
    ) && launch_policy_matches_session_policy(&authority.launch_policy, request, cli)
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
            "current_attachment_identity": authority.attachment_identity,
            "requested_connection": requested_connection_projection(request),
            "requested_session_policy": requested_session_policy_projection(request, cli),
            "current_launch_policy": authority.launch_policy,
            "daemon_session_id": authority.daemon_session_id,
        }),
    ))
}

pub(crate) async fn validate_existing_session_connection_request_via_authority_probe_with_deadline(
    cli: &EffectiveCli,
    request: &ConnectionRequest,
    authority_socket_path: &Path,
    expected_daemon_session_id: Option<&str>,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<(), rub_core::error::RubError> {
    if !requires_existing_session_validation(true, request, cli) {
        return Ok(());
    }

    let authority = fetch_existing_session_authority_via_probe(
        authority_socket_path,
        expected_daemon_session_id,
        &cli.session,
        deadline,
        timeout_ms,
    )
    .await?;
    let requested_attachment_identity = if request_needs_live_attachment_resolution(
        authority.attachment_identity.as_deref(),
        request,
    ) {
        resolve_attachment_identity_with_deadline(
            cli,
            request,
            None,
            deadline,
            timeout_ms,
            "existing_session_validation_attachment_identity_resolution",
        )
        .await?
    } else {
        requested_attachment_identity(cli, request)
    };
    if attachment_identity_matches_request(
        &authority.attachment_identity,
        requested_attachment_identity.as_deref(),
        authority.launch_policy.connection_target.as_ref(),
        request,
    ) && launch_policy_matches_session_policy(&authority.launch_policy, request, cli)
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
            "current_attachment_identity": authority.attachment_identity,
            "requested_connection": requested_connection_projection(request),
            "requested_session_policy": requested_session_policy_projection(request, cli),
            "current_launch_policy": authority.launch_policy,
            "daemon_session_id": authority.daemon_session_id,
        }),
    ))
}

async fn fetch_existing_session_authority_via_probe(
    authority_socket_path: &Path,
    expected_daemon_session_id: Option<&str>,
    session_name: &str,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<ExistingSessionAuthority, RubError> {
    if daemon_ctl::remaining_budget_ms(deadline) == 0 {
        return Err(command_timeout_error(
            timeout_ms,
            "existing_session_validation",
        ));
    }
    let socket_identity = daemon_ctl::current_socket_path_identity(
        authority_socket_path,
        "daemon_ctl.validation.socket_path",
        "existing_session_authority_socket",
        ErrorCode::IpcProtocolError,
        "existing_session_validation_socket_identity_read_failed",
    )?;
    let mut client = if let Some(daemon_session_id) = expected_daemon_session_id {
        daemon_ctl::authority_bound_connected_client(
            authority_socket_path,
            daemon_session_id,
            socket_identity,
            Some(daemon_ctl::AttachBudget {
                deadline,
                timeout_ms,
            }),
            daemon_ctl::AuthorityBoundConnectSpec {
                phase: "existing_session_validation_connect",
                error_code: ErrorCode::IpcProtocolError,
                message_prefix: "Failed to connect to the existing-session authority socket",
                path_authority: "daemon_ctl.validation.socket_path",
                upstream_truth: "existing_session_authority_socket",
            },
        )
        .await?
    } else {
        daemon_ctl::connect_ipc_with_retry_until(
            authority_socket_path,
            daemon_ctl::AttachBudget {
                deadline,
                timeout_ms,
            },
            "existing_session_validation_connect",
            ErrorCode::IpcProtocolError,
            "Failed to connect to the existing-session authority socket",
            "daemon_ctl.validation.socket_path",
            "existing_session_authority_socket",
        )
        .await
        .map_err(|failure| failure.into_error())?
        .0
    };
    fetch_existing_session_authority(
        &mut client,
        expected_daemon_session_id,
        session_name,
        Some(deadline),
        Some(timeout_ms),
    )
    .await
}

async fn fetch_existing_session_authority(
    client: &mut IpcClient,
    expected_daemon_session_id: Option<&str>,
    session_name: &str,
    deadline: Option<Instant>,
    timeout_ms: Option<u64>,
) -> Result<ExistingSessionAuthority, RubError> {
    let handshake_timeout_ms = match (deadline, timeout_ms) {
        (Some(deadline), Some(timeout_ms)) => {
            let remaining = daemon_ctl::remaining_budget_ms(deadline);
            if remaining == 0 {
                return Err(command_timeout_error(
                    timeout_ms,
                    "existing_session_validation",
                ));
            }
            remaining.max(1)
        }
        _ => 3_000,
    };
    let handshake =
        daemon_ctl::fetch_handshake_info_with_timeout(client, handshake_timeout_ms).await?;
    if let Some(expected) = expected_daemon_session_id
        && handshake.daemon_session_id != expected
    {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "Existing-session validation for '{}' bound to daemon '{}' but handshake returned '{}'",
                session_name, expected, handshake.daemon_session_id
            ),
            serde_json::json!({
                "reason": "existing_session_validation_authority_mismatch",
                "expected_daemon_session_id": expected,
                "handshake_daemon_session_id": handshake.daemon_session_id,
            }),
        ));
    }

    Ok(ExistingSessionAuthority {
        daemon_session_id: handshake.daemon_session_id,
        launch_policy: handshake.launch_policy,
        attachment_identity: handshake.attachment_identity,
    })
}

pub(crate) fn requires_existing_session_validation(
    connected_to_existing_daemon: bool,
    request: &ConnectionRequest,
    cli: &EffectiveCli,
) -> bool {
    connected_to_existing_daemon
        && (cli.session_id.is_some()
            || !matches!(request, ConnectionRequest::None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Commands, RequestedLaunchPolicy};
    use rub_ipc::client::IpcClient;
    use rub_ipc::handshake::HANDSHAKE_PROBE_COMMAND_ID;
    use rub_ipc::protocol::{IpcRequest, IpcResponse};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::net::UnixListener;
    use uuid::Uuid;

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("rsv-{}", Uuid::now_v7()))
    }

    fn cli_with(command: Commands, home: &std::path::Path) -> EffectiveCli {
        EffectiveCli {
            session: "default".to_string(),
            session_id: None,
            rub_home: home.to_path_buf(),
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

    fn spawn_handshake_server(
        socket_path: &std::path::Path,
        daemon_session_id: &str,
        attachment_identity: Option<&str>,
    ) -> tokio::task::JoinHandle<()> {
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path).expect("bind handshake socket");
        let daemon_session_id = daemon_session_id.to_string();
        let attachment_identity = attachment_identity.map(str::to_string);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept handshake client");
            let std_stream: StdUnixStream =
                stream.into_std().expect("convert tokio unix stream to std");
            let mut reader =
                BufReader::new(std_stream.try_clone().expect("clone handshake stream"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read handshake request");
            let request: IpcRequest =
                serde_json::from_str(line.trim_end()).expect("decode handshake request");
            assert_eq!(request.command, "_handshake");
            assert_eq!(
                request.command_id.as_deref(),
                Some(HANDSHAKE_PROBE_COMMAND_ID)
            );

            let mut writer = std_stream;
            let response = IpcResponse::success(
                "req-1",
                serde_json::json!({
                    "daemon_session_id": daemon_session_id.clone(),
                    "launch_policy": {
                        "headless": true,
                        "ignore_cert_errors": false,
                        "hide_infobars": true,
                        "user_data_dir": "/tmp/live",
                        "connection_target": null,
                        "stealth_level": "L1",
                        "stealth_patches": [],
                        "stealth_default_enabled": true,
                        "humanize_enabled": false,
                        "humanize_speed": "normal",
                        "stealth_coverage": null,
                    },
                    "attachment_identity": attachment_identity,
                }),
            )
            .with_command_id(HANDSHAKE_PROBE_COMMAND_ID)
            .expect("probe command_id must be valid")
            .with_daemon_session_id(daemon_session_id)
            .expect("daemon_session_id must be valid");
            serde_json::to_writer(&mut writer, &response).expect("encode handshake response");
            writer.write_all(b"\n").expect("newline handshake response");
        })
    }

    fn spawn_delayed_cdp_identity_server(delay: Duration) -> (String, tokio::task::JoinHandle<()>) {
        let runtime = tokio::runtime::Handle::current();
        let std_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind delayed cdp server");
        std_listener
            .set_nonblocking(true)
            .expect("set delayed cdp server nonblocking");
        let address = std_listener.local_addr().expect("local addr");
        let endpoint = format!("http://{address}/json/version");
        let handle = tokio::task::spawn_blocking(move || {
            let listener =
                TcpListener::from_std(std_listener).expect("convert tcp listener to tokio");
            runtime.block_on(async move {
                let (mut stream, _) = listener.accept().await.expect("accept delayed cdp client");
                let body =
                    format!(r#"{{"webSocketDebuggerUrl":"ws://{address}/devtools/browser/test"}}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                tokio::time::sleep(delay).await;
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write delayed cdp response");
            });
        });
        (endpoint, handle)
    }

    #[tokio::test]
    async fn existing_session_validation_prefers_bound_daemon_authority_over_local_snapshot() {
        let home = temp_home();
        std::fs::create_dir_all(&home).expect("create temp home");
        let socket_path = home.join("v.sock");
        let server = spawn_handshake_server(
            &socket_path,
            "sess-live",
            Some("profile:/tmp/live/Profile 1"),
        );

        let mut cli = cli_with(Commands::Doctor, &home);
        cli.profile = Some("Profile 1".to_string());
        let request = ConnectionRequest::Profile {
            name: "Profile 1".to_string(),
            dir_name: "Profile 1".to_string(),
            resolved_path: "/tmp/live/Profile 1".to_string(),
            user_data_root: "/tmp/live".to_string(),
        };

        let client = IpcClient::connect(&socket_path)
            .await
            .expect("connect validation client");
        let mut client = client
            .bind_daemon_session_id("sess-live")
            .expect("bind daemon session authority");

        validate_existing_session_connection_request(
            &cli,
            &request,
            &mut client,
            Some("sess-live"),
        )
        .await
        .expect("live daemon authority should validate request");

        server.await.expect("join handshake server");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn existing_session_validation_uses_remaining_command_budget_for_handshake() {
        let home = temp_home();
        std::fs::create_dir_all(&home).expect("create temp home");
        let socket_path = home.join("v.sock");
        let server = spawn_handshake_server(
            &socket_path,
            "sess-live",
            Some("profile:/tmp/live/Profile 1"),
        );

        let mut cli = cli_with(Commands::Doctor, &home);
        cli.profile = Some("Profile 1".to_string());
        let request = ConnectionRequest::Profile {
            name: "Profile 1".to_string(),
            dir_name: "Profile 1".to_string(),
            resolved_path: "/tmp/live/Profile 1".to_string(),
            user_data_root: "/tmp/live".to_string(),
        };

        let mut client = IpcClient::connect(&socket_path)
            .await
            .expect("connect validation client");

        let error = validate_existing_session_connection_request_with_deadline(
            &cli,
            &request,
            &mut client,
            Some("sess-live"),
            Some(Instant::now() - Duration::from_millis(1)),
            Some(1_500),
        )
        .await
        .expect_err("expired command budget should fail before handshake send");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(|value| value.as_str()),
            Some("existing_session_validation")
        );

        server.abort();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn existing_session_validation_fails_closed_for_connection_none_when_cli_session_id_is_authoritative()
     {
        let home = temp_home();
        std::fs::create_dir_all(&home).expect("create temp home");
        let socket_path = home.join("v.sock");
        let server = spawn_handshake_server(&socket_path, "sess-replacement", None);

        let mut cli = cli_with(Commands::Doctor, &home);
        cli.session_id = Some("sess-live".to_string());

        let mut client = IpcClient::connect(&socket_path)
            .await
            .expect("connect validation client");

        let error = validate_existing_session_connection_request_with_deadline(
            &cli,
            &ConnectionRequest::None,
            &mut client,
            cli.session_id.as_deref(),
            None,
            None,
        )
        .await
        .expect_err("remembered daemon authority mismatch must fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(|value| value.as_str()),
            Some("existing_session_validation_authority_mismatch")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("expected_daemon_session_id"))
                .and_then(|value| value.as_str()),
            Some("sess-live")
        );

        server.await.expect("join handshake server");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn existing_session_validation_uses_remaining_budget_for_live_attachment_identity_resolution()
     {
        let home = temp_home();
        std::fs::create_dir_all(&home).expect("create temp home");
        let socket_path = home.join("v.sock");
        let handshake_server = spawn_handshake_server(
            &socket_path,
            "sess-live",
            Some("cdp:ws://127.0.0.1:9222/devtools/browser/live"),
        );
        let (cdp_url, cdp_server) = spawn_delayed_cdp_identity_server(Duration::from_millis(200));

        let cli = cli_with(Commands::Doctor, &home);
        let request = ConnectionRequest::CdpUrl { url: cdp_url };
        let mut client = IpcClient::connect(&socket_path)
            .await
            .expect("connect validation client");

        let error = validate_existing_session_connection_request_with_deadline(
            &cli,
            &request,
            &mut client,
            Some("sess-live"),
            Some(Instant::now() + Duration::from_millis(50)),
            Some(50),
        )
        .await
        .expect_err("live attachment identity resolution must share the validation budget");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|value| value.get("phase"))
                .and_then(|value| value.as_str()),
            Some("existing_session_validation_attachment_identity_resolution")
        );

        handshake_server.await.expect("join handshake server");
        cdp_server.await.expect("join delayed cdp server");
        let _ = std::fs::remove_dir_all(&home);
    }
}
