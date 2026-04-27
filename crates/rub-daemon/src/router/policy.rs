mod epoch;
mod handoff;

use std::sync::Arc;

use super::PendingExternalDomCommit;
use crate::session::SessionState;

pub(super) fn command_allowed_during_handoff(command: &str) -> bool {
    handoff::command_allowed_during_handoff(command)
}

pub(super) fn response_dom_epoch(
    command: &str,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    pending_external_dom_commit: PendingExternalDomCommit,
) -> Option<u64> {
    epoch::response_dom_epoch(command, args, state, pending_external_dom_commit)
}

pub(super) fn command_invalidates_cached_snapshots_without_epoch_bump(
    command: &str,
    args: &serde_json::Value,
) -> bool {
    epoch::command_invalidates_cached_snapshots_without_epoch_bump(command, args)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn command_increments_epoch(command: &str) -> bool {
    epoch::command_increments_epoch(command)
}

#[cfg(test)]
mod tests;
