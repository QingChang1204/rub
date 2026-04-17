//! JSON assertion macros for rub test harness.

#[doc(hidden)]
pub const EXPECTED_STDOUT_SCHEMA_VERSION: &str =
    rub_core::model::CommandResult::STDOUT_SCHEMA_VERSION;

/// Assert that a JSON value represents a successful command result.
///
/// # Example
/// ```ignore
/// let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
/// assert_json_success!(json);
/// assert_json_success!(json, "open");  // also checks command name
/// ```
#[macro_export]
macro_rules! assert_json_success {
    ($json:expr) => {{
        let json = &$json;
        assert_eq!(
            json["success"],
            true,
            "Expected success=true, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("stdout_schema_version")
                .is_some_and(|value| value == $crate::assert::EXPECTED_STDOUT_SCHEMA_VERSION),
            "Expected stdout_schema_version={}, got: {}",
            $crate::assert::EXPECTED_STDOUT_SCHEMA_VERSION,
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("request_id")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.trim().is_empty()),
            "Expected request_id to be a non-empty string"
        );
        assert!(
            json.get("command_id")
                .is_none_or(|value| value.is_null()
                    || value.as_str().is_some_and(|id| !id.trim().is_empty())),
            "Expected command_id to be null/absent or a non-empty string"
        );
        assert!(
            json.get("command").is_some_and(|value| value.is_string()),
            "Missing command"
        );
        assert!(
            json.get("session").is_some_and(|value| value.is_string()),
            "Missing session"
        );
        assert!(
            json.get("timing").is_some_and(|value| value.is_object()),
            "Missing timing"
        );
        assert!(
            json.get("error").is_none() || json["error"].is_null(),
            "Expected success payload to omit error, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("data").is_some_and(|value| value.is_object()),
            "Expected success payload to include data, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
    }};
    ($json:expr, $command:expr) => {{
        let json = &$json;
        assert_json_success!(json);
        assert_eq!(
            json["command"], $command,
            "Expected command={}, got: {}",
            $command, json["command"]
        );
    }};
}

/// Assert that a JSON value represents a failed command result with a specific error code.
///
/// # Example
/// ```ignore
/// let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
/// assert_json_error!(json, "STALE_SNAPSHOT");
/// ```
#[macro_export]
macro_rules! assert_json_error {
    ($json:expr, $error_code:expr) => {{
        let json = &$json;
        assert_eq!(
            json["success"],
            false,
            "Expected success=false, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("stdout_schema_version")
                .is_some_and(|value| value == $crate::assert::EXPECTED_STDOUT_SCHEMA_VERSION),
            "Expected stdout_schema_version={}, got: {}",
            $crate::assert::EXPECTED_STDOUT_SCHEMA_VERSION,
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("request_id")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.trim().is_empty()),
            "Expected request_id to be a non-empty string"
        );
        assert!(
            json.get("command_id")
                .is_none_or(|value| value.is_null()
                    || value.as_str().is_some_and(|id| !id.trim().is_empty())),
            "Expected command_id to be null/absent or a non-empty string"
        );
        assert!(
            json.get("command").is_some_and(|value| value.is_string()),
            "Missing command"
        );
        assert!(
            json.get("session").is_some_and(|value| value.is_string()),
            "Missing session"
        );
        assert!(
            json.get("timing").is_some_and(|value| value.is_object()),
            "Missing timing"
        );
        assert_eq!(
            json["error"]["code"], $error_code,
            "Expected error code={}, got: {}",
            $error_code, json["error"]["code"]
        );
        assert!(
            json["error"]
                .get("message")
                .is_some_and(|value| value.is_string()),
            "Missing error message"
        );
        assert!(
            json["error"]
                .get("suggestion")
                .is_some_and(|value| value.is_string()),
            "Missing error suggestion"
        );
        assert!(
            json.get("data").is_none() || json["data"].is_null(),
            "Expected error payload to omit data, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
    }};
}

