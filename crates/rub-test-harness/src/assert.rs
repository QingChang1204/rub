//! JSON assertion macros for rub test harness.

use rub_core::model::CommandResult;

#[doc(hidden)]
pub const EXPECTED_STDOUT_SCHEMA_VERSION: &str =
    rub_core::model::CommandResult::STDOUT_SCHEMA_VERSION;

#[doc(hidden)]
pub fn stdout_command_allows_missing_command_id(command: &str) -> bool {
    rub_core::command::allows_missing_request_command_id(command)
}

#[doc(hidden)]
pub fn stdout_result_is_contract_fallback(json: &serde_json::Value) -> bool {
    json.get("error")
        .and_then(|value| value.get("context"))
        .and_then(|value| value.as_object())
        .is_some_and(|context| {
            context
                .get("stdout_contract_fallback")
                .and_then(|value| value.as_bool())
                == Some(true)
                && context
                    .get("projection_kind")
                    .and_then(|value| value.as_str())
                    == Some("cli_stdout_contract_fallback")
        })
}

#[doc(hidden)]
pub fn checked_command_result(json: &serde_json::Value) -> CommandResult {
    let result: CommandResult = serde_json::from_value(json.clone()).unwrap_or_else(|error| {
        panic!(
            "Failed to deserialize standard JSON envelope into CommandResult: {error}\njson: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        )
    });
    result.validate_contract().unwrap_or_else(|error| {
        panic!(
            "Standard JSON envelope contract violation: {}\njson: {}",
            serde_json::to_string_pretty(&error).unwrap_or_default(),
            serde_json::to_string_pretty(json).unwrap_or_default()
        )
    });
    result
}

#[doc(hidden)]
pub fn assert_stdout_command_id_contract(json: &serde_json::Value) {
    let _ = checked_command_result(json);
}

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
        let _command_result = $crate::assert::checked_command_result(json);
        assert_eq!(
            json["success"],
            true,
            "Expected success=true, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
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
        let _command_result = $crate::assert::checked_command_result(json);
        assert_eq!(
            json["success"],
            false,
            "Expected success=false, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
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
        if let Some(context) = json["error"].get("context") {
            assert!(
                context.is_object() || context.is_null(),
                "Expected error context to be an object when present, got: {}",
                serde_json::to_string_pretty(json).unwrap_or_default()
            );
        }
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

/// Assert that a CLI-local failure after daemon commit fails closed while
/// preserving the committed daemon response as error-context recovery truth.
#[macro_export]
macro_rules! assert_post_commit_local_failure {
    ($json:expr, $error_code:expr) => {{
        let json = &$json;
        let _command_result = $crate::assert::checked_command_result(json);
        assert_eq!(
            json["success"],
            false,
            "Expected success=false for post-commit local failure, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("data").is_none() || json["data"].is_null(),
            "Expected no success data for post-commit local failure, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json.get("error").is_some_and(|value| value.is_object()),
            "Expected top-level error for post-commit local failure, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert_eq!(
            json["error"]["code"], $error_code,
            "Expected post-commit error code={}, got: {}",
            $error_code, json["error"]["code"]
        );
        assert!(
            json["error"]
                .get("message")
                .is_some_and(|value| value.is_string()),
            "Missing post-commit error message"
        );
        assert!(
            json["error"]
                .get("suggestion")
                .is_some_and(|value| value.is_string()),
            "Missing post-commit error suggestion"
        );
        assert_eq!(
            json["error"]["context"]["daemon_request_committed"],
            true,
            "Expected committed daemon truth marker, got: {}",
            serde_json::to_string_pretty(json).unwrap_or_default()
        );
        assert!(
            json["error"]["context"]
                .get("reason")
                .is_some_and(|value| value.is_string()),
            "Missing stable post-commit local failure reason"
        );
        assert!(
            json["error"]["context"]
                .get("committed_response_projection")
                .is_some_and(|value| !value.is_null()),
            "Missing non-null committed daemon response projection"
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
        let lossy = json["projection_state"]["lossy"]
            .as_bool()
            .expect("lossy must be a boolean after assertion");
        let lossy_reasons = json["projection_state"]["lossy_reasons"]
            .as_array()
            .expect("lossy_reasons must be an array after assertion");
        for reason in lossy_reasons {
            assert!(
                reason.as_str().is_some_and(|value| !value.is_empty()),
                "Expected every lossy reason to be a non-empty string, got: {}",
                serde_json::to_string_pretty(json).unwrap_or_default()
            );
        }
        assert_eq!(
            lossy,
            !lossy_reasons.is_empty(),
            "Expected lossy flag to match lossy_reasons emptiness, got: {}",
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
            "error": {
                "code": "INVALID_INPUT",
                "message": "bad input",
                "suggestion": "fix it"
            }
        })
    }

    fn timing_json() -> serde_json::Value {
        serde_json::json!({
            "queue_ms": 0,
            "exec_ms": 0,
            "total_ms": 0
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
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
            "timing": timing_json(),
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
            "timing": timing_json(),
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
            "timing": timing_json(),
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
            "timing": timing_json(),
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
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
            "timing": timing_json(),
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
            "timing": timing_json(),
            "data": null,
            "error": {
                "code": "INVALID_INPUT",
                "message": "local export failed after daemon success",
                "suggestion": "fix it",
                "context": {
                    "reason": "post_commit_history_export_failed",
                    "daemon_request_committed": true,
                    "committed_response_projection": {
                        "result": { "format": "pipe" }
                    }
                }
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
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
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
    #[should_panic(
        expected = "stdout result command_id must be a non-empty string for non-compat command open"
    )]
    fn success_macro_rejects_missing_command_id_for_non_compat_command() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": timing_json(),
            "data": { "ok": true }
        });
        crate::assert_json_success!(json);
    }

    #[test]
    fn success_macro_allows_missing_command_id_for_compat_control_command() {
        let json = serde_json::json!({
            "success": true,
            "command": "_handshake",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": timing_json(),
            "data": { "daemon_session_id": "sess-default" },
            "error": null,
        });
        crate::assert_json_success!(json);
    }

    #[test]
    fn stdout_command_id_allowlist_matches_retained_compat_commands() {
        for command in ["_handshake", "_upgrade_check", "_blocker_diagnose"] {
            assert!(
                super::stdout_command_allows_missing_command_id(command),
                "{command} should remain in the shared compat allowlist"
            );
        }
        for command in ["open", "doctor", "_orchestration_probe"] {
            assert!(
                !super::stdout_command_allows_missing_command_id(command),
                "{command} should not be treated as a compat allowlist command"
            );
        }
    }

    #[test]
    fn error_macro_allows_missing_command_id_for_stdout_contract_fallback() {
        let json = serde_json::json!({
            "success": false,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": timing_json(),
            "error": {
                "code": "IPC_PROTOCOL_ERROR",
                "message": "IPC response contract error: bad",
                "suggestion": "fix it",
                "context": {
                    "reason": "invalid_stdout_result_contract",
                    "stdout_contract_fallback": true,
                    "projection_kind": "cli_stdout_contract_fallback"
                }
            }
        });
        crate::assert_json_error!(json, "IPC_PROTOCOL_ERROR");
    }

    #[test]
    fn success_macro_allows_missing_or_null_command_id_for_all_compat_commands() {
        for command in ["_handshake", "_upgrade_check", "_blocker_diagnose"] {
            let json_missing = serde_json::json!({
                "success": true,
                "command": command,
                "stdout_schema_version": "3.0",
                "request_id": "req-1",
                "session": "default",
                "timing": timing_json(),
                "data": { "ok": true },
                "error": null,
            });
            crate::assert_json_success!(json_missing, command);

            let json_null = serde_json::json!({
                "success": true,
                "command": command,
                "stdout_schema_version": "3.0",
                "request_id": "req-1",
                "command_id": null,
                "session": "default",
                "timing": timing_json(),
                "data": { "ok": true },
                "error": null,
            });
            crate::assert_json_success!(json_null, command);
        }
    }

    #[test]
    fn error_macro_allows_missing_or_null_command_id_for_all_compat_commands() {
        for command in ["_handshake", "_upgrade_check", "_blocker_diagnose"] {
            let json_missing = serde_json::json!({
                "success": false,
                "command": command,
                "stdout_schema_version": "3.0",
                "request_id": "req-1",
                "session": "default",
                "timing": timing_json(),
                "error": {
                    "code": "IPC_PROTOCOL_ERROR",
                    "message": "compat control-plane failure",
                    "suggestion": "fix it"
                }
            });
            crate::assert_json_error!(json_missing, "IPC_PROTOCOL_ERROR");

            let json_null = serde_json::json!({
                "success": false,
                "command": command,
                "stdout_schema_version": "3.0",
                "request_id": "req-1",
                "command_id": null,
                "session": "default",
                "timing": timing_json(),
                "error": {
                    "code": "IPC_PROTOCOL_ERROR",
                    "message": "compat control-plane failure",
                    "suggestion": "fix it"
                }
            });
            crate::assert_json_error!(json_null, "IPC_PROTOCOL_ERROR");
        }
    }

    #[test]
    fn checked_command_result_accepts_valid_standard_json_envelope() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "command_id": "cmd-1",
            "session": "default",
            "timing": timing_json(),
            "data": { "result": { "ok": true } },
            "error": null,
        });

        let result = super::checked_command_result(&json);
        assert!(result.success);
        assert_eq!(result.command, "open");
        assert_eq!(result.command_id.as_deref(), Some("cmd-1"));
    }

    #[test]
    #[should_panic(expected = "Standard JSON envelope contract violation")]
    fn checked_command_result_rejects_contract_invalid_json_even_when_shape_deserializes() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "session": "default",
            "timing": timing_json(),
            "data": { "result": { "ok": true } },
            "error": null,
        });

        let _ = super::checked_command_result(&json);
    }

    #[test]
    #[should_panic(expected = "Failed to deserialize standard JSON envelope into CommandResult")]
    fn checked_command_result_rejects_non_command_result_json_shape() {
        let json = serde_json::json!({
            "result": { "ok": true }
        });

        let _ = super::checked_command_result(&json);
    }

    #[test]
    #[should_panic(expected = "Failed to deserialize standard JSON envelope into CommandResult")]
    fn success_macro_rejects_non_string_command_id() {
        let json = serde_json::json!({
            "success": true,
            "command": "open",
            "stdout_schema_version": "3.0",
            "request_id": "req-1",
            "command_id": 7,
            "session": "default",
            "timing": timing_json(),
            "data": { "ok": true }
        });
        crate::assert_json_success!(json);
    }
}
