use super::{
    command_metadata, command_supports_post_wait, is_in_process_only_command, is_internal_command,
};

#[test]
fn command_metadata_single_sources_internal_and_post_wait_flags() {
    let handshake = command_metadata("_handshake");
    assert!(handshake.internal);
    assert!(!handshake.supports_post_wait);
    assert!(!handshake.in_process_only);
    assert!(is_internal_command("_handshake"));
    assert!(!is_in_process_only_command("_handshake"));

    let open = command_metadata("open");
    assert!(!open.internal);
    assert!(open.supports_post_wait);
    assert!(!open.in_process_only);
    assert!(command_supports_post_wait("open"));

    let history = command_metadata("history");
    assert!(!history.internal);
    assert!(!history.supports_post_wait);
    assert!(!history.in_process_only);

    let trigger_fill = command_metadata("_trigger_fill");
    assert!(trigger_fill.internal);
    assert!(trigger_fill.supports_post_wait);
    assert!(trigger_fill.in_process_only);
    assert!(is_internal_command("_trigger_fill"));
    assert!(is_in_process_only_command("_trigger_fill"));
}
