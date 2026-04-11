mod execute;
mod metadata;

use std::sync::Arc;

use super::*;
use rub_core::command::CommandMetadata;

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn command_metadata(command: &str) -> CommandMetadata {
    metadata::command_metadata(command)
}

pub(super) fn is_internal_command(command: &str) -> bool {
    metadata::is_internal_command(command)
}

pub(super) fn is_in_process_only_command(command: &str) -> bool {
    metadata::is_in_process_only_command(command)
}

pub(super) fn command_supports_post_wait(command: &str) -> bool {
    metadata::command_supports_post_wait(command)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn dispatch_named_command(
    router: &DaemonRouter,
    command: &str,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    execute::dispatch_named_command(router, command, args, deadline, state).await
}

pub(super) fn execute_named_command_with_fence<'a>(
    router: &'a DaemonRouter,
    command: &'a str,
    args: &'a serde_json::Value,
    deadline: TransactionDeadline,
    state: &'a Arc<SessionState>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<serde_json::Value, RubError>> + Send + 'a>,
> {
    execute::execute_named_command_with_fence(router, command, args, deadline, state)
}

#[cfg(test)]
mod tests;