/// Assert that a snapshot contains an expected number of elements.
///
/// # Example
/// ```ignore
/// let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
/// assert_element_count!(json, 5);
/// ```
#[macro_export]
macro_rules! assert_element_count {
    ($json:expr, $expected:expr) => {{
        let json = &$json;
        let elements = json["data"]["result"]["snapshot"]["elements"]
            .as_array()
            .expect("Expected canonical data.result.snapshot.elements array");
        assert_eq!(
            elements.len(),
            $expected,
            "Expected {} elements, got {}",
            $expected,
            elements.len()
        );
    }};
}

/// Assert that a raw stdout surface matches the expected text after trimming
/// trailing newlines written by the CLI process.
#[macro_export]
macro_rules! assert_raw_stdout {
    ($stdout:expr, $expected:expr) => {{
        let stdout = &$stdout;
        let actual = stdout.trim();
        assert_eq!(
            actual, $expected,
            "Expected raw stdout={}, got: {:?}",
            $expected, stdout
        );
    }};
}

/// Assert that a failed command result preserves the daemon commit truth while
/// exposing a CLI-local follow-up failure surface.
#[macro_export]
macro_rules! assert_post_commit_local_failure {
    ($json:expr, $error_code:expr) => {{
        let json = &$json;
        assert_eq!(
            json["success"],
            false,
            "Expected success=false, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("stdout_schema_version")
                .is_some_and(|value| value == $crate::assert::EXPECTED_STDOUT_SCHEMA_VERSION),
            "Expected stdout_schema_version={}, got: {}",
            $crate::assert::EXPECTED_STDOUT_SCHEMA_VERSION,
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("request_id")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.trim().is_empty()),
            "Expected request_id to be a non-empty string"
        );
        assert!(
            json.get("command_id")
                .is_none_or(|value| value.is_null()
                    || value.as_str().is_some_and(|id| !id.trim().is_empty())),
            "Expected command_id to be null/absent or a non-empty string"
        );
        assert!(
            json.get("command").is_some_and(|value| value.is_string()),
            "Missing command"
        );
        assert!(
            json.get("session").is_some_and(|value| value.is_string()),
            "Missing session"
        );
        assert!(
            json.get("timing").is_some_and(|value| value.is_object()),
            "Missing timing"
        );
        assert_eq!(
            json["error"]["code"], $error_code,
            "Expected error code={}, got: {}",
            $error_code, json["error"]["code"]
        );
        assert!(
            json["error"]
                .get("message")
                .is_some_and(|value| value.is_string()),
            "Missing error message"
        );
        assert!(
            json["error"]
                .get("suggestion")
                .is_some_and(|value| value.is_string()),
            "Missing error suggestion"
        );
        assert!(
            json.get("data").is_some_and(|value| value.is_object()),
            "Expected post-commit local failure payload to preserve daemon data, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["commit_state"],
            "daemon_committed_local_followup_failed",
            "Expected explicit post-commit local failure state, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["surface"],
            "cli_post_commit_followup_failure",
            "Expected explicit post-commit follow-up surface, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["truth_level"],
            "operator_projection",
            "Expected operator projection truth label for post-commit follow-up, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["projection_kind"],
            "cli_post_commit_followup_failure",
            "Expected explicit post-commit follow-up projection kind, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["projection_authority"],
            "cli.post_commit_followup",
            "Expected cli.post_commit_followup authority, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["upstream_commit_truth"],
            "daemon_response_committed",
            "Expected committed daemon truth ancestry, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["control_role"],
            "display_only",
            "Expected display_only control role for post-commit follow-up, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["durability"],
            "best_effort",
            "Expected best_effort durability for post-commit follow-up, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["data"]["post_commit_followup_state"]["recovery_contract"],
            "no_public_recovery_contract",
            "Expected no public durable recovery contract, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
    }};
}

