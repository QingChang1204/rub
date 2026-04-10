use rub_core::error::{ErrorEnvelope, RubError};
use std::path::Path;
use std::sync::Arc;

pub(super) async fn exit_startup_error_with_browser_cleanup(
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

pub(super) fn write_startup_error(envelope: &ErrorEnvelope) {
    let (_, error_path) = crate::daemon_ctl::startup_signal_paths();
    if let Some(path) = error_path
        && let Ok(json) = serde_json::to_string(envelope)
    {
        let _ = rub_core::fs::atomic_write_bytes(&path, json.as_bytes(), 0o600);
    }
}

pub(super) fn init_tracing(log_path: &Path) {
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
            let _ = std::fs::rename(&src, &dst);
        }
    }

    let rotated = path.with_file_name("daemon.log.1");
    let _ = std::fs::rename(path, rotated);
    Ok(())
}
