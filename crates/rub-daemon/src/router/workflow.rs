use std::sync::Arc;

mod args;
mod command_build;
mod execution;
mod fill_atomic;
mod pipe_execution;
mod projection;
mod spec;
mod validate;

#[cfg(test)]
use self::args::{FillArgs, PipeArgs, submit_args};
#[cfg(test)]
use self::spec::{parse_pipe_spec, resolve_step_references};
use super::*;
use rub_core::error::RubError;

pub(super) async fn cmd_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_fill(router, args, deadline, state).await
}

pub(super) async fn cmd_fill_validate(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    validate::cmd_fill_validate(router, args, deadline, state).await
}

pub(super) async fn cmd_trigger_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_trigger_fill(router, args, deadline, state).await
}

pub(super) async fn cmd_pipe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_pipe(router, args, deadline, state).await
}

pub(super) async fn cmd_trigger_pipe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_trigger_pipe(router, args, deadline, state).await
}

#[cfg(test)]
mod tests;
