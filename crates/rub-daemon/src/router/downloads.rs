use std::sync::Arc;
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{DownloadEntry, DownloadRuntimeStatus, DownloadState};

use crate::router::request_args::parse_json_args;
use crate::session::SessionState;

use super::DaemonRouter;

mod asset_save;

use asset_save::DownloadSaveArgs;

#[derive(Debug)]
enum DownloadCommand {
    Wait(DownloadWaitArgs),
    Cancel(DownloadCancelArgs),
    Save(DownloadSaveArgs),
}

impl DownloadCommand {
    fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("wait")
        {
            "wait" => Ok(Self::Wait(parse_json_args(args, "download wait")?)),
            "cancel" => Ok(Self::Cancel(parse_json_args(args, "download cancel")?)),
            "save" => Ok(Self::Save(parse_json_args(args, "download save")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown download subcommand: '{other}'"),
            )),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadWaitArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadCancelArgs {
    #[serde(rename = "sub")]
    _sub: String,
    id: String,
}

pub(super) async fn cmd_downloads(
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let runtime = state.download_runtime().await;
    Ok(download_payload(
        download_registry_subject(),
        download_registry_result(&runtime),
        serde_json::to_value(runtime).map_err(RubError::from)?,
    ))
}

pub(super) async fn cmd_download(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    match DownloadCommand::parse(args)? {
        DownloadCommand::Wait(args) => cmd_download_wait(args, state).await,
        DownloadCommand::Cancel(args) => cmd_download_cancel(router, args, state).await,
        DownloadCommand::Save(args) => asset_save::cmd_download_save(router, args).await,
    }
}

async fn cmd_download_wait(
    args: DownloadWaitArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let desired_state = parse_wait_state(args.state.as_deref())?;
    let cursor = state.download_cursor().await;
    let started = Instant::now();
    let poll_interval = Duration::from_millis(50);
    let timeout = Duration::from_millis(args.timeout_ms.unwrap_or(30_000));

    let initial_runtime = state.download_runtime().await;
    let mut target_guid = args.id.clone().or_else(|| {
        initial_runtime
            .last_download
            .as_ref()
            .map(|download| download.guid.clone())
    });
    if matches!(initial_runtime.status, DownloadRuntimeStatus::Unsupported) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Download runtime is unsupported for this session",
        ));
    }

    loop {
        if target_guid.is_none() {
            target_guid = state
                .download_events_after(cursor)
                .await
                .into_iter()
                .last()
                .map(|event| event.download.guid);
        }

        let runtime = state.download_runtime().await;
        if let Some(guid) = target_guid.as_deref()
            && let Some(download) = state.download_entry(guid).await
            && matches_wait_state(&download, desired_state)
        {
            return Ok(download_payload(
                download_subject(Some(guid), "wait", Some(wait_state_label(desired_state))),
                serde_json::json!({
                    "download": download,
                    "matched": true,
                }),
                serde_json::to_value(runtime).map_err(RubError::from)?,
            ));
        }

        if started.elapsed() >= timeout {
            let runtime = state.download_runtime().await;
            let current_download = match target_guid.as_deref() {
                Some(guid) => state.download_entry(guid).await,
                None => runtime.last_download.clone(),
            };
            return Err(download_wait_timeout_error(
                target_guid.as_deref(),
                args.state.as_deref().unwrap_or("completed"),
                timeout,
                started.elapsed(),
                runtime,
                current_download,
            ));
        }

        tokio::time::sleep(poll_interval).await;
    }
}

