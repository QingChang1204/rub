use rub_core::error::{ErrorEnvelope, RubError};
use rub_core::fs::FileCommitOutcome;
use std::path::Path;
use std::sync::Arc;

pub(super) fn exit_startup_error(envelope: ErrorEnvelope) -> ! {
    let envelope = finalize_startup_error_reporting(envelope);
    eprintln!("{envelope}");
    std::process::exit(1);
}

pub(super) async fn exit_startup_error_with_browser_cleanup(
    envelope: ErrorEnvelope,
    browser_manager: Option<&Arc<rub_cdp::browser::BrowserManager>>,
) -> ! {
    let cleanup_result = if let Some(browser_manager) = browser_manager {
        Some(browser_manager.close().await)
    } else {
        None
    };
    if cleanup_result.as_ref().is_some_and(|result| result.is_ok())
        && let Some(cleanup_path) = crate::daemon_ctl::startup_cleanup_signal_path()
    {
        let _ = crate::daemon_ctl::clear_startup_cleanup_proof(&cleanup_path);
    }
    let envelope = if let Some(cleanup_result) = cleanup_result {
        annotate_startup_error_with_browser_cleanup(envelope, cleanup_result)
    } else {
        envelope
    };

    exit_startup_error(envelope);
}

pub(super) fn annotate_startup_error_with_browser_cleanup(
    mut envelope: ErrorEnvelope,
    cleanup_result: Result<(), RubError>,
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

fn finalize_startup_error_reporting(mut envelope: ErrorEnvelope) -> ErrorEnvelope {
    if let Err(error) = write_startup_error(&envelope) {
        let mut context = envelope
            .context
            .take()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        context.insert(
            "startup_error_write_succeeded".to_string(),
            serde_json::json!(false),
        );
        context.insert(
            "startup_error_write_error".to_string(),
            serde_json::json!(error.to_string()),
        );
        envelope.context = Some(serde_json::Value::Object(context));
    }
    envelope
}

pub(super) fn write_startup_error(envelope: &ErrorEnvelope) -> std::io::Result<FileCommitOutcome> {
    let (_, error_path) = crate::daemon_ctl::startup_signal_paths();
    let Some(path) = error_path else {
        return Err(std::io::Error::other(
            "startup error signal path is not configured",
        ));
    };
    let json = serde_json::to_string(envelope)
        .map_err(|error| std::io::Error::other(format!("serialize startup error: {error}")))?;
    rub_core::fs::atomic_write_bytes(&path, json.as_bytes(), 0o600)
}

pub(super) fn init_tracing(log_path: &Path) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .try_init()
        .map_err(|error| std::io::Error::other(format!("initialize tracing subscriber: {error}")))
}

pub(super) fn rotate_logs(path: &Path, max_bytes: u64, retention: usize) -> std::io::Result<()> {
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
            std::fs::rename(&src, &dst)?;
        }
    }

    let rotated = path.with_file_name("daemon.log.1");
    std::fs::rename(path, rotated)?;
    Ok(())
}
