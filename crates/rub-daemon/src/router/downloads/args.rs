use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{DownloadEntry, DownloadState};

use crate::router::request_args::parse_json_args;

use super::asset_save::DownloadSaveArgs;

#[derive(Debug)]
pub(super) enum DownloadCommand {
    Wait(DownloadWaitArgs),
    Cancel(DownloadCancelArgs),
    Save(DownloadSaveArgs),
}

impl DownloadCommand {
    pub(super) fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
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
pub(super) struct DownloadWaitArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) state: Option<String>,
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DownloadCancelArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DownloadWaitState {
    Started,
    InProgress,
    Completed,
    Failed,
    Canceled,
    Terminal,
}

pub(super) fn parse_wait_state(value: Option<&str>) -> Result<DownloadWaitState, RubError> {
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

pub(super) fn matches_wait_state(download: &DownloadEntry, desired: DownloadWaitState) -> bool {
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

pub(super) fn wait_state_label(state: DownloadWaitState) -> &'static str {
    match state {
        DownloadWaitState::Started => "started",
        DownloadWaitState::InProgress => "in_progress",
        DownloadWaitState::Completed => "completed",
        DownloadWaitState::Failed => "failed",
        DownloadWaitState::Canceled => "canceled",
        DownloadWaitState::Terminal => "terminal",
    }
}
