use std::time::Duration;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{DownloadEntry, DownloadRuntimeInfo, DownloadState};

use crate::router::artifacts::annotate_operator_path_reference_state;

pub(super) fn download_wait_timeout_error(
    target_guid: Option<&str>,
    desired_state: &str,
    timeout: Duration,
    elapsed: Duration,
    runtime: DownloadRuntimeInfo,
    current_download: Option<DownloadEntry>,
) -> RubError {
    let mut context = serde_json::json!({
        "kind": "download",
        "id": target_guid,
        "state": desired_state,
        "timeout_ms": timeout.as_millis() as u64,
        "elapsed_ms": elapsed.as_millis() as u64,
        "download_runtime": runtime,
    });
    if let Some(download_runtime) = context.get_mut("download_runtime") {
        annotate_download_runtime_path_states(download_runtime);
    }

    if let Some(download) = current_download.as_ref() {
        context["download"] = serde_json::to_value(download).unwrap_or(serde_json::Value::Null);
    }

    let message = match current_download.as_ref() {
        Some(download) => format!(
            "Download wait timed out before '{desired_state}' was observed; current state is '{}'{}",
            download_state_label(download.state),
            format_download_progress(download)
        ),
        None => format!(
            "Download wait timed out before '{desired_state}' was observed; no matching download has been observed yet"
        ),
    };
    let suggestion = match current_download.as_ref() {
        Some(download)
            if matches!(
                download.state,
                DownloadState::Started | DownloadState::InProgress
            ) =>
        {
            "Download is still in progress. Run 'rub downloads' to inspect current progress, or retry with a longer --timeout.".to_string()
        }
        Some(_) => {
            "Run 'rub downloads' to inspect the latest lifecycle state and verify the requested wait target.".to_string()
        }
        None => {
            "Run 'rub downloads' to confirm the download started, or retry with a longer --timeout if the browser has not emitted the first download event yet.".to_string()
        }
    };

    RubError::domain_with_context_and_suggestion(
        ErrorCode::WaitTimeout,
        message,
        context,
        suggestion,
    )
}

pub(super) fn download_payload(
    subject: serde_json::Value,
    result: serde_json::Value,
    runtime: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
        "runtime": runtime,
    })
}

pub(super) fn download_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "download_registry",
    })
}

pub(super) fn download_subject(
    id: Option<&str>,
    operation: &str,
    wait_state: Option<&str>,
) -> serde_json::Value {
    let mut subject = serde_json::Map::new();
    subject.insert(
        "kind".to_string(),
        serde_json::Value::String("download".to_string()),
    );
    subject.insert(
        "operation".to_string(),
        serde_json::Value::String(operation.to_string()),
    );
    if let Some(id) = id {
        subject.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    }
    if let Some(wait_state) = wait_state {
        subject.insert(
            "wait_state".to_string(),
            serde_json::Value::String(wait_state.to_string()),
        );
    }
    serde_json::Value::Object(subject)
}

pub(super) fn download_registry_result(runtime: &DownloadRuntimeInfo) -> serde_json::Value {
    serde_json::json!({
        "download_dir": runtime.download_dir,
        "active_downloads": runtime.active_downloads,
        "completed_downloads": runtime.completed_downloads,
        "last_download": runtime.last_download,
    })
}

pub(super) fn annotate_download_runtime_path_states(runtime: &mut serde_json::Value) {
    if runtime
        .get("download_dir")
        .is_some_and(|value| !value.is_null())
    {
        annotate_operator_path_reference_state(
            runtime,
            "download_dir_state",
            "session.download_runtime.download_dir",
            "session_download_runtime",
            "managed_download_directory",
        );
    }
}

fn download_state_label(state: DownloadState) -> &'static str {
    match state {
        DownloadState::Started => "started",
        DownloadState::InProgress => "in_progress",
        DownloadState::Completed => "completed",
        DownloadState::Failed => "failed",
        DownloadState::Canceled => "canceled",
    }
}

fn format_download_progress(download: &DownloadEntry) -> String {
    match download.total_bytes {
        Some(total_bytes) if total_bytes > 0 => {
            format!(" ({} / {} bytes)", download.received_bytes, total_bytes)
        }
        _ if download.received_bytes > 0 => {
            format!(" ({} bytes received)", download.received_bytes)
        }
        _ => String::new(),
    }
}
