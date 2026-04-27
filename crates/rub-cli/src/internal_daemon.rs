use crate::commands::EffectiveCli;
use crate::session_policy::ConnectionRequest;
use rub_daemon::rub_paths::RubPaths;

mod browser_bootstrap;
mod browser_callbacks;
mod observatory_ingress;
mod startup_bootstrap;
mod startup_inputs;
mod startup_reporting;

use self::browser_bootstrap::{attach_browser, build_browser_manager, set_handoff_projection};
use self::browser_callbacks::install_browser_callbacks;
use self::startup_bootstrap::{
    InternalDaemonPathContext, daemon_runtime_error, internal_daemon_local_io_error,
    resolve_cli_or_env_session_id, rub_home_startup_error,
};
use self::startup_inputs::resolve_startup_inputs;
use self::startup_reporting::{
    exit_startup_error, exit_startup_error_with_browser_cleanup, init_tracing, rotate_logs,
};

fn startup_cleanup_fallback_proof_for_request(
    connection_request: &ConnectionRequest,
    effective_user_data_dir: Option<&str>,
) -> Option<crate::daemon_ctl::StartupCleanupProof> {
    match connection_request {
        ConnectionRequest::CdpUrl { .. } | ConnectionRequest::AutoDiscover => None,
        ConnectionRequest::Profile { dir_name, .. } => {
            Some(crate::daemon_ctl::StartupCleanupProof {
                kind: crate::daemon_ctl::StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
                managed_user_data_dir: effective_user_data_dir?.to_string(),
                managed_profile_directory: Some(dir_name.clone()),
                ephemeral: false,
            })
        }
        ConnectionRequest::UserDataDir { .. } | ConnectionRequest::None => {
            Some(crate::daemon_ctl::StartupCleanupProof {
                kind: crate::daemon_ctl::StartupCleanupAuthorityKind::ManagedBrowserProfileFallback,
                managed_user_data_dir: effective_user_data_dir?.to_string(),
                managed_profile_directory: None,
                ephemeral: matches!(connection_request, ConnectionRequest::None),
            })
        }
    }
}

