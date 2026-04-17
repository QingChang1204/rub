use super::{
    InteractionTraceMode, POST_COMMIT_LOCAL_FAILURE_STATE, format_cli_error,
    format_exec_raw_response, format_post_commit_cli_error, format_response,
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
fn format_response_trace_mode_picks_up_new_interaction_fields_without_hardcoded_list() {
    let response = IpcResponse {
        ipc_protocol_version: "1.0".to_string(),
        command_id: Some("019-request".to_string()),
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "interaction": {
                "semantic_class": "click",
                "element_verified": true,
                "interaction_confirmed": true,
                "confirmation_status": "confirmed",
                "confirmation_kind": "dom_effect",
                "custom_projection": {
                    "kind": "new_surface",
                    "value": 7
                },
                "observed_effects": {
                    "context_changed": true
                }
            }
        })),
        error: None,
        timing: rub_core::model::Timing::default(),
    };

    let output = format_response(
        &response,
        "click",
        "default",
        rub_home(),
        false,
        InteractionTraceMode::Trace,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["interaction_trace"]["observed_effects"]["custom_projection"]["kind"],
        "new_surface"
    );
    assert_eq!(
        json["data"]["interaction_trace"]["observed_effects"]["custom_projection"]["value"],
        7
    );
}

#[test]
fn format_response_attaches_frame_drift_authority_guidance_to_errors() {
    let response = IpcResponse {
        ipc_protocol_version: "1.0".to_string(),
        command_id: Some("019-frame-drift".to_string()),
        request_id: "019-request".to_string(),
        status: ResponseStatus::Error,
        data: None,
        error: Some(
            ErrorEnvelope::new(
                rub_core::error::ErrorCode::StaleSnapshot,
                "snapshot frame drifted",
            )
            .with_context(serde_json::json!({
                "snapshot_id": "snap-1",
                "authority_state": "selected_frame_context_drifted"
            })),
        ),
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "get",
        "default",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(
        json["error"]["context"]["authority_guidance"]["source_signal"],
        "selected_frame_context_drifted"
    );
    assert_eq!(
        json["error"]["context"]["authority_guidance"]["next_command_hints"][0]["command"],
        "rub frames"
    );
}

#[test]
fn format_cli_error_attaches_takeover_continuity_guidance() {
    let output = format_cli_error(
        "takeover",
        "default",
        ErrorEnvelope::new(
            rub_core::error::ErrorCode::BrowserCrashed,
            "No active tab remained after takeover transition",
        )
        .with_context(serde_json::json!({
            "reason": "continuity_no_active_tab"
        })),
        false,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(
        json["error"]["context"]["authority_guidance"]["source_signal"],
        "continuity_no_active_tab"
    );
    assert_eq!(
        json["error"]["context"]["authority_guidance"]["next_command_hints"][0]["command"],
        "rub tabs"
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
fn format_response_rejects_blank_request_id_before_trace_projection() {
    let response = IpcResponse {
        ipc_protocol_version: "1.0".to_string(),
        command_id: None,
        request_id: "   ".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "interaction": {
                "semantic_class": "click"
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "click",
        "default",
        rub_home(),
        false,
        InteractionTraceMode::Verbose,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["code"], "IPC_PROTOCOL_ERROR");
    assert_eq!(json["error"]["context"]["field"], "request_id");
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
fn format_response_rejects_protocol_version_mismatch() {
    let response = IpcResponse {
        ipc_protocol_version: "0.9".to_string(),
        command_id: Some("019-invalid".to_string()),
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({"ok": true})),
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
    assert_eq!(json["error"]["code"], "IPC_VERSION_MISMATCH");
    assert_eq!(
        json["error"]["context"]["reason"],
        "ipc_response_protocol_version_mismatch"
    );
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

#[test]
fn format_response_contract_error_path_sanitizes_blank_stdout_request_id() {
    let response = IpcResponse {
        ipc_protocol_version: "1.0".to_string(),
        command_id: Some("019-invalid".to_string()),
        request_id: "   ".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({"ok": true})),
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
    assert_eq!(json["error"]["context"]["field"], "request_id");
    assert_ne!(json["request_id"], Value::String("   ".to_string()));
    assert!(
        !json["request_id"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty()
    );
}
