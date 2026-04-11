use std::sync::Arc;

use crate::session::SessionState;

use super::super::PendingExternalDomCommit;

pub(super) fn response_dom_epoch(
    command: &str,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    pending_external_dom_commit: PendingExternalDomCommit,
) -> Option<u64> {
    if command_increments_epoch(command) || dialog_action_commits_epoch(command, args) {
        if matches!(pending_external_dom_commit, PendingExternalDomCommit::Clear) {
            state.clear_pending_external_dom_change();
        }
        Some(state.increment_epoch())
    } else if matches!(
        command,
        "scroll" | "fill" | "pipe" | "_trigger_fill" | "_trigger_pipe"
    ) {
        Some(state.current_epoch())
    } else {
        None
    }
}

fn dialog_action_commits_epoch(command: &str, args: &serde_json::Value) -> bool {
    command == "dialog"
        && matches!(
            args.get("sub").and_then(|value| value.as_str()),
            Some("accept" | "dismiss")
        )
}

pub(super) fn command_increments_epoch(command: &str) -> bool {
    matches!(
        command,
        "open"
            | "click"
            | "exec"
            | "back"
            | "forward"
            | "reload"
            | "keys"
            | "type"
            | "switch"
            | "close-tab"
            | "hover"
            | "upload"
            | "select"
    )
}
