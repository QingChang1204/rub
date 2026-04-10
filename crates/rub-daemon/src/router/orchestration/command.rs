use rub_core::error::{ErrorCode, RubError};

use super::super::request_args::parse_json_args;

#[derive(Debug)]
pub(super) enum OrchestrationCommand {
    Add(OrchestrationAddArgs),
    List,
    Trace(OrchestrationTraceArgs),
    Remove(OrchestrationIdArgs),
    Pause(OrchestrationIdArgs),
    Resume(OrchestrationIdArgs),
    Execute(OrchestrationIdArgs),
    Export(OrchestrationIdArgs),
}

impl OrchestrationCommand {
    pub(super) fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("list")
        {
            "add" => Ok(Self::Add(parse_json_args(args, "orchestration add")?)),
            "list" => Ok(Self::List),
            "trace" => Ok(Self::Trace(parse_json_args(args, "orchestration trace")?)),
            "remove" => Ok(Self::Remove(parse_json_args(args, "orchestration remove")?)),
            "pause" => Ok(Self::Pause(parse_json_args(args, "orchestration pause")?)),
            "resume" => Ok(Self::Resume(parse_json_args(args, "orchestration resume")?)),
            "execute" => Ok(Self::Execute(parse_json_args(
                args,
                "orchestration execute",
            )?)),
            "export" => Ok(Self::Export(parse_json_args(args, "orchestration export")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown orchestration subcommand '{other}'"),
            )),
        }
    }

    pub(super) async fn execute(
        self,
        router: &super::DaemonRouter,
        state: &std::sync::Arc<crate::session::SessionState>,
    ) -> Result<serde_json::Value, RubError> {
        match self {
            Self::Add(args) => super::registry::cmd_orchestration_add(router, args, state).await,
            Self::List => super::registry::cmd_orchestration_list(state).await,
            Self::Trace(args) => super::registry::cmd_orchestration_trace(args, state).await,
            Self::Remove(args) => super::registry::cmd_orchestration_remove(args, state).await,
            Self::Pause(args) => {
                super::execution::update_orchestration_status(
                    args.id,
                    state,
                    rub_core::model::OrchestrationRuleStatus::Paused,
                )
                .await
            }
            Self::Resume(args) => {
                super::execution::update_orchestration_status(
                    args.id,
                    state,
                    rub_core::model::OrchestrationRuleStatus::Armed,
                )
                .await
            }
            Self::Execute(args) => {
                super::execution::cmd_orchestration_execute(router, args.id, state).await
            }
            Self::Export(args) => super::registry::cmd_orchestration_export(args.id, state).await,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrchestrationAddArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) spec: String,
    #[serde(default)]
    pub(super) paused: bool,
    #[serde(default)]
    pub(super) spec_source: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrchestrationTraceArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    #[serde(default = "default_trace_last")]
    pub(super) last: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrchestrationIdArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) id: u32,
}

const fn default_trace_last() -> u64 {
    20
}

#[cfg(test)]
pub(super) fn required_u32_arg(args: &serde_json::Value, name: &str) -> Result<u32, RubError> {
    let value = args
        .get(name)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Missing required argument: '{name}'"),
            )
        })?;
    u32::try_from(value).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Argument '{name}' exceeds maximum supported id {}",
                u32::MAX
            ),
        )
    })
}
