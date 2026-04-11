use rub_core::command::{CommandMetadata, command_metadata as shared_command_metadata};

pub(super) fn command_metadata(command: &str) -> CommandMetadata {
    shared_command_metadata(command)
}

pub(super) fn is_internal_command(command: &str) -> bool {
    command_metadata(command).internal
}

pub(super) fn is_in_process_only_command(command: &str) -> bool {
    command_metadata(command).in_process_only
}

pub(super) fn command_supports_post_wait(command: &str) -> bool {
    command_metadata(command).supports_post_wait
}
