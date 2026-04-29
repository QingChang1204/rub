use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::json_spec::NormalizedJsonSpec;

use crate::session::SessionState;

use super::super::DaemonRouter;
use super::super::request_args::parse_json_args;
use super::mutation::{
    cmd_trigger_add, cmd_trigger_list, cmd_trigger_remove, cmd_trigger_trace, update_trigger_status,
};
use rub_core::model::TriggerStatus;

#[derive(Debug)]
pub(super) enum TriggerCommand {
    Add(TriggerAddArgs),
    List,
    Trace(TriggerTraceArgs),
    Remove(TriggerIdArgs),
    Pause(TriggerIdArgs),
    Resume(TriggerIdArgs),
}

impl TriggerCommand {
    pub(super) fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("list")
        {
            "add" => Ok(Self::Add(parse_json_args(args, "trigger add")?)),
            "list" => Ok(Self::List),
            "trace" => Ok(Self::Trace(parse_json_args(args, "trigger trace")?)),
            "remove" => Ok(Self::Remove(parse_json_args(args, "trigger remove")?)),
            "pause" => Ok(Self::Pause(parse_json_args(args, "trigger pause")?)),
            "resume" => Ok(Self::Resume(parse_json_args(args, "trigger resume")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown trigger subcommand '{other}'"),
            )),
        }
    }

    pub(super) async fn execute(
        self,
        router: &DaemonRouter,
        deadline: crate::router::TransactionDeadline,
        state: &Arc<SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        match self {
            Self::Add(args) => cmd_trigger_add(router, args, state).await,
            Self::List => cmd_trigger_list(router, state).await,
            Self::Trace(args) => cmd_trigger_trace(router, args, state).await,
            Self::Remove(args) => cmd_trigger_remove(router, args.id, deadline, state).await,
            Self::Pause(args) => {
                update_trigger_status(router, args.id, deadline, state, TriggerStatus::Paused).await
            }
            Self::Resume(args) => {
                update_trigger_status(router, args.id, deadline, state, TriggerStatus::Armed).await
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TriggerAddArgs {
    #[serde(rename = "sub")]
    _sub: String,
    pub(super) spec: NormalizedJsonSpec,
    #[serde(default)]
    pub(super) paused: bool,
    #[serde(default)]
    pub(super) spec_source: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TriggerTraceArgs {
    #[serde(rename = "sub")]
    _sub: String,
    #[serde(default = "default_trigger_trace_last")]
    pub(super) last: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TriggerIdArgs {
    #[serde(rename = "sub")]
    _sub: String,
    pub(super) id: u32,
}

const fn default_trigger_trace_last() -> u64 {
    20
}
