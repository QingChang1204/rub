use crate::commands::EffectiveCli;
use crate::session_policy::{
    ConnectionRequest, effective_attachment_identity, materialize_connection_request,
    parse_connection_request, requested_user_data_dir,
};
use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{
    ConnectionTarget, ConsoleErrorEvent, NetworkFailureEvent, NetworkRequestRecord, PageErrorEvent,
    RequestSummaryEvent,
};
use rub_daemon::rub_paths::RubPaths;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

enum ObservatoryMutation {
    ConsoleError(ConsoleErrorEvent),
    PageError(PageErrorEvent),
    NetworkFailure(NetworkFailureEvent),
    RequestSummary(RequestSummaryEvent),
}

const SESSION_ID_ENV: &str = "RUB_SESSION_ID";
const OBSERVATORY_INGRESS_LIMIT: usize = 1_024;
const NETWORK_REQUEST_INGRESS_LIMIT: usize = 1_024;

fn resolve_startup_session_id() -> Result<String, ErrorEnvelope> {
    let Some(session_id) = std::env::var(SESSION_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(rub_daemon::session::new_session_id());
    };

    rub_daemon::rub_paths::validate_session_id_component(&session_id).map_err(|reason| {
        ErrorEnvelope::new(
            ErrorCode::DaemonStartFailed,
            format!("Invalid {SESSION_ID_ENV}: {reason}"),
        )
        .with_context(serde_json::json!({
            "env": SESSION_ID_ENV,
            "session_id": session_id,
            "reason": "invalid_session_id_component",
        }))
    })?;

    Ok(session_id)
}

fn resolve_cli_or_env_session_id(cli: &EffectiveCli) -> Result<String, ErrorEnvelope> {
    if let Some(session_id) = cli.session_id.as_deref() {
        rub_daemon::rub_paths::validate_session_id_component(session_id).map_err(|reason| {
            ErrorEnvelope::new(
                ErrorCode::DaemonStartFailed,
                format!("Invalid --session-id: {reason}"),
            )
            .with_context(serde_json::json!({
                "flag": "--session-id",
                "session_id": session_id,
                "reason": "invalid_session_id_component",
            }))
        })?;
        return Ok(session_id.to_string());
    }
    resolve_startup_session_id()
}

fn enqueue_observatory_mutation(
    tx: &tokio::sync::mpsc::Sender<ObservatoryMutation>,
    mutation: ObservatoryMutation,
    state: &Arc<rub_daemon::session::SessionState>,
    overflowed: &Arc<AtomicBool>,
) {
    if tx.try_send(mutation).is_ok() {
        return;
    }

    let _ = state.record_observatory_ingress_overflow();

    if !overflowed.swap(true, Ordering::SeqCst) {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .mark_observatory_degraded("observatory_ingress_overflow")
                .await;
        });
    }
}

fn enqueue_network_request_record(
    tx: &tokio::sync::mpsc::Sender<Box<NetworkRequestRecord>>,
    record: NetworkRequestRecord,
    state: &Arc<rub_daemon::session::SessionState>,
    overflowed: &Arc<AtomicBool>,
) {
    if tx.try_send(Box::new(record)).is_ok() {
        return;
    }

    let _ = state.record_network_request_ingress_overflow();
    state.network_request_notifier().notify_waiters();

    if !overflowed.swap(true, Ordering::SeqCst) {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .mark_observatory_degraded("network_request_ingress_overflow")
                .await;
        });
    }
}

