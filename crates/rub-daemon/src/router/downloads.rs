use std::sync::Arc;
use std::time::{Duration, Instant};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{DownloadRuntimeStatus, DownloadState};

use crate::session::SessionState;

use super::DaemonRouter;

mod args;
mod asset_save;
mod projection;

use self::args::{
    DownloadCancelArgs, DownloadCommand, DownloadWaitArgs, matches_wait_state, parse_wait_state,
    wait_state_label,
};
use self::projection::{
    download_payload, download_registry_result, download_registry_subject, download_subject,
    download_wait_timeout_error,
};

pub(super) async fn cmd_downloads(
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let runtime = state.download_runtime().await;
    let mut result = download_registry_result(&runtime);
    annotate_download_runtime_path_states(&mut result);
    let mut runtime_projection = serde_json::to_value(runtime).map_err(RubError::from)?;
    annotate_download_runtime_path_states(&mut runtime_projection);
    Ok(download_payload(
        download_registry_subject(),
        result,
        runtime_projection,
    ))
}

pub(super) fn annotate_download_runtime_path_states(runtime: &mut serde_json::Value) {
    projection::annotate_download_runtime_path_states(runtime);
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
            let mut runtime_projection = serde_json::to_value(runtime).map_err(RubError::from)?;
            annotate_download_runtime_path_states(&mut runtime_projection);
            return Ok(download_payload(
                download_subject(Some(guid), "wait", Some(wait_state_label(desired_state))),
                serde_json::json!({
                    "download": download,
                    "matched": true,
                }),
                runtime_projection,
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
            let mut runtime_projection = serde_json::to_value(runtime).map_err(RubError::from)?;
            annotate_download_runtime_path_states(&mut runtime_projection);
            return Ok(download_payload(
                download_subject(Some(guid.as_str()), "cancel", None),
                serde_json::json!({
                    "download": download,
                }),
                runtime_projection,
            ));
        }
        if Instant::now() >= deadline {
            let mut context = serde_json::json!({
                "kind": "download",
                "id": guid,
                "state": "canceled",
                "timeout_ms": 2_000u64,
                "download_runtime": runtime,
                "download": state.download_entry(&guid).await,
            });
            if let Some(download_runtime) = context.get_mut("download_runtime") {
                annotate_download_runtime_path_states(download_runtime);
            }
            return Err(RubError::domain_with_context(
                ErrorCode::WaitTimeout,
                format!("Timed out waiting for download '{guid}' to reach canceled state"),
                context,
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::args::DownloadWaitState;
    use super::projection::download_wait_timeout_error;
    use super::{annotate_download_runtime_path_states, matches_wait_state, parse_wait_state};
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
        assert_eq!(
            envelope.context.as_ref().unwrap()["download_runtime"]["download_dir_state"]["path_kind"],
            "managed_download_directory"
        );
    }

    #[test]
    fn annotate_download_runtime_path_states_marks_managed_directory_reference() {
        let mut runtime = serde_json::json!({
            "download_dir": "/tmp/downloads",
            "active_downloads": [],
            "completed_downloads": [],
            "last_download": null,
        });

        annotate_download_runtime_path_states(&mut runtime);

        assert_eq!(
            runtime["download_dir_state"]["truth_level"],
            "operator_path_reference"
        );
        assert_eq!(
            runtime["download_dir_state"]["path_authority"],
            "session.download_runtime.download_dir"
        );
        assert_eq!(
            runtime["download_dir_state"]["upstream_truth"],
            "session_download_runtime"
        );
        assert_eq!(
            runtime["download_dir_state"]["path_kind"],
            "managed_download_directory"
        );
    }
}