pub async fn run(cli: EffectiveCli) {
    let rub_home = cli.rub_home.clone();
    let session = cli.session.clone();

    if let Err(e) = std::fs::create_dir_all(&rub_home) {
        exit_startup_error(rub_home_startup_error(&rub_home, &e));
    }

    let rub_paths = RubPaths::new(&rub_home);
    if let Err(error) = rub_paths.mark_temp_home_owner_if_applicable() {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to mark temporary RUB_HOME ownership at {}: {error}",
                rub_paths.temp_home_owner_marker_path().display()
            ),
            InternalDaemonPathContext {
                path_key: "temp_home_owner_marker",
                path: &rub_paths.temp_home_owner_marker_path(),
                path_authority: "cli.internal_daemon.temp_home_owner_marker",
                upstream_truth: "cli_rub_home",
                path_kind: "temp_home_owner_marker",
                reason: "temp_home_owner_marker_write_failed",
            },
        ));
    }
    if let Err(error) = std::fs::create_dir_all(rub_paths.logs_dir()) {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to create daemon log directory {}: {error}",
                rub_paths.logs_dir().display()
            ),
            InternalDaemonPathContext {
                path_key: "logs_dir",
                path: &rub_paths.logs_dir(),
                path_authority: "cli.internal_daemon.logs_dir",
                upstream_truth: "cli_rub_home",
                path_kind: "daemon_logs_directory",
                reason: "logs_dir_create_failed",
            },
        ));
    }
    let session_id = match resolve_cli_or_env_session_id(&cli) {
        Ok(session_id) => session_id,
        Err(envelope) => exit_startup_error(envelope),
    };
    let session_paths = rub_paths.session_runtime(&session, &session_id);
    if let Err(error) = std::fs::create_dir_all(session_paths.session_dir()) {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to create session runtime directory {}: {error}",
                session_paths.session_dir().display()
            ),
            InternalDaemonPathContext {
                path_key: "session_dir",
                path: &session_paths.session_dir(),
                path_authority: "cli.internal_daemon.session_runtime_dir",
                upstream_truth: "startup_session_id",
                path_kind: "session_runtime_directory",
                reason: "session_runtime_dir_create_failed",
            },
        ));
    }
    if let Err(error) = std::fs::create_dir_all(session_paths.download_dir()) {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to create session download directory {}: {error}",
                session_paths.download_dir().display()
            ),
            InternalDaemonPathContext {
                path_key: "download_dir",
                path: &session_paths.download_dir(),
                path_authority: "cli.internal_daemon.download_dir",
                upstream_truth: "startup_session_id",
                path_kind: "session_download_directory",
                reason: "download_dir_create_failed",
            },
        ));
    }
    let log_path = rub_paths.daemon_log_path();
    if let Err(error) = rotate_logs(&log_path, 10 * 1024 * 1024, 3) {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to rotate daemon log {}: {error}",
                log_path.display()
            ),
            InternalDaemonPathContext {
                path_key: "log_path",
                path: &log_path,
                path_authority: "cli.internal_daemon.daemon_log",
                upstream_truth: "cli_rub_home",
                path_kind: "daemon_log_file",
                reason: "daemon_log_rotation_failed",
            },
        ));
    }
    if let Err(error) = init_tracing(&log_path) {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to initialize daemon tracing at {}: {error}",
                log_path.display()
            ),
            InternalDaemonPathContext {
                path_key: "log_path",
                path: &log_path,
                path_authority: "cli.internal_daemon.daemon_log",
                upstream_truth: "cli_rub_home",
                path_kind: "daemon_log_file",
                reason: "daemon_log_init_failed",
            },
        ));
    }

    let startup_inputs = match resolve_startup_inputs(&cli, &session_id).await {
        Ok(inputs) => inputs,
        Err(envelope) => exit_startup_error(envelope),
    };

    let state = std::sync::Arc::new(rub_daemon::session::SessionState::new_with_id(
        session.clone(),
        session_id,
        rub_home.clone(),
        startup_inputs.effective_user_data_dir.clone(),
    ));
    state
        .set_attachment_identity(startup_inputs.attachment_identity.clone())
        .await;
    let browser_manager = build_browser_manager(
        &cli,
        &startup_inputs.connection_request,
        startup_inputs.effective_user_data_dir.clone(),
        session_paths.download_dir(),
    );

    if let Some(cleanup_proof) = startup_cleanup_fallback_proof_for_request(
        &startup_inputs.connection_request,
        startup_inputs.effective_user_data_dir.as_deref(),
    ) && let Some(cleanup_path) = crate::daemon_ctl::startup_cleanup_signal_path()
        && let Err(error) =
            crate::daemon_ctl::write_startup_cleanup_proof_at(&cleanup_path, &cleanup_proof)
    {
        exit_startup_error(internal_daemon_local_io_error(
            &rub_home,
            format!(
                "Failed to publish startup cleanup proof {}: {error}",
                cleanup_path.display()
            ),
            InternalDaemonPathContext {
                path_key: "cleanup_file",
                path: &cleanup_path,
                path_authority: "cli.internal_daemon.startup_cleanup_file",
                upstream_truth: "startup_cleanup_signal_file",
                path_kind: "startup_cleanup_file",
                reason: "startup_cleanup_file_write_failed",
            },
        ));
    }

    let browser_event_sink = rub_daemon::session::BrowserSessionEventSink::new(&state);

    install_browser_callbacks(&browser_manager, &state, &browser_event_sink).await;

    if let Err(envelope) =
        attach_browser(&browser_manager, &state, &startup_inputs.connection_request).await
    {
        exit_startup_error_with_browser_cleanup(envelope, Some(&browser_manager)).await;
    }

    set_handoff_projection(&state, &browser_manager, cli.headed).await;

    let epoch = state.epoch_ref();
    let humanize_config = rub_cdp::humanize::HumanizeConfig {
        enabled: cli.humanize,
        speed: startup_inputs.humanize_speed,
    };
    let adapter =
        rub_cdp::adapter::ChromiumAdapter::new(browser_manager.clone(), epoch, humanize_config);
    let browser_port: std::sync::Arc<dyn rub_core::port::BrowserPort> =
        std::sync::Arc::new(adapter);
    let router = std::sync::Arc::new(rub_daemon::router::DaemonRouter::new(browser_port));

    if let Err(e) = rub_daemon::daemon::run_daemon(&session, &rub_home, router, state).await {
        exit_startup_error_with_browser_cleanup(
            daemon_runtime_error(&rub_home, e.to_string()),
            Some(&browser_manager),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InternalDaemonPathContext, rub_home_startup_error, startup_bootstrap::SESSION_ID_ENV,
        startup_bootstrap::internal_daemon_local_io_error,
        startup_bootstrap::resolve_startup_session_id, startup_cleanup_fallback_proof_for_request,
        startup_reporting::annotate_startup_error_with_browser_cleanup,
    };
    use crate::internal_daemon::startup_bootstrap::internal_daemon_path_state;
    use crate::session_policy::ConnectionRequest;
    use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
    use std::io;
    use std::path::Path;

    #[test]
    fn invalid_env_session_id_is_rejected_before_runtime_paths_are_derived() {
        unsafe {
            std::env::set_var(SESSION_ID_ENV, "../escape");
        }
        let error = resolve_startup_session_id().expect_err("invalid env session id must fail");
        assert_eq!(error.code, ErrorCode::DaemonStartFailed);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("invalid_session_id_component")
        );
        unsafe {
            std::env::remove_var(SESSION_ID_ENV);
        }
    }

    #[test]
    fn startup_cleanup_annotation_records_cleanup_failure_without_dropping_context() {
        let envelope = ErrorEnvelope::new(ErrorCode::DaemonStartFailed, "startup failed")
            .with_context(serde_json::json!({
                "reason": "forced_startup_failure",
            }));
        let annotated = annotate_startup_error_with_browser_cleanup(
            envelope,
            Err(RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                "cleanup failed",
            )),
        );
        let context = annotated.context.expect("cleanup context");
        assert_eq!(context["reason"], "forced_startup_failure");
        assert_eq!(context["startup_browser_cleanup_attempted"], true);
        assert_eq!(context["startup_browser_cleanup_succeeded"], false);
        assert_eq!(
            context["startup_browser_cleanup_error"],
            "BROWSER_LAUNCH_FAILED: cleanup failed"
        );
    }

    #[test]
    fn rub_home_startup_error_marks_rub_home_state() {
        let error = rub_home_startup_error(Path::new("/tmp/rub-home"), &io::Error::other("boom"));
        let context = error.context.expect("rub_home startup context");
        assert_eq!(context["reason"], "rub_home_create_failed");
        assert_eq!(
            context["rub_home_state"]["path_authority"],
            "cli.internal_daemon.rub_home"
        );
    }

    #[test]
    fn daemon_runtime_error_marks_rub_home_state() {
        let error = super::daemon_runtime_error(Path::new("/tmp/rub-home"), "boom".to_string());
        let context = error.context.expect("daemon runtime context");
        assert_eq!(context["reason"], "daemon_runtime_failed");
        assert_eq!(
            context["rub_home_state"]["path_authority"],
            "cli.internal_daemon.rub_home"
        );
    }

    #[test]
    fn internal_daemon_local_io_error_marks_path_state() {
        let error = internal_daemon_local_io_error(
            Path::new("/tmp/rub-home"),
            "log init failed",
            InternalDaemonPathContext {
                path_key: "log_path",
                path: Path::new("/tmp/rub-home/logs/daemon.log"),
                path_authority: "cli.internal_daemon.daemon_log",
                upstream_truth: "cli_rub_home",
                path_kind: "daemon_log_file",
                reason: "daemon_log_init_failed",
            },
        );
        let context = error.context.expect("local io context");
        assert_eq!(context["reason"], "daemon_log_init_failed");
        assert_eq!(context["log_path"], "/tmp/rub-home/logs/daemon.log");
        assert_eq!(
            context["log_path_state"]["path_authority"],
            "cli.internal_daemon.daemon_log"
        );
    }

    #[test]
    fn internal_daemon_path_state_marks_display_only_local_runtime_reference() {
        let state = internal_daemon_path_state(
            "cli.internal_daemon.rub_home",
            "cli_rub_home",
            "rub_home_directory",
        );
        assert_eq!(state.truth_level, "local_runtime_reference");
        assert_eq!(state.path_authority, "cli.internal_daemon.rub_home");
        assert_eq!(state.upstream_truth, "cli_rub_home");
        assert_eq!(state.path_kind, "rub_home_directory");
        assert_eq!(state.control_role, "display_only");
    }

    #[test]
    fn startup_cleanup_fallback_proof_is_published_for_managed_launches_only() {
        let managed = startup_cleanup_fallback_proof_for_request(
            &ConnectionRequest::None,
            Some("/tmp/rub-managed-profile"),
        )
        .expect("managed startup should publish cleanup proof");
        assert_eq!(managed.managed_user_data_dir, "/tmp/rub-managed-profile");
        assert_eq!(managed.managed_profile_directory, None);
        assert!(managed.ephemeral);
        assert_eq!(
            managed.kind,
            crate::daemon_ctl::StartupCleanupAuthorityKind::ManagedBrowserProfileFallback
        );

        let external = startup_cleanup_fallback_proof_for_request(
            &ConnectionRequest::CdpUrl {
                url: "http://127.0.0.1:9222/json/version".to_string(),
            },
            Some("/tmp/unused"),
        );
        assert!(external.is_none());
    }

    #[test]
    fn startup_cleanup_fallback_proof_preserves_profile_scoped_authority() {
        let proof = startup_cleanup_fallback_proof_for_request(
            &ConnectionRequest::Profile {
                name: "Work".to_string(),
                dir_name: "Profile 3".to_string(),
                resolved_path: "/Users/test/Chrome/Profile 3".to_string(),
                user_data_root: "/Users/test/Chrome".to_string(),
            },
            Some("/Users/test/Chrome"),
        )
        .expect("profile startup should publish cleanup proof");

        assert_eq!(proof.managed_user_data_dir, "/Users/test/Chrome");
        assert_eq!(
            proof.managed_profile_directory.as_deref(),
            Some("Profile 3")
        );
        assert!(!proof.ephemeral);
    }
}
