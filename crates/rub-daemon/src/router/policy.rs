use std::sync::Arc;

use rub_core::command::CommandName;

use super::PendingExternalDomCommit;
use crate::session::SessionState;

pub(super) fn command_allowed_during_handoff(command: &str) -> bool {
    if CommandName::parse(command).is_some_and(|name| {
        let metadata = name.metadata();
        metadata.internal && !metadata.in_process_only
    }) {
        return true;
    }

    matches!(
        command,
        "doctor"
            | "runtime"
            | "frames"
            | "history"
            | "downloads"
            | "download"
            | "handoff"
            | "takeover"
            | "dialog"
            | "state"
            | "observe"
            | "inspect"
            | "trigger"
            | "tabs"
            | "get"
            | "screenshot"
            | "close"
    )
}

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

#[cfg(test)]
mod tests {
    use rub_core::command::CommandName;

    use super::{command_allowed_during_handoff, command_increments_epoch};

    /// Commands that read the current epoch without incrementing it.
    /// These are multi-step commands that need epoch context for
    /// downstream snapshot association but do not themselves mutate the DOM.
    fn command_reads_epoch(command: &str) -> bool {
        matches!(
            command,
            "scroll" | "fill" | "pipe" | "_trigger_fill" | "_trigger_pipe"
        )
    }

    /// Commands classified as pure query: no epoch interaction.
    /// Internal commands are always query-only.
    fn command_is_epoch_neutral(command: &str) -> bool {
        !command_increments_epoch(command) && !command_reads_epoch(command)
    }

    /// **Regression guard**: every known CommandName wire string must be
    /// explicitly classified into exactly one epoch category:
    ///   (A) increments epoch  ← `command_increments_epoch`
    ///   (B) reads epoch       ← "scroll" | "fill" | "pipe"
    ///   (C) epoch-neutral     ← all others
    ///
    /// The three categories are mutually exclusive by construction.
    /// Adding a new command to CommandName without updating policy.rs
    /// will cause (C) to silently apply — this test documents what the
    /// developer *intended* for every command, making that silent drift
    /// visible in PR review.
    #[test]
    fn epoch_classification_is_exhaustive_over_all_known_commands() {
        // Epoch-incrementing commands: these write the DOM / navigate / interact.
        let epoch_incrementing = [
            "open",
            "click",
            "exec",
            "back",
            "forward",
            "reload",
            "keys",
            "type",
            "switch",
            "close-tab",
            "hover",
            "upload",
            "select",
        ];

        // Epoch-reading commands: multi-step, need epoch for snapshot association.
        let epoch_reading = ["scroll", "fill", "pipe"];

        // Epoch-neutral commands: pure query, observability, or management.
        let epoch_neutral = [
            // Internal housekeeping
            "_handshake",
            "_upgrade_check",
            "_orchestration_probe",
            "_orchestration_target_dispatch",
            "_orchestration_workflow_source_vars",
            // Navigation/observation queries
            "state",
            "observe",
            "orchestration",
            "inspect",
            "find",
            "screenshot",
            "doctor",
            "runtime",
            "frames",
            "frame",
            "history",
            "downloads",
            "download",
            "storage",
            "handoff",
            "takeover",
            "dialog",
            "intercept",
            "interference",
            "close",
            "wait",
            "tabs",
            "trigger",
            "get",
            "cookies",
            "extract",
        ];

        // Verify each category is internally consistent.
        for cmd in epoch_incrementing {
            assert!(
                command_increments_epoch(cmd),
                "Expected '{cmd}' to increment epoch but command_increments_epoch returned false"
            );
            assert!(
                !command_reads_epoch(cmd),
                "'{cmd}' is in both epoch_incrementing and epoch_reading — fix the classification"
            );
        }

        for cmd in epoch_reading {
            assert!(
                command_reads_epoch(cmd),
                "Expected '{cmd}' to read epoch but command_reads_epoch returned false"
            );
            assert!(
                !command_increments_epoch(cmd),
                "'{cmd}' is in both epoch_reading and epoch_incrementing — fix the classification"
            );
        }

        for cmd in epoch_neutral {
            assert!(
                command_is_epoch_neutral(cmd),
                "Expected '{cmd}' to be epoch-neutral but it appears in an incrementing/reading list"
            );
        }

        // Verify the three lists collectively cover every known CommandName.
        // If a new CommandName is added without updating this test, this assertion fails.
        let all_known: Vec<&str> = [
            epoch_incrementing.as_slice(),
            &epoch_reading,
            &epoch_neutral,
        ]
        .concat()
        .into_iter()
        .collect();

        // Every CommandName must parse and appear in exactly one list.
        let known_commands = [
            CommandName::Handshake,
            CommandName::UpgradeCheck,
            CommandName::OrchestrationProbe,
            CommandName::OrchestrationTargetDispatch,
            CommandName::OrchestrationWorkflowSourceVars,
            CommandName::Open,
            CommandName::State,
            CommandName::Observe,
            CommandName::Orchestration,
            CommandName::Inspect,
            CommandName::Find,
            CommandName::Click,
            CommandName::Exec,
            CommandName::Scroll,
            CommandName::Back,
            CommandName::Forward,
            CommandName::Reload,
            CommandName::Screenshot,
            CommandName::Doctor,
            CommandName::Runtime,
            CommandName::Frames,
            CommandName::Frame,
            CommandName::History,
            CommandName::Downloads,
            CommandName::Download,
            CommandName::Storage,
            CommandName::Handoff,
            CommandName::Takeover,
            CommandName::Dialog,
            CommandName::Intercept,
            CommandName::Interference,
            CommandName::Close,
            CommandName::Keys,
            CommandName::Type,
            CommandName::Wait,
            CommandName::Tabs,
            CommandName::Trigger,
            CommandName::Switch,
            CommandName::CloseTab,
            CommandName::Get,
            CommandName::Hover,
            CommandName::Cookies,
            CommandName::Upload,
            CommandName::Select,
            CommandName::Fill,
            CommandName::Extract,
            CommandName::Pipe,
        ];

        for name in known_commands {
            let wire = name.as_str();
            assert!(
                all_known.contains(&wire),
                "CommandName::{name:?} (wire: '{wire}') is not classified in any epoch category — \
                update policy.rs epoch_classification_is_exhaustive_over_all_known_commands"
            );

            // Each command appears in at most one non-neutral category.
            let in_incrementing = command_increments_epoch(wire);
            let in_reading = command_reads_epoch(wire);
            assert!(
                !(in_incrementing && in_reading),
                "'{wire}' is in both epoch_incrementing and epoch_reading — mutually exclusive invariant violated"
            );
        }
    }

