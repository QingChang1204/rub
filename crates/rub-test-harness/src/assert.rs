//! JSON assertion macros for rub test harness.

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
                .is_some_and(|value| value.is_string()),
            "Missing stdout_schema_version"
        );
        assert!(
            json.get("request_id")
                .is_some_and(|value| value.is_string()),
            "Missing request_id"
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
                .is_some_and(|value| value.is_string()),
            "Missing stdout_schema_version"
        );
        assert!(
            json.get("request_id")
                .is_some_and(|value| value.is_string()),
            "Missing request_id"
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

    #[test]
    fn success_macro_accepts_null_error_and_present_data() {
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
            "stdout_schema_version": "2.0",
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
            "stdout_schema_version": "2.0",
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
            "stdout_schema_version": "2.0",
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
    fn error_macro_evaluates_input_once() {
        let calls = AtomicUsize::new(0);
        crate::assert_json_error!(counted_error_json(&calls), "INVALID_INPUT");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