/// Assert that a daemon projection/export surface explicitly identifies itself
/// as a bounded post-commit projection derived from committed daemon truth.
#[macro_export]
macro_rules! assert_bounded_projection_surface {
    ($json:expr, $projection_authority:expr, $surface:expr) => {{
        let json = &$json;
        assert_eq!(
            json["projection_state"]["surface"],
            $surface,
            "Expected surface={}, got: {}",
            $surface,
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["projection_state"]["projection_kind"],
            "bounded_post_commit_projection",
            "Expected bounded post-commit projection surface, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["projection_state"]["truth_level"],
            "operator_projection",
            "Expected operator projection truth label, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["projection_state"]["projection_authority"],
            $projection_authority,
            "Expected projection_authority={}, got: {}",
            $projection_authority,
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["projection_state"]["upstream_commit_truth"],
            "daemon_response_committed",
            "Expected committed daemon truth label, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["projection_state"]["control_role"],
            "display_only",
            "Expected display_only control role, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["projection_state"]["durability"],
            "best_effort",
            "Expected best_effort durability label, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json["projection_state"]["lossy"].is_boolean(),
            "Expected boolean lossy flag, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json["projection_state"]["lossy_reasons"].is_array(),
            "Expected lossy_reasons array, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
    }};
}

/// Assert that a protocol error exposes a stable structured reason in its
/// error context rather than collapsing to an opaque string.
#[macro_export]
macro_rules! assert_protocol_error_reason {
    ($json:expr, $error_code:expr, $reason:expr) => {{
        let json = &$json;
        $crate::assert_json_error!(json, $error_code);
        assert_eq!(
            json["error"]["context"]["reason"],
            $reason,
            "Expected protocol error reason={}, got: {}",
            $reason,
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
    }};
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counted_success_json(counter: &AtomicUsize) -> serde_json::Value {
        counter.fetch_add(1, Ordering::SeqCst);
        serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "data": {},
            "error": null,
        })
    }

    fn counted_error_json(counter: &AtomicUsize) -> serde_json::Value {
        counter.fetch_add(1, Ordering::SeqCst);
        serde_json::json!({
            "success": false,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "error": {
                "code": "INVALID_INPUT",
                "message": "bad input",
                "suggestion": "fix it"
            }
        })
    }

    fn counted_raw_stdout(counter: &AtomicUsize) -> String {
        counter.fetch_add(1, Ordering::SeqCst);
        "The Page Title\n".to_string()
    }

    #[test]
    fn success_macro_accepts_null_error_and_present_data() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "data": {},
            "error": null,
        });
        crate::assert_json_success!(json);
    }

    #[test]
    fn success_macro_evaluates_input_once() {
        let calls = AtomicUsize::new(0);
        crate::assert_json_success!(counted_success_json(&calls), "open");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[should_panic]
    fn success_macro_rejects_non_null_error() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "data": {},
            "error": {"code": "INVALID_INPUT"},
        });
        crate::assert_json_success!(json);
    }

    #[test]
    #[should_panic]
    fn error_macro_rejects_non_null_data() {
        let json = serde_json::json!({
            "success": false,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "data": {},
            "error": {
                "code": "INVALID_INPUT",
                "message": "bad input",
                "suggestion": "fix it"
            }
        });
        crate::assert_json_error!(json, "INVALID_INPUT");
    }

    #[test]
    #[should_panic]
    fn success_macro_rejects_missing_command_or_session() {
        let json = serde_json::json!({
            "success": true,
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "timing": {},
            "data": {},
            "error": null,
        });
        crate::assert_json_success!(json);
    }

    #[test]
    #[should_panic]
    fn error_macro_rejects_missing_command() {
        let json = serde_json::json!({
            "success": false,
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "error": {
                "code": "INVALID_INPUT",
                "message": "bad input",
                "suggestion": "fix it"
            }
        });
        crate::assert_json_error!(json, "INVALID_INPUT");
    }

    #[test]
    fn error_macro_evaluates_input_once() {
        let calls = AtomicUsize::new(0);
        crate::assert_json_error!(counted_error_json(&calls), "INVALID_INPUT");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[should_panic]
    fn success_macro_rejects_wrong_stdout_schema_version() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "2.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "data": {},
            "error": null,
        });
        crate::assert_json_success!(json);
    }

    #[test]
    #[should_panic]
    fn error_macro_rejects_wrong_stdout_schema_version() {
        let json = serde_json::json!({
            "success": false,
            "command": "open",
            "stdout_schema_version": "2.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "error": {
                "code": "INVALID_INPUT",
                "message": "bad input",
                "suggestion": "fix it"
            }
        });
        crate::assert_json_error!(json, "INVALID_INPUT");
    }

    #[test]
    #[should_panic]
    fn success_macro_rejects_blank_request_id() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "   ",
            "session": "default",
            "timing": {},
            "data": {},
            "error": null,
        });
        crate::assert_json_success!(json);
    }

    #[test]
    #[should_panic]
    fn error_macro_rejects_blank_command_id() {
        let json = serde_json::json!({
            "success": false,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "command_id": "   ",
            "session": "default",
            "timing": {},
            "error": {
                "code": "INVALID_INPUT",
                "message": "bad input",
                "suggestion": "fix it"
            }
        });
        crate::assert_json_error!(json, "INVALID_INPUT");
    }

    #[test]
    fn raw_stdout_macro_evaluates_input_once_and_trims_newline() {
        let calls = AtomicUsize::new(0);
        crate::assert_raw_stdout!(counted_raw_stdout(&calls), "The Page Title");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn post_commit_local_failure_macro_accepts_typed_commit_state() {
        let json = serde_json::json!({
            "success": false,
            "command": "history",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "command_id": "cmd-1",
            "session": "default",
            "timing": {},
            "data": {
                "commit_state": "daemon_committed_local_followup_failed",
                "post_commit_followup_state": {
                    "surface": "cli_post_commit_followup_failure",
                    "truth_level": "operator_projection",
                    "projection_kind": "cli_post_commit_followup_failure",
                    "projection_authority": "cli.post_commit_followup",
                    "upstream_commit_truth": "daemon_response_committed",
                    "control_role": "display_only",
                    "durability": "best_effort",
                    "recovery_contract": "no_public_recovery_contract"
                },
                "result": { "format": "pipe" }
            },
            "error": {
                "code": "INVALID_INPUT",
                "message": "local export failed after daemon success",
                "suggestion": "fix it"
            }
        });
        crate::assert_post_commit_local_failure!(json, "INVALID_INPUT");
    }

    #[test]
    fn bounded_projection_surface_macro_accepts_projection_labels() {
        let json = serde_json::json!({
            "projection_state": {
                "surface": "workflow_capture_export",
                "truth_level": "operator_projection",
                "projection_kind": "bounded_post_commit_projection",
                "projection_authority": "session.workflow_capture",
                "upstream_commit_truth": "daemon_response_committed",
                "control_role": "display_only",
                "durability": "best_effort",
                "lossy": false,
                "lossy_reasons": []
            }
        });
        crate::assert_bounded_projection_surface!(
            json,
            "session.workflow_capture",
            "workflow_capture_export"
        );
    }

    #[test]
    fn protocol_error_reason_macro_accepts_structured_reason() {
        let json = serde_json::json!({
            "success": false,
            "command": "state",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": {},
            "error": {
                "code": "IPC_PROTOCOL_ERROR",
                "message": "IPC response contract error: bad",
                "suggestion": "fix it",
                "context": {
                    "reason": "invalid_ipc_response_contract"
                }
            }
        });
        crate::assert_protocol_error_reason!(
            json,
            "IPC_PROTOCOL_ERROR",
            "invalid_ipc_response_contract"
        );
    }

    #[test]
    #[should_panic(expected = "Expected command_id to be null/absent or a non-empty string")]
    fn success_macro_rejects_non_string_command_id() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "command_id": 7,
            "session": "default",
            "timing": {},
            "data": { "ok": true }
        });
        crate::assert_json_success!(json);
    }
}