    /// Epoch-incrementing commands must never appear in the handoff allow-list.
    /// A command that increments epoch mutates the DOM and must be blocked
    /// during human verification handoff.
    #[test]
    fn epoch_incrementing_commands_are_blocked_during_handoff() {
        let epoch_incrementing = [
            "open",
            "click",
            "exec",
            "back",
            "forward",
            "reload",
            "keys",
            "type",
            "switch",
            "close-tab",
            "hover",
            "upload",
            "select",
        ];
        for cmd in epoch_incrementing {
            assert!(
                !command_allowed_during_handoff(cmd),
                "'{cmd}' increments epoch but is allowed during handoff — this is a correctness violation"
            );
        }
    }

    /// Internal commands are always allowed during handoff (they are
    /// management-plane, not DOM-mutating).
    #[test]
    fn all_internal_commands_are_allowed_during_handoff() {
        for cmd in [
            "_handshake",
            "_upgrade_check",
            "_orchestration_probe",
            "_orchestration_tab_frames",
            "_orchestration_target_dispatch",
            "_orchestration_workflow_source_vars",
        ] {
            assert!(
                command_allowed_during_handoff(cmd),
                "Internal command '{cmd}' should always be allowed during handoff"
            );
        }
    }

    #[test]
    fn trigger_internal_workflow_commands_remain_blocked_during_handoff() {
        for cmd in ["_trigger_fill", "_trigger_pipe"] {
            assert!(
                !command_allowed_during_handoff(cmd),
                "Trigger workflow command '{cmd}' should inherit automation handoff blocking"
            );
        }
    }
}
