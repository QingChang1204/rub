//! JSON output formatter for CLI → stdout.

mod continuity;

use self::continuity::attach_workflow_continuity;
use rub_core::error::ErrorEnvelope;
use rub_core::model::CommandResult;
use rub_ipc::protocol::IpcResponse;
use serde_json::{Map, Value, json};
use std::path::Path;

const POST_COMMIT_LOCAL_FAILURE_STATE: &str = "daemon_committed_local_followup_failed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionTraceMode {
    Compact,
    Verbose,
    Trace,
}

/// Convert an IPC response to a CLI stdout CommandResult.
pub fn format_response(
    response: &IpcResponse,
    command: &str,
    session: &str,
    rub_home: &Path,
    pretty: bool,
    trace_mode: InteractionTraceMode,
) -> String {
    if let Some(envelope) = response.contract_error_envelope() {
        let result = CommandResult {
            success: false,
            command: command.to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: response.request_id.clone(),
            command_id: response.command_id.clone(),
            session: session.to_string(),
            timing: response.timing,
            data: None,
            error: Some(envelope),
        };
        return if pretty {
            serde_json::to_string_pretty(&result)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        } else {
            serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        };
    }

    let mut result = CommandResult {
        success: response.status == rub_ipc::protocol::ResponseStatus::Success,
        command: command.to_string(),
        stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
        request_id: response.request_id.clone(),
        command_id: response.command_id.clone(),
        session: session.to_string(),
        timing: response.timing,
        data: response.data.clone(),
        error: response.error.clone(),
    };
    attach_interaction_trace(&mut result, trace_mode);
    attach_workflow_continuity(&mut result, rub_home);

    if pretty {
        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// Format the explicit raw stdout surface for `exec --raw`.
///
/// This surface is intentionally separate from the standard JSON envelope.
/// Callers must opt into it explicitly instead of routing through
/// `format_response()`.
pub fn format_exec_raw_response(response: &IpcResponse, pretty: bool) -> Option<String> {
    if response.status != rub_ipc::protocol::ResponseStatus::Success {
        return None;
    }
    let result = response.data.as_ref().and_then(|data| data.get("result"))?;
    Some(format_raw_value(result, pretty))
}

fn format_raw_value(value: &Value, pretty: bool) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ if pretty => {
            serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
    }
}

/// Format a CLI-side error (before IPC, e.g. daemon start failure).
pub fn format_cli_error(
    command: &str,
    session: &str,
    envelope: ErrorEnvelope,
    pretty: bool,
) -> String {
    let result = CommandResult::error(command, session, uuid::Uuid::now_v7().to_string(), envelope);

    if pretty {
        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// Format a CLI-side error that happened after the daemon had already committed a response.
pub fn format_post_commit_cli_error(
    response: &IpcResponse,
    command: &str,
    session: &str,
    envelope: ErrorEnvelope,
    pretty: bool,
) -> String {
    let data = response
        .data
        .as_ref()
        .map(annotate_post_commit_local_failure_data);
    let result = CommandResult {
        success: false,
        command: command.to_string(),
        stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
        request_id: response.request_id.clone(),
        command_id: response.command_id.clone(),
        session: session.to_string(),
        timing: response.timing,
        data,
        error: Some(envelope),
    };

    if pretty {
        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

fn annotate_post_commit_local_failure_data(data: &Value) -> Value {
    match data {
        Value::Object(object) => {
            let mut annotated = object.clone();
            annotated.insert(
                "commit_state".to_string(),
                Value::String(POST_COMMIT_LOCAL_FAILURE_STATE.to_string()),
            );
            annotated.insert(
                "post_commit_followup_state".to_string(),
                post_commit_followup_state_json(),
            );
            Value::Object(annotated)
        }
        other => json!({
            "commit_state": POST_COMMIT_LOCAL_FAILURE_STATE,
            "post_commit_followup_state": post_commit_followup_state_json(),
            "daemon_response": other,
        }),
    }
}

fn post_commit_followup_state_json() -> Value {
    json!({
        "surface": "cli_post_commit_followup_failure",
        "truth_level": "operator_projection",
        "projection_kind": "cli_post_commit_followup_failure",
        "projection_authority": "cli.post_commit_followup",
        "upstream_commit_truth": "daemon_response_committed",
        "control_role": "display_only",
        "durability": "best_effort",
        "recovery_contract": "no_public_recovery_contract",
    })
}

/// Format a CLI-side success result when no IPC response exists yet.
pub fn format_cli_success(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: serde_json::Value,
    pretty: bool,
    trace_mode: InteractionTraceMode,
) -> String {
    let mut result =
        CommandResult::success(command, session, uuid::Uuid::now_v7().to_string(), data);
    attach_interaction_trace(&mut result, trace_mode);
    attach_workflow_continuity(&mut result, rub_home);

    if pretty {
        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    } else {
        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

fn attach_interaction_trace(result: &mut CommandResult, trace_mode: InteractionTraceMode) {
    if matches!(trace_mode, InteractionTraceMode::Compact) {
        return;
    }
    let Some(data) = result.data.as_mut() else {
        return;
    };
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let Some(interaction) = object
        .get("interaction")
        .and_then(Value::as_object)
        .cloned()
    else {
        return;
    };

    let trace_id = result
        .command_id
        .clone()
        .unwrap_or_else(|| result.request_id.clone());

    let mut trace = Map::new();
    trace.insert("trace_id".to_string(), Value::String(trace_id));
    trace.insert("command".to_string(), Value::String(result.command.clone()));

    copy_field(&interaction, &mut trace, "semantic_class");
    copy_field(&interaction, &mut trace, "element_verified");
    copy_field(&interaction, &mut trace, "actuation");
    copy_field(&interaction, &mut trace, "interaction_confirmed");
    copy_field(&interaction, &mut trace, "confirmation_status");
    copy_field(&interaction, &mut trace, "confirmation_kind");

    if matches!(trace_mode, InteractionTraceMode::Trace)
        && let Some(effects) = summarize_observed_effects(&interaction)
    {
        trace.insert("observed_effects".to_string(), effects);
    }

    object.insert("interaction_trace".to_string(), Value::Object(trace));
}

fn copy_field(source: &Map<String, Value>, dest: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key) {
        dest.insert(key.to_string(), value.clone());
    }
}

fn summarize_observed_effects(interaction: &Map<String, Value>) -> Option<Value> {
    interaction.get("observed_effects").cloned()
}

#[cfg(test)]
mod tests {
    use super::{
        InteractionTraceMode, POST_COMMIT_LOCAL_FAILURE_STATE, format_exec_raw_response,
        format_post_commit_cli_error, format_response,
    };
    use rub_core::error::ErrorEnvelope;
    use rub_core::model::Timing;
    use rub_ipc::protocol::{IpcResponse, ResponseStatus};
    use serde_json::Value;
    use std::path::Path;

    fn rub_home() -> &'static Path {
        Path::new("/tmp/rub-home")
    }

    #[test]
    fn format_response_trace_mode_attaches_full_interaction_trace() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-trace".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({
                "interaction": {
                    "semantic_class": "select_choice",
                    "element_verified": true,
                    "actuation": "programmatic",
                    "interaction_confirmed": false,
                    "confirmation_status": "degraded",
                    "confirmation_kind": "selection_applied",
                    "runtime_state_delta": {
                        "before": {
                            "state_inspector": {
                                "status": "active",
                                "auth_state": "anonymous",
                                "cookie_count": 0,
                                "local_storage_keys": [],
                                "session_storage_keys": [],
                                "auth_signals": []
                            },
                            "readiness_state": {
                                "status": "active",
                                "route_stability": "stable",
                                "loading_present": false,
                                "skeleton_present": false,
                                "overlay_state": "none",
                                "document_ready_state": "complete",
                                "blocking_signals": []
                            }
                        },
                        "after": {
                            "state_inspector": {
                                "status": "active",
                                "auth_state": "unknown",
                                "cookie_count": 0,
                                "local_storage_keys": ["authToken"],
                                "session_storage_keys": [],
                                "auth_signals": ["local_storage_present", "auth_like_storage_key_present"]
                            },
                            "readiness_state": {
                                "status": "active",
                                "route_stability": "transitioning",
                                "loading_present": true,
                                "skeleton_present": false,
                                "overlay_state": "none",
                                "document_ready_state": "complete",
                                "blocking_signals": ["loading_present", "route_transitioning"]
                            }
                        },
                        "changed": [
                            "state_inspector.auth_state",
                            "state_inspector.local_storage_keys",
                            "state_inspector.auth_signals",
                            "readiness_state.route_stability",
                            "readiness_state.loading_present",
                            "readiness_state.blocking_signals"
                        ]
                    },
                    "runtime_observatory_events": [
                        {
                            "kind": "console_error",
                            "sequence": 7,
                            "event": {
                                "level": "error",
                                "message": "boom",
                                "source": "main"
                            }
                        }
                    ],
                    "interference": {
                        "before": {
                            "mode": "public_web_stable",
                            "status": "inactive",
                            "active_policies": ["safe_recovery", "handoff_escalation"],
                            "recovery_in_progress": false,
                            "handoff_required": false
                        },
                        "after": {
                            "mode": "public_web_stable",
                            "status": "active",
                            "current_interference": {
                                "kind": "interstitial_navigation",
                                "summary": "interstitial-like navigation drift detected",
                                "current_url": "https://example.com/after#vignette",
                                "primary_url": "https://example.com/before"
                            },
                            "last_interference": {
                                "kind": "interstitial_navigation",
                                "summary": "interstitial-like navigation drift detected",
                                "current_url": "https://example.com/after#vignette",
                                "primary_url": "https://example.com/before"
                            },
                            "active_policies": ["safe_recovery", "handoff_escalation"],
                            "recovery_in_progress": false,
                            "handoff_required": false
                        },
                        "changed": [
                            "interference_runtime.status",
                            "interference_runtime.current_interference",
                            "interference_runtime.last_interference"
                        ]
                    },
                    "context_turnover": {
                        "context_changed": true,
                        "context_replaced": false,
                        "after_page": {
                            "url": "https://example.com/after",
                            "title": "After",
                            "context_replaced": false
                        }
                    },
                    "confirmation_details": {
                        "context_changed": true,
                        "expected_value": "CA",
                        "observed": { "selected_value": null },
                        "after_page": {
                            "url": "https://example.com/after",
                            "title": "After",
                            "context_replaced": false
                        }
                    },
                    "observed_effects": {
                        "context_changed": true,
                        "after_page": {
                            "url": "https://example.com/after",
                            "title": "After",
                            "context_replaced": false
                        },
                        "context_turnover": {
                            "context_changed": true,
                            "context_replaced": false,
                            "after_page": {
                                "url": "https://example.com/after",
                                "title": "After",
                                "context_replaced": false
                            }
                        },
                        "runtime_state_delta": {
                            "before": {
                                "state_inspector": {
                                    "status": "active",
                                    "auth_state": "anonymous",
                                    "cookie_count": 0,
                                    "local_storage_keys": [],
                                    "session_storage_keys": [],
                                    "auth_signals": []
                                },
                                "readiness_state": {
                                    "status": "active",
                                    "route_stability": "stable",
                                    "loading_present": false,
                                    "skeleton_present": false,
                                    "overlay_state": "none",
                                    "document_ready_state": "complete",
                                    "blocking_signals": []
                                }
                            },
                            "after": {
                                "state_inspector": {
                                    "status": "active",
                                    "auth_state": "unknown",
                                    "cookie_count": 0,
                                    "local_storage_keys": ["authToken"],
                                    "session_storage_keys": [],
                                    "auth_signals": ["local_storage_present", "auth_like_storage_key_present"]
                                },
                                "readiness_state": {
                                    "status": "active",
                                    "route_stability": "transitioning",
                                    "loading_present": true,
                                    "skeleton_present": false,
                                    "overlay_state": "none",
                                    "document_ready_state": "complete",
                                    "blocking_signals": ["loading_present", "route_transitioning"]
                                }
                            },
                            "changed": [
                                "state_inspector.auth_state",
                                "state_inspector.local_storage_keys",
                                "state_inspector.auth_signals",
                                "readiness_state.route_stability",
                                "readiness_state.loading_present",
                                "readiness_state.blocking_signals"
                            ]
                        },
                        "runtime_observatory_events": [
                            {
                                "kind": "console_error",
                                "sequence": 7,
                                "event": {
                                    "level": "error",
                                    "message": "boom",
                                    "source": "main"
                                }
                            }
                        ],
                        "interference": {
                            "before": {
                                "mode": "public_web_stable",
                                "status": "inactive",
                                "active_policies": ["safe_recovery", "handoff_escalation"],
                                "recovery_in_progress": false,
                                "handoff_required": false
                            },
                            "after": {
                                "mode": "public_web_stable",
                                "status": "active",
                                "current_interference": {
                                    "kind": "interstitial_navigation",
                                    "summary": "interstitial-like navigation drift detected",
                                    "current_url": "https://example.com/after#vignette",
                                    "primary_url": "https://example.com/before"
                                },
                                "last_interference": {
                                    "kind": "interstitial_navigation",
                                    "summary": "interstitial-like navigation drift detected",
                                    "current_url": "https://example.com/after#vignette",
                                    "primary_url": "https://example.com/before"
                                },
                                "active_policies": ["safe_recovery", "handoff_escalation"],
                                "recovery_in_progress": false,
                                "handoff_required": false
                            },
                            "changed": [
                                "interference_runtime.status",
                                "interference_runtime.current_interference",
                                "interference_runtime.last_interference"
                            ]
                        }
                    }
                }
            })),
            error: None,
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "select",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Trace,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            json["data"]["interaction"]["semantic_class"],
            "select_choice"
        );
        let trace = &json["data"]["interaction_trace"];
        assert_eq!(trace["trace_id"], "019-trace");
        assert_eq!(trace["command"], "select");
        assert_eq!(trace["semantic_class"], "select_choice");
        assert_eq!(trace["confirmation_status"], "degraded");
        assert_eq!(trace["observed_effects"]["context_changed"], true);
        assert_eq!(
            trace["observed_effects"]["context_turnover"]["context_changed"],
            true
        );
        assert_eq!(
            trace["observed_effects"]["after_page"]["url"],
            "https://example.com/after"
        );
        assert_eq!(
            trace["observed_effects"]["runtime_state_delta"]["after"]["state_inspector"]["auth_signals"],
            serde_json::json!(["local_storage_present", "auth_like_storage_key_present"])
        );
        assert_eq!(
            trace["observed_effects"]["runtime_observatory_events"][0]["kind"],
            "console_error"
        );
        assert_eq!(
            trace["observed_effects"]["interference"]["after"]["current_interference"]["kind"],
            "interstitial_navigation"
        );
    }

    #[test]
    fn post_commit_cli_error_preserves_daemon_request_correlation() {
        let response = IpcResponse {
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            request_id: "req-42".to_string(),
            command_id: Some("cmd-42".to_string()),
            status: ResponseStatus::Success,
            timing: Timing::default(),
            data: Some(serde_json::json!({"result": {"ok": true}})),
            error: None,
        };

        let formatted = format_post_commit_cli_error(
            &response,
            "history",
            "default",
            ErrorEnvelope::new(
                rub_core::error::ErrorCode::InvalidInput,
                "local export failed after daemon success",
            ),
            false,
        );
        let value: Value = serde_json::from_str(&formatted).expect("valid output JSON");
        assert_eq!(value["request_id"], "req-42");
        assert_eq!(value["command_id"], "cmd-42");
        assert_eq!(value["success"], false);
        assert_eq!(
            value["data"]["commit_state"],
            POST_COMMIT_LOCAL_FAILURE_STATE
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["surface"],
            "cli_post_commit_followup_failure"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["truth_level"],
            "operator_projection"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["projection_kind"],
            "cli_post_commit_followup_failure"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["projection_authority"],
            "cli.post_commit_followup"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["upstream_commit_truth"],
            "daemon_response_committed"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["control_role"],
            "display_only"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["durability"],
            "best_effort"
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["recovery_contract"],
            "no_public_recovery_contract"
        );
        assert_eq!(value["data"]["result"]["ok"], true);
        assert_eq!(
            value["error"]["message"],
            "local export failed after daemon success"
        );
    }

    #[test]
    fn post_commit_cli_error_wraps_non_object_daemon_payload_with_commit_state() {
        let response = IpcResponse {
            ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
            request_id: "req-99".to_string(),
            command_id: Some("cmd-99".to_string()),
            status: ResponseStatus::Success,
            timing: Timing::default(),
            data: Some(serde_json::json!("done")),
            error: None,
        };

        let formatted = format_post_commit_cli_error(
            &response,
            "history",
            "default",
            ErrorEnvelope::new(
                rub_core::error::ErrorCode::InvalidInput,
                "local export failed after daemon success",
            ),
            false,
        );
        let value: Value = serde_json::from_str(&formatted).expect("valid output JSON");

        assert_eq!(
            value["data"]["commit_state"],
            POST_COMMIT_LOCAL_FAILURE_STATE
        );
        assert_eq!(
            value["data"]["post_commit_followup_state"]["truth_level"],
            "operator_projection"
        );
        assert_eq!(value["data"]["daemon_response"], "done");
    }

    #[test]
    fn format_exec_raw_response_returns_explicit_raw_surface() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-raw".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({
                "result": "The Page Title"
            })),
            error: None,
            timing: Timing {
                queue_ms: 0,
                exec_ms: 5,
                total_ms: 5,
            },
        };

        let output = format_response(
            &response,
            "exec",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Compact,
        );
        let value: Value = serde_json::from_str(&output).expect("valid JSON output");
        assert_eq!(value["success"], true);
        let raw = format_exec_raw_response(&response, false).expect("raw output should exist");
        assert_eq!(raw, "The Page Title");
    }

    #[test]
    fn format_exec_raw_response_requires_success_with_result_payload() {
        let error_response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-raw".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Error,
            data: None,
            error: Some(ErrorEnvelope::new(
                rub_core::error::ErrorCode::InvalidInput,
                "boom",
            )),
            timing: Timing {
                queue_ms: 0,
                exec_ms: 5,
                total_ms: 5,
            },
        };
        assert!(format_exec_raw_response(&error_response, false).is_none());

        let missing_result = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-raw".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({ "ok": true })),
            error: None,
            timing: Timing {
                queue_ms: 0,
                exec_ms: 5,
                total_ms: 5,
            },
        };
        assert!(format_exec_raw_response(&missing_result, false).is_none());
    }

    #[test]
    fn format_response_keeps_exec_success_in_json_envelope_by_default() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-raw".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({
                "result": "The Page Title"
            })),
            error: None,
            timing: Timing {
                queue_ms: 0,
                exec_ms: 5,
                total_ms: 5,
            },
        };

        let output = format_response(
            &response,
            "exec",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Compact,
        );
        let value: Value = serde_json::from_str(&output).expect("valid JSON output");
        assert_eq!(value["success"], true);
        assert_eq!(value["data"]["result"], "The Page Title");
    }

    #[test]
    fn format_response_compact_mode_omits_interaction_trace() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-request".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({
                "interaction": {
                    "semantic_class": "hover",
                    "element_verified": true,
                    "observed_effects": {
                        "context_changed": true
                    }
                }
            })),
            error: None,
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "hover",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Compact,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert!(json["data"].get("interaction_trace").is_none(), "{json}");
    }

    #[test]
    fn format_response_verbose_mode_attaches_summary_without_observed_effects() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: None,
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({
                "interaction": {
                    "semantic_class": "hover",
                    "element_verified": true,
                    "interaction_confirmed": true,
                    "confirmation_status": "confirmed",
                    "confirmation_kind": "focus_change",
                    "observed_effects": {
                        "context_changed": true
                    }
                }
            })),
            error: None,
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "hover",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Verbose,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["data"]["interaction_trace"]["trace_id"], "019-request");
        assert_eq!(json["data"]["interaction_trace"]["semantic_class"], "hover");
        assert!(
            json["data"]["interaction_trace"]
                .get("observed_effects")
                .is_none(),
            "{json}"
        );
    }

    #[test]
    fn format_response_verbose_uses_request_id_when_command_id_missing() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: None,
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({
                "interaction": {
                    "semantic_class": "hover",
                    "element_verified": true
                }
            })),
            error: None,
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "hover",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Verbose,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["data"]["interaction_trace"]["trace_id"], "019-request");
    }

    #[test]
    fn format_response_rejects_success_envelope_with_error_payload() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-invalid".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({"ok": true})),
            error: Some(ErrorEnvelope::new(
                rub_core::error::ErrorCode::InvalidInput,
                "should not be present on success",
            )),
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "doctor",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Compact,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["code"], "IPC_PROTOCOL_ERROR");
        assert!(json["data"].is_null(), "{json}");
    }

    #[test]
    fn format_response_rejects_success_envelope_without_data() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-invalid".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Success,
            data: None,
            error: None,
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "doctor",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Compact,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["code"], "IPC_PROTOCOL_ERROR");
        assert!(json["data"].is_null(), "{json}");
    }

    #[test]
    fn format_response_rejects_error_envelope_with_success_data() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some("019-invalid".to_string()),
            request_id: "019-request".to_string(),
            status: ResponseStatus::Error,
            data: Some(serde_json::json!({"ok": true})),
            error: Some(ErrorEnvelope::new(
                rub_core::error::ErrorCode::InvalidInput,
                "invalid",
            )),
            timing: Timing::default(),
        };

        let output = format_response(
            &response,
            "doctor",
            "default",
            rub_home(),
            false,
            InteractionTraceMode::Compact,
        );
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["code"], "IPC_PROTOCOL_ERROR");
        assert!(json["data"].is_null(), "{json}");
    }
}
