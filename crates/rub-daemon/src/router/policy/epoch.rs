use std::sync::Arc;

use rub_core::command::{DomEpochPolicy, command_metadata};

use crate::session::SessionState;

use super::super::PendingExternalDomCommit;

pub(super) fn response_dom_epoch(
    command: &str,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    pending_external_dom_commit: PendingExternalDomCommit,
) -> Option<u64> {
    let policy = command_metadata(command).dom_epoch_policy;
    if matches!(policy, DomEpochPolicy::Bump)
        || (matches!(policy, DomEpochPolicy::ArgsDependent)
            && dialog_action_commits_epoch(command, args))
    {
        if matches!(pending_external_dom_commit, PendingExternalDomCommit::Clear) {
            state.clear_pending_external_dom_change();
        }
        Some(state.increment_epoch())
    } else if matches!(policy, DomEpochPolicy::InvalidateSnapshotWithoutBump)
        || (matches!(policy, DomEpochPolicy::ArgsDependent)
            && command_invalidates_cached_snapshots_from_args(command, args))
    {
        Some(state.current_epoch())
    } else {
        None
    }
}

pub(super) fn command_invalidates_cached_snapshots_without_epoch_bump(
    command: &str,
    args: &serde_json::Value,
) -> bool {
    match command_metadata(command).dom_epoch_policy {
        DomEpochPolicy::InvalidateSnapshotWithoutBump => true,
        DomEpochPolicy::ArgsDependent => {
            command_invalidates_cached_snapshots_from_args(command, args)
        }
        DomEpochPolicy::None | DomEpochPolicy::Bump => false,
    }
}

fn command_invalidates_cached_snapshots_from_args(command: &str, args: &serde_json::Value) -> bool {
    extract_scan_scrolls_page(command, args) || find_topmost_scrolls_page(command, args)
}

fn extract_scan_scrolls_page(command: &str, args: &serde_json::Value) -> bool {
    command == "extract" && args.get("scan").is_some_and(|scan| !scan.is_null())
}

fn find_topmost_scrolls_page(command: &str, args: &serde_json::Value) -> bool {
    command == "find" && args.get("topmost").and_then(|value| value.as_bool()) == Some(true)
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
        command_metadata(command).dom_epoch_policy,
        DomEpochPolicy::Bump
    )
}

#[cfg(test)]
mod tests {
    use super::{
        command_invalidates_cached_snapshots_without_epoch_bump, find_topmost_scrolls_page,
    };

    #[test]
    fn find_topmost_is_treated_as_same_epoch_scroll_mutation() {
        let args = serde_json::json!({ "topmost": true });
        assert!(find_topmost_scrolls_page("find", &args));
        assert!(command_invalidates_cached_snapshots_without_epoch_bump(
            "find", &args
        ));
    }

    #[test]
    fn ordinary_find_does_not_invalidate_snapshot_cache() {
        let args = serde_json::json!({ "topmost": false });
        assert!(!find_topmost_scrolls_page("find", &args));
        assert!(!command_invalidates_cached_snapshots_without_epoch_bump(
            "find", &args
        ));
    }
}
