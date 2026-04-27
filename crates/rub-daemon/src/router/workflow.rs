use std::sync::Arc;

mod args;
mod command_build;
mod execution;
mod fill_atomic;
mod pipe_execution;
mod projection;
mod spec;
mod validate;

use self::args::{FillArgs, PipeArgs, submit_args};
#[cfg(test)]
use self::spec::{parse_pipe_spec, resolve_step_references};
use super::*;
use crate::router::request_args::parse_json_args;
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

pub(crate) fn semantic_replay_args(
    command: &str,
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    match command {
        "fill" | "_trigger_fill" => {
            let parsed: FillArgs = parse_json_args(args, "fill").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("spec".to_string(), parsed.spec.as_value().clone());
            projected.insert("atomic".to_string(), serde_json::json!(parsed.atomic));
            if let Some(snapshot_id) = parsed._snapshot_id {
                projected.insert("snapshot_id".to_string(), serde_json::json!(snapshot_id));
            }
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(submit) = submit_args(&parsed.submit) {
                projected.insert("submit".to_string(), submit);
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        "pipe" | "_trigger_pipe" => {
            let parsed: PipeArgs = parse_json_args(args, "pipe").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("spec".to_string(), parsed.spec.as_value().clone());
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
