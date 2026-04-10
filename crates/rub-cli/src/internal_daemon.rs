use crate::commands::EffectiveCli;
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
    daemon_runtime_error, resolve_cli_or_env_session_id, rub_home_startup_error,
};
use self::startup_inputs::resolve_startup_inputs;
use self::startup_reporting::{
    exit_startup_error_with_browser_cleanup, init_tracing, rotate_logs, write_startup_error,
};

pub async fn run(cli: EffectiveCli) {
    let rub_home = cli.rub_home.clone();
    let session = cli.session.clone();

    if let Err(e) = std::fs::create_dir_all(&rub_home) {
        let envelope = rub_home_startup_error(&rub_home, &e);
        write_startup_error(&envelope);
        eprintln!("{envelope}");
        std::process::exit(1);
    }

    let rub_paths = RubPaths::new(&rub_home);
    let _ = rub_paths.mark_temp_home_owner_if_applicable();
    let _ = std::fs::create_dir_all(rub_paths.logs_dir());
    let session_id = match resolve_cli_or_env_session_id(&cli) {
        Ok(session_id) => session_id,
        Err(envelope) => {
            write_startup_error(&envelope);
            eprintln!("{envelope}");
            std::process::exit(1);
        }
    };
    let session_paths = rub_paths.session_runtime(&session, &session_id);
    let _ = std::fs::create_dir_all(session_paths.session_dir());
    let _ = std::fs::create_dir_all(session_paths.download_dir());
    let log_path = rub_paths.daemon_log_path();
    let _ = rotate_logs(&log_path, 10 * 1024 * 1024, 3);
    init_tracing(&log_path);

    let startup_inputs = match resolve_startup_inputs(&cli, &session_id).await {
        Ok(inputs) => inputs,
        Err(envelope) => {
            write_startup_error(&envelope);
            eprintln!("{envelope}");
            std::process::exit(1);
        }
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
        rub_home_startup_error, startup_bootstrap::SESSION_ID_ENV,
        startup_bootstrap::resolve_startup_session_id,
        startup_reporting::annotate_startup_error_with_browser_cleanup,
    };
    use crate::internal_daemon::startup_bootstrap::internal_daemon_path_state;
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
}