async fn cmd_download_cancel(
    router: &DaemonRouter,
    args: DownloadCancelArgs,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let guid = args.id;
    router.browser.cancel_download(&guid).await?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let runtime = state.download_runtime().await;
        if let Some(download) = state.download_entry(&guid).await
            && download.state == DownloadState::Canceled
        {
            return Ok(download_payload(
                download_subject(Some(guid.as_str()), "cancel", None),
                serde_json::json!({
                    "download": download,
                }),
                serde_json::to_value(runtime).map_err(RubError::from)?,
            ));
        }
        if Instant::now() >= deadline {
            return Err(RubError::domain_with_context(
                ErrorCode::WaitTimeout,
                format!("Timed out waiting for download '{guid}' to reach canceled state"),
                serde_json::json!({
                    "kind": "download",
                    "id": guid,
                    "state": "canceled",
                    "timeout_ms": 2_000u64,
                    "download_runtime": runtime,
                    "download": state.download_entry(&guid).await,
                }),
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadWaitState {
    Started,
    InProgress,
    Completed,
    Failed,
    Canceled,
    Terminal,
}

fn parse_wait_state(value: Option<&str>) -> Result<DownloadWaitState, RubError> {
    match value.unwrap_or("completed") {
        "started" => Ok(DownloadWaitState::Started),
        "in_progress" => Ok(DownloadWaitState::InProgress),
        "completed" => Ok(DownloadWaitState::Completed),
        "failed" => Ok(DownloadWaitState::Failed),
        "canceled" => Ok(DownloadWaitState::Canceled),
        "terminal" => Ok(DownloadWaitState::Terminal),
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown download wait state '{other}'"),
        )),
    }
}

fn matches_wait_state(download: &DownloadEntry, desired: DownloadWaitState) -> bool {
    match desired {
        DownloadWaitState::Started => download.state == DownloadState::Started,
        DownloadWaitState::InProgress => download.state == DownloadState::InProgress,
        DownloadWaitState::Completed => download.state == DownloadState::Completed,
        DownloadWaitState::Failed => download.state == DownloadState::Failed,
        DownloadWaitState::Canceled => download.state == DownloadState::Canceled,
        DownloadWaitState::Terminal => matches!(
            download.state,
            DownloadState::Completed | DownloadState::Failed | DownloadState::Canceled
        ),
    }
}

fn download_wait_timeout_error(
    target_guid: Option<&str>,
    desired_state: &str,
    timeout: Duration,
    elapsed: Duration,
    runtime: rub_core::model::DownloadRuntimeInfo,
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

fn download_payload(
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

fn download_registry_subject() -> serde_json::Value {
    serde_json::json!({
        "kind": "download_registry",
    })
}

fn download_subject(
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

fn download_registry_result(runtime: &rub_core::model::DownloadRuntimeInfo) -> serde_json::Value {
    serde_json::json!({
        "download_dir": runtime.download_dir,
        "active_downloads": runtime.active_downloads,
        "completed_downloads": runtime.completed_downloads,
        "last_download": runtime.last_download,
    })
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

fn wait_state_label(state: DownloadWaitState) -> &'static str {
    match state {
        DownloadWaitState::Started => "started",
        DownloadWaitState::InProgress => "in_progress",
        DownloadWaitState::Completed => "completed",
        DownloadWaitState::Failed => "failed",
        DownloadWaitState::Canceled => "canceled",
        DownloadWaitState::Terminal => "terminal",
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

#[cfg(test)]
mod tests {
    use super::{
        DownloadWaitState, download_wait_timeout_error, matches_wait_state, parse_wait_state,
    };
    use rub_core::error::ErrorCode;
    use rub_core::model::{DownloadEntry, DownloadMode, DownloadRuntimeInfo, DownloadState};
    use std::time::Duration;

    fn download(state: DownloadState) -> DownloadEntry {
        DownloadEntry {
            guid: "guid-1".to_string(),
            state,
            url: None,
            suggested_filename: None,
            final_path: None,
            mime_hint: None,
            received_bytes: 0,
            total_bytes: None,
            started_at: "2026-03-30T00:00:00Z".to_string(),
            completed_at: None,
            frame_id: None,
            trigger_command_id: None,
        }
    }

    #[test]
    fn wait_state_parser_accepts_terminal_aliases() {
        assert_eq!(
            parse_wait_state(Some("terminal")).expect("terminal should parse"),
            DownloadWaitState::Terminal
        );
        let err = parse_wait_state(Some("mystery")).expect_err("invalid state should error");
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn wait_state_matcher_handles_terminal_states() {
        assert!(matches_wait_state(
            &download(DownloadState::Completed),
            DownloadWaitState::Terminal
        ));
        assert!(matches_wait_state(
            &download(DownloadState::Canceled),
            DownloadWaitState::Terminal
        ));
        assert!(!matches_wait_state(
            &download(DownloadState::InProgress),
            DownloadWaitState::Terminal
        ));
    }

    #[test]
    fn wait_timeout_error_includes_live_download_progress_and_guidance() {
        let error = download_wait_timeout_error(
            Some("guid-1"),
            "completed",
            Duration::from_secs(10),
            Duration::from_secs(10),
            DownloadRuntimeInfo {
                status: rub_core::model::DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                download_dir: Some("/tmp/downloads".to_string()),
                active_downloads: vec![download(DownloadState::InProgress)],
                completed_downloads: Vec::new(),
                last_download: Some(download(DownloadState::InProgress)),
                degraded_reason: None,
            },
            Some(DownloadEntry {
                received_bytes: 512,
                total_bytes: Some(1024),
                ..download(DownloadState::InProgress)
            }),
        );
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::WaitTimeout);
        assert!(envelope.message.contains("current state is 'in_progress'"));
        assert!(envelope.message.contains("512 / 1024 bytes"));
        assert!(envelope.suggestion.contains("rub downloads"));
        assert_eq!(
            envelope.context.as_ref().unwrap()["download"]["guid"],
            "guid-1"
        );
    }
}
