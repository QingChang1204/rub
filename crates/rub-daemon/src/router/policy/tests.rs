use rub_core::command::CommandName;

use super::{
    command_allowed_during_handoff, command_increments_epoch,
    command_invalidates_cached_snapshots_without_epoch_bump,
};

/// Commands that read the current epoch without incrementing it.
/// These are multi-step commands that need epoch context for
/// downstream snapshot association but do not themselves mutate the DOM.
fn command_reads_epoch(command: &str) -> bool {
    matches!(command, "scroll" | "fill" | "_trigger_fill")
}

/// Commands classified as pure query: no epoch interaction.
/// Internal commands are always query-only.
fn command_is_epoch_neutral(command: &str) -> bool {
    !command_increments_epoch(command) && !command_reads_epoch(command)
}

/// **Regression guard**: every known CommandName wire string must be
/// explicitly classified into exactly one epoch category:
///   (A) increments epoch  ← `command_increments_epoch`
///   (B) reads epoch       ← "scroll" | "fill" | "_trigger_fill"
///   (C) epoch-neutral     ← all others
///
/// The three categories are mutually exclusive by construction.
/// Adding a new command to CommandName without updating policy.rs
/// will cause (C) to silently apply — this test documents what the
/// developer *intended* for every command, making that silent drift
/// visible in PR review.
#[test]
fn epoch_classification_is_exhaustive_over_all_known_commands() {
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

    let epoch_reading = ["scroll", "fill", "_trigger_fill"];

    let epoch_neutral = [
        "_handshake",
        "_upgrade_check",
        "_blocker_diagnose",
        "_interactability_probe",
        "_fill_validate",
        "_orchestration_probe",
        "_orchestration_tab_frames",
        "_orchestration_target_dispatch",
        "_orchestration_workflow_source_vars",
        "state",
        "pipe",
        "_trigger_pipe",
        "extract",
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
        "secret",
        "wait",
        "tabs",
        "trigger",
        "get",
        "cookies",
    ];

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

    let all_known: Vec<&str> = [
        epoch_incrementing.as_slice(),
        &epoch_reading,
        &epoch_neutral,
    ]
    .concat()
    .into_iter()
    .collect();

    let known_commands = [
        CommandName::Handshake,
        CommandName::UpgradeCheck,
        CommandName::BlockerDiagnose,
        CommandName::InteractabilityProbe,
        CommandName::FillValidate,
        CommandName::OrchestrationProbe,
        CommandName::OrchestrationTabFrames,
        CommandName::OrchestrationTargetDispatch,
        CommandName::OrchestrationWorkflowSourceVars,
        CommandName::TriggerFill,
        CommandName::TriggerPipe,
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
        CommandName::Secret,
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

        let in_incrementing = command_increments_epoch(wire);
        let in_reading = command_reads_epoch(wire);
        assert!(
            !(in_incrementing && in_reading),
            "'{wire}' is in both epoch_incrementing and epoch_reading — mutually exclusive invariant violated"
        );
    }
}

#[test]
fn extract_cache_invalidation_is_scan_args_aware() {
    assert!(
        command_invalidates_cached_snapshots_without_epoch_bump(
            "extract",
            &serde_json::json!({"scan": {"limit": 3}})
        ),
        "extract scan scrolls the viewport and must clear stale snapshot projections"
    );
    assert!(
        !command_invalidates_cached_snapshots_without_epoch_bump("extract", &serde_json::json!({})),
        "ordinary extract is read-only and must not clear snapshot projections"
    );
}

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
