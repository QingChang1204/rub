use rub_core::command::CommandName;

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