pub async fn run(cli: EffectiveCli) {
    let rub_home = cli.rub_home.clone();
    let session = cli.session.clone();

    if let Err(e) = std::fs::create_dir_all(&rub_home) {
        let envelope = ErrorEnvelope::new(
            ErrorCode::DaemonStartFailed,
            format!("Cannot create RUB_HOME {}: {e}", rub_home.display()),
        );
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

    let connection_request = match parse_connection_request(&cli) {
        Ok(request) => request,
        Err(error) => {
            let envelope = error.into_envelope();
            write_startup_error(&envelope);
            eprintln!("{envelope}");
            std::process::exit(1);
        }
    };
    let connection_request = match materialize_connection_request(&connection_request).await {
        Ok(request) => request,
        Err(error) => {
            let envelope = error.into_envelope();
            write_startup_error(&envelope);
            eprintln!("{envelope}");
            std::process::exit(1);
        }
    };
    let effective_user_data_dir =
        requested_user_data_dir(&cli, &connection_request).or_else(|| {
            matches!(connection_request, ConnectionRequest::None).then(|| {
                rub_cdp::projected_managed_profile_path(None)
                    .display()
                    .to_string()
            })
        });
    let attachment_identity = match effective_attachment_identity(
        &cli,
        &connection_request,
        effective_user_data_dir.as_deref(),
    )
    .await
    {
        Ok(identity) => identity,
        Err(error) => {
            let envelope = error.into_envelope();
            write_startup_error(&envelope);
            eprintln!("{envelope}");
            std::process::exit(1);
        }
    };

    if let Some(attachment_identity) = attachment_identity.as_deref() {
        match rub_daemon::session::check_profile_in_use(
            &rub_home,
            attachment_identity,
            Some(session_id.as_str()),
        ) {
            Ok(Some(conflicting_session)) => {
                let envelope = ErrorEnvelope::new(
                    ErrorCode::ProfileInUse,
                    format!(
                        "Browser attachment {attachment_identity} is already used by session {conflicting_session}"
                    ),
                );
                write_startup_error(&envelope);
                eprintln!("{envelope}");
                std::process::exit(1);
            }
            Ok(None) => {}
            Err(error) => {
                let envelope = ErrorEnvelope::new(
                    ErrorCode::DaemonStartFailed,
                    format!(
                        "Failed to verify browser attachment ownership for {attachment_identity}: {error}"
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "attachment_ownership_check_failed",
                    "attachment_identity": attachment_identity,
                }));
                write_startup_error(&envelope);
                eprintln!("{envelope}");
                std::process::exit(1);
            }
        }
    }

    let state = std::sync::Arc::new(rub_daemon::session::SessionState::new_with_id(
        session.clone(),
        session_id,
        rub_home.clone(),
        effective_user_data_dir.clone(),
    ));
    state.set_attachment_identity(attachment_identity).await;
    let browser_manager = std::sync::Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: !cli.headed,
            ignore_cert_errors: cli.ignore_cert_errors,
            user_data_dir: effective_user_data_dir
                .clone()
                .map(std::path::PathBuf::from),
            download_dir: Some(session_paths.download_dir()),
            profile_directory: match &connection_request {
                ConnectionRequest::Profile { dir_name, .. } => Some(dir_name.clone()),
                _ => None,
            },
            hide_infobars: cli.hide_infobars,
            stealth: !cli.no_stealth,
        },
    ));

    let Some(humanize_speed) = rub_cdp::humanize::HumanizeSpeed::from_str_opt(&cli.humanize_speed)
    else {
        let envelope = ErrorEnvelope::new(
            ErrorCode::InvalidInput,
            format!(
                "Unsupported humanize speed '{}'; use fast, normal, or slow",
                cli.humanize_speed
            ),
        );
        write_startup_error(&envelope);
        eprintln!("{envelope}");
        std::process::exit(1);
    };

    let browser_event_sink = rub_daemon::session::BrowserSessionEventSink::new(&state);

    {
        let (observatory_tx, mut observatory_rx) =
            tokio::sync::mpsc::channel::<ObservatoryMutation>(OBSERVATORY_INGRESS_LIMIT);
        let (network_request_tx, mut network_request_rx) =
            tokio::sync::mpsc::channel::<Box<NetworkRequestRecord>>(NETWORK_REQUEST_INGRESS_LIMIT);
        let observatory_state = state.clone();
        tokio::spawn(async move {
            while let Some(mutation) = observatory_rx.recv().await {
                match mutation {
                    ObservatoryMutation::ConsoleError(event) => {
                        observatory_state.record_console_error(event).await;
                    }
                    ObservatoryMutation::PageError(event) => {
                        observatory_state.record_page_error(event).await;
                    }
                    ObservatoryMutation::NetworkFailure(event) => {
                        observatory_state.record_network_failure(event).await;
                    }
                    ObservatoryMutation::RequestSummary(event) => {
                        observatory_state.record_request_summary(event).await;
                    }
                }
            }
        });
        let request_record_state = state.clone();
        tokio::spawn(async move {
            while let Some(record) = network_request_rx.recv().await {
                request_record_state
                    .upsert_network_request_record(*record)
                    .await;
            }
        });
        let observatory_overflowed = Arc::new(AtomicBool::new(false));
        let network_request_overflowed = Arc::new(AtomicBool::new(false));
        if let Err(error) = browser_manager
            .set_observatory_callbacks(rub_cdp::runtime_observatory::ObservatoryCallbacks {
                on_console_error: Some(std::sync::Arc::new({
                    let observatory_tx = observatory_tx.clone();
                    let observatory_state = state.clone();
                    let observatory_overflowed = observatory_overflowed.clone();
                    move |event| {
                        enqueue_observatory_mutation(
                            &observatory_tx,
                            ObservatoryMutation::ConsoleError(event),
                            &observatory_state,
                            &observatory_overflowed,
                        );
                    }
                })),
                on_page_error: Some(std::sync::Arc::new({
                    let observatory_tx = observatory_tx.clone();
                    let observatory_state = state.clone();
                    let observatory_overflowed = observatory_overflowed.clone();
                    move |event| {
                        enqueue_observatory_mutation(
                            &observatory_tx,
                            ObservatoryMutation::PageError(event),
                            &observatory_state,
                            &observatory_overflowed,
                        );
                    }
                })),
                on_network_failure: Some(std::sync::Arc::new({
                    let observatory_tx = observatory_tx.clone();
                    let observatory_state = state.clone();
                    let observatory_overflowed = observatory_overflowed.clone();
                    move |event| {
                        enqueue_observatory_mutation(
                            &observatory_tx,
                            ObservatoryMutation::NetworkFailure(event),
                            &observatory_state,
                            &observatory_overflowed,
                        );
                    }
                })),
                on_request_summary: Some(std::sync::Arc::new({
                    let observatory_tx = observatory_tx.clone();
                    let observatory_state = state.clone();
                    let observatory_overflowed = observatory_overflowed.clone();
                    move |event| {
                        enqueue_observatory_mutation(
                            &observatory_tx,
                            ObservatoryMutation::RequestSummary(event),
                            &observatory_state,
                            &observatory_overflowed,
                        );
                    }
                })),
                on_request_record: Some(std::sync::Arc::new({
                    let observatory_state = state.clone();
                    let network_request_tx = network_request_tx.clone();
                    let network_request_overflowed = network_request_overflowed.clone();
                    move |record| {
                        enqueue_network_request_record(
                            &network_request_tx,
                            record,
                            &observatory_state,
                            &network_request_overflowed,
                        );
                    }
                })),
                on_runtime_degraded: Some(std::sync::Arc::new({
                    let observatory_state = state.clone();
                    move |reason| {
                        let observatory_state = observatory_state.clone();
                        tokio::spawn(async move {
                            observatory_state.mark_observatory_degraded(reason).await;
                        });
                    }
                })),
            })
            .await
        {
            state
                .mark_observatory_degraded(format!("observatory_callback_install_failed:{error}"))
                .await;
        }
    }
    {
        let runtime_state = state.clone();
        let runtime_sequence_state = state.clone();
        if let Err(error) = browser_manager
            .set_runtime_state_callbacks(rub_cdp::runtime_state::RuntimeStateCallbacks {
                allocate_sequence: Some(std::sync::Arc::new(move || {
                    runtime_sequence_state.allocate_runtime_state_sequence()
                })),
                on_snapshot: Some(std::sync::Arc::new(move |sequence, snapshot| {
                    let state = runtime_state.clone();
                    tokio::spawn(async move {
                        state
                            .publish_runtime_state_snapshot(sequence, snapshot)
                            .await;
                    });
                })),
            })
            .await
        {
            let sequence = state.allocate_runtime_state_sequence();
            state
                .mark_runtime_state_probe_degraded(
                    sequence,
                    format!("runtime_state_callback_install_failed:{error}"),
                )
                .await;
        }
    }
    {
        let runtime_state = state.clone();
        let opened_state = state.clone();
        let closed_state = state.clone();
        let runtime_event_sink = browser_event_sink.clone();
        let opened_event_sink = browser_event_sink.clone();
        let closed_event_sink = browser_event_sink.clone();
        if let Err(error) = browser_manager
            .set_dialog_callbacks(rub_cdp::dialogs::DialogCallbacks {
                on_runtime: Some(std::sync::Arc::new(move |runtime| {
                    let browser_sequence = runtime_state.allocate_browser_event_sequence();
                    runtime_event_sink.enqueue(
                        rub_daemon::session::BrowserSessionEvent::DialogRuntime {
                            browser_sequence,
                            generation: runtime.generation,
                            status: runtime.runtime.status,
                            degraded_reason: runtime.runtime.degraded_reason,
                        },
                    );
                })),
                on_opened: Some(std::sync::Arc::new(move |event| {
                    let browser_sequence = opened_state.allocate_browser_event_sequence();
                    opened_event_sink.enqueue(
                        rub_daemon::session::BrowserSessionEvent::DialogOpened {
                            browser_sequence,
                            generation: event.generation,
                            kind: event.kind,
                            message: event.message,
                            url: event.url,
                            tab_target_id: event.tab_target_id,
                            frame_id: event.frame_id,
                            default_prompt: event.default_prompt,
                            has_browser_handler: event.has_browser_handler,
                        },
                    );
                })),
                on_closed: Some(std::sync::Arc::new(move |event| {
                    let browser_sequence = closed_state.allocate_browser_event_sequence();
                    closed_event_sink.enqueue(
                        rub_daemon::session::BrowserSessionEvent::DialogClosed {
                            browser_sequence,
                            generation: event.generation,
                            accepted: event.accepted,
                            user_input: event.user_input,
                        },
                    );
                })),
            })
            .await
        {
            state
                .mark_dialog_runtime_degraded(
                    browser_manager.current_listener_generation(),
                    format!("dialog_callback_install_failed:{error}"),
                )
                .await;
        }
    }
    {
        let runtime_state = state.clone();
        let started_state = state.clone();
        let progress_state = state.clone();
        let runtime_event_sink = browser_event_sink.clone();
        let started_event_sink = browser_event_sink.clone();
        let progress_event_sink = browser_event_sink.clone();
        if let Err(error) = browser_manager
            .set_download_callbacks(rub_cdp::downloads::DownloadCallbacks {
                on_runtime: Some(std::sync::Arc::new(move |runtime| {
                    let browser_sequence = runtime_state.allocate_browser_event_sequence();
                    runtime_event_sink.enqueue(
                        rub_daemon::session::BrowserSessionEvent::DownloadRuntime {
                            browser_sequence,
                            generation: runtime.generation,
                            status: runtime.runtime.status,
                            mode: runtime.runtime.mode,
                            download_dir: runtime.runtime.download_dir,
                            degraded_reason: runtime.runtime.degraded_reason,
                        },
                    );
                })),
                on_started: Some(std::sync::Arc::new(move |event| {
                    let browser_sequence = started_state.allocate_browser_event_sequence();
                    started_event_sink.enqueue(
                        rub_daemon::session::BrowserSessionEvent::DownloadStarted {
                            browser_sequence,
                            generation: event.generation,
                            guid: event.guid,
                            url: event.url,
                            suggested_filename: event.suggested_filename,
                            frame_id: event.frame_id,
                        },
                    );
                })),
                on_progress: Some(std::sync::Arc::new(move |event| {
                    let browser_sequence = progress_state.allocate_browser_event_sequence();
                    progress_event_sink.enqueue(
                        rub_daemon::session::BrowserSessionEvent::DownloadProgress {
                            browser_sequence,
                            generation: event.generation,
                            guid: event.guid,
                            state: event.state,
                            received_bytes: event.received_bytes,
                            total_bytes: event.total_bytes,
                            final_path: event.final_path,
                        },
                    );
                })),
            })
            .await
        {
            state
                .mark_download_runtime_degraded(
                    browser_manager.current_listener_generation(),
                    format!("download_callback_install_failed:{error}"),
                )
                .await;
        }
    }

    if let ConnectionRequest::Profile {
        name,
        resolved_path,
        ..
    } = &connection_request
    {
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

    let browser_result = match &connection_request {
        ConnectionRequest::CdpUrl { url } => {
            let canonical_url =
                match rub_cdp::attachment::canonical_external_browser_identity(url).await {
                    Ok(url) => url,
                    Err(error) => {
                        let envelope = error.into_envelope();
                        write_startup_error(&envelope);
                        eprintln!("{envelope}");
                        std::process::exit(1);
                    }
                };
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
        ConnectionRequest::Profile { .. } | ConnectionRequest::None => {
            if matches!(connection_request, ConnectionRequest::None) {
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

    if let Err(e) = browser_result {
        exit_startup_error_with_browser_cleanup(e.into_envelope(), Some(&browser_manager)).await;
    }

    if cli.headed || browser_manager.is_external().await {
        state.set_handoff_available(true).await;
    } else {
        state
            .set_human_verification_handoff(rub_core::model::HumanVerificationHandoffInfo {
                unavailable_reason: Some("session_not_user_accessible".to_string()),
                ..rub_core::model::HumanVerificationHandoffInfo::default()
            })
            .await;
    }

    let epoch = state.epoch_ref();
    let humanize_config = rub_cdp::humanize::HumanizeConfig {
        enabled: cli.humanize,
        speed: humanize_speed,
    };
    let adapter =
        rub_cdp::adapter::ChromiumAdapter::new(browser_manager.clone(), epoch, humanize_config);
    let browser_port: std::sync::Arc<dyn rub_core::port::BrowserPort> =
        std::sync::Arc::new(adapter);
    let router = std::sync::Arc::new(rub_daemon::router::DaemonRouter::new(browser_port));

    {
        let state_for_callback = state.clone();
        browser_manager
            .set_epoch_callback(Box::new(move || {
                let _ = state_for_callback.observe_external_dom_change();
            }))
            .await;
    }

    if let Err(e) = rub_daemon::daemon::run_daemon(&session, &rub_home, router, state).await {
        exit_startup_error_with_browser_cleanup(
            ErrorEnvelope::new(ErrorCode::DaemonStartFailed, e.to_string()),
            Some(&browser_manager),
        )
        .await;
    }
}

async fn exit_startup_error_with_browser_cleanup(
    envelope: ErrorEnvelope,
    browser_manager: Option<&Arc<rub_cdp::browser::BrowserManager>>,
) -> ! {
    let envelope = if let Some(browser_manager) = browser_manager {
        annotate_startup_error_with_browser_cleanup(envelope, browser_manager.close().await)
    } else {
        envelope
    };

    write_startup_error(&envelope);
    eprintln!("{envelope}");
    std::process::exit(1);
}

fn annotate_startup_error_with_browser_cleanup(
    mut envelope: ErrorEnvelope,
    cleanup_result: Result<(), rub_core::error::RubError>,
) -> ErrorEnvelope {
    let mut context = envelope
        .context
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    context.insert(
        "startup_browser_cleanup_attempted".to_string(),
        serde_json::json!(true),
    );
    context.insert(
        "startup_browser_cleanup_succeeded".to_string(),
        serde_json::json!(cleanup_result.is_ok()),
    );
    if let Err(error) = cleanup_result {
        context.insert(
            "startup_browser_cleanup_error".to_string(),
            serde_json::json!(error.to_string()),
        );
    }
    envelope.context = Some(serde_json::Value::Object(context));
    envelope
}

fn write_startup_error(envelope: &ErrorEnvelope) {
    let (_, error_path) = crate::daemon_ctl::startup_signal_paths();
    if let Some(path) = error_path
        && let Ok(json) = serde_json::to_string(envelope)
    {
        let _ = rub_core::fs::atomic_write_bytes(&path, json.as_bytes(), 0o600);
    }
}

fn init_tracing(log_path: &std::path::Path) {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path);

    match log_file {
        Ok(file) => {
            tracing_subscriber::fmt()
                .json()
                .with_target(false)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
        }
        Err(_) => {
            tracing_subscriber::fmt()
                .with_target(false)
                .with_ansi(false)
                .init();
        }
    }
}

fn rotate_logs(path: &std::path::Path, max_bytes: u64, retention: usize) -> std::io::Result<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < max_bytes {
        return Ok(());
    }

    for index in (1..retention).rev() {
        let src = path.with_file_name(format!("daemon.log.{index}"));
        let dst = path.with_file_name(format!("daemon.log.{}", index + 1));
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
    }

    let rotated = path.with_file_name("daemon.log.1");
    let _ = std::fs::rename(path, rotated);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        SESSION_ID_ENV, annotate_startup_error_with_browser_cleanup, resolve_startup_session_id,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};

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
}
