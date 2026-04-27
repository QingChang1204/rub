use super::super::{InteractionTraceMode, format_response};
use rub_core::model::Timing;
use rub_ipc::protocol::{IpcResponse, ResponseStatus};
use serde_json::Value;
use std::path::Path;

fn rub_home() -> &'static Path {
    Path::new("/tmp/rub-home")
}

#[test]
fn format_response_adds_same_runtime_continuity_for_context_transition() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-wait".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "wait" },
            "result": {
                "matched": true,
                "outcome_summary": {
                    "class": "confirmed_context_transition",
                    "authoritative": true,
                    "summary": "context changed"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "wait",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["recommended_runtime"]["kind"],
        "current_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["current_runtime"]["rub_home"],
        "/tmp/rub-home"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["current_runtime"]["session"],
        "forum"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "active_execution_runtime"
    );
}

#[test]
fn format_response_guides_content_lane_after_find_content() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-find".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "find_query",
                "surface": "content",
            },
            "result": {
                "match_count": 2,
                "returned_count": 2,
                "truncated": false,
                "matches": [
                    {
                        "tag_name": "h2",
                        "text": "External links",
                        "role": "heading",
                        "label": "External links"
                    }
                ]
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "find",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "find_content_anchor"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["surface"],
        "content"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub get text ..."
    );
}

#[test]
fn format_response_guides_interaction_lane_after_find_interactive_candidates() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-find".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "find_query",
                "surface": "interactive_snapshot",
            },
            "result": {
                "snapshot_id": "snap-1",
                "match_count": 1,
                "returned_count": 1,
                "truncated": false,
                "matches": [
                    {
                        "index": 0,
                        "text": "Save",
                        "role": "button",
                        "label": "Save",
                    }
                ]
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "find",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "find_interactive_candidates"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["surface"],
        "interactive_snapshot"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub find --content ..."
    );
}

#[test]
fn format_response_adds_fresh_home_continuity_for_provider_gate() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-blockers".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "blocker_explain" },
            "result": {
                "diagnosis": {
                    "class": "provider_gate",
                    "authoritative": true,
                    "summary": "provider gate",
                    "next_safe_actions": ["rub handoff start"],
                    "workflow_guidance": {
                        "continuation_kind": "fresh_rub_home",
                        "summary": "continue alternate-provider work in a fresh runtime",
                        "runtime_roles": {
                            "current_runtime": {
                                "role": "gated_recovery_runtime",
                                "summary": "keep this runtime for inspection"
                            },
                            "recommended_runtime": {
                                "role": "alternate_provider_runtime",
                                "summary": "use the fresh runtime for alternate-provider work"
                            }
                        },
                        "recommended_runtime": {
                            "kind": "fresh_rub_home",
                            "rub_home_hint": "<fresh RUB_HOME>",
                            "session": "default",
                            "reason": "isolated_runtime_recommended"
                        },
                        "next_command_hints": [
                            {
                                "command": "rub handoff start",
                                "reason": "pause automation here and move the gated page into manual recovery"
                            }
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
        "explain",
        "email",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "fresh_rub_home"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["recommended_runtime"]["kind"],
        "fresh_rub_home"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["recommended_runtime"]["rub_home_hint"],
        "<fresh RUB_HOME>"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "gated_recovery_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["recommended_runtime"]["role"],
        "alternate_provider_runtime"
    );
}

#[test]
fn format_response_uses_top_level_doctor_for_degraded_runtime_guidance() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-blockers".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "blocker_explain" },
            "result": {
                "diagnosis": {
                    "class": "degraded_runtime",
                    "authoritative": false,
                    "summary": "runtime degraded",
                    "workflow_guidance": {
                        "continuation_kind": "same_runtime",
                        "summary": "inspect degraded surfaces in the current runtime",
                        "runtime_roles": {
                            "current_runtime": {
                                "role": "observation_runtime",
                                "summary": "inspect here"
                            },
                            "recommended_runtime": {
                                "role": "observation_runtime",
                                "summary": "stay here"
                            }
                        },
                        "recommended_runtime": {
                            "kind": "current_runtime",
                            "reason": "same_runtime_authoritative_followup"
                        },
                        "next_command_hints": [
                            {
                                "command": "rub doctor",
                                "reason": "inspect degraded runtime surfaces in the same session"
                            }
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
        "explain",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub doctor"
    );
}

#[test]
fn format_response_keeps_active_handoff_in_same_runtime() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-blockers".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "blocker_explain" },
            "result": {
                "diagnosis": {
                    "class": "provider_gate",
                    "primary_reason": "handoff_active",
                    "authoritative": true,
                    "summary": "handoff active",
                    "next_safe_actions": ["rub handoff complete"],
                    "workflow_guidance": {
                        "continuation_kind": "same_runtime",
                        "signal": "handoff_active",
                        "summary": "finish recovery here",
                        "recommended_runtime": {
                            "kind": "current_runtime",
                            "reason": "same_runtime_authoritative_followup"
                        },
                        "next_command_hints": [
                            {
                                "command": "rub handoff status",
                                "reason": "inspect current handoff state"
                            },
                            {
                                "command": "rub handoff complete",
                                "reason": "resume automation after manual verification"
                            }
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
        "explain",
        "email",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["recommended_runtime"]["kind"],
        "current_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "handoff_active"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "manual_recovery_runtime"
    );
}

#[test]
fn format_response_prefers_blocker_guidance_when_present() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-blockers".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "blocker_explain" },
            "result": {
                "diagnosis": {
                    "class": "overlay_blocker",
                    "authoritative": true,
                    "summary": "overlay blocker",
                    "next_safe_actions": ["dismiss overlay"],
                    "workflow_guidance": {
                        "continuation_kind": "same_runtime",
                        "signal": "overlay_blocker",
                        "summary": "stay in the same runtime",
                        "recommended_runtime": {
                            "kind": "current_runtime",
                            "reason": "same_runtime_authoritative_followup"
                        },
                        "next_command_hints": [
                            {
                                "command": "rub explain interactability ...",
                                "reason": "confirm which target is blocked"
                            }
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
        "explain",
        "email",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "overlay_blocker"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub explain interactability ..."
    );
}

#[test]
fn format_response_adds_same_runtime_continuity_for_new_item_observed() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "collection_extract" },
            "result": {
                "matched_item": { "subject": "Confirm your new account" },
                "outcome_summary": {
                    "class": "confirmed_new_item_observed",
                    "authoritative": true,
                    "summary": "new row observed"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "email",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "confirmed_new_item_observed"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "observation_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub click --target-text \"Confirm your new account\""
    );
}

#[test]
fn format_response_prefers_open_hint_when_matched_item_contains_url() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "collection_extract" },
            "result": {
                "matched_item": {
                    "subject": "Confirm your new account",
                    "activation_url": "https://forum.example/activate?token=abc"
                },
                "outcome_summary": {
                    "class": "confirmed_new_item_observed",
                    "authoritative": true,
                    "summary": "new row observed"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "email",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub open \"https://forum.example/activate?token=abc\""
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "observation_runtime"
    );
}

#[test]
fn format_response_adds_same_runtime_continuity_for_interactable_target() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-wait".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "wait_condition" },
            "result": {
                "matched": true,
                "outcome_summary": {
                    "class": "confirmed_interactable_target",
                    "authoritative": true,
                    "summary": "target became interactable"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "wait",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "confirmed_interactable_target"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "active_execution_runtime"
    );
}

#[test]
fn format_response_adds_same_runtime_continuity_for_confirmed_target_description() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-wait-description".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "wait_condition" },
            "result": {
                "matched": true,
                "outcome_summary": {
                    "class": "confirmed_target_description",
                    "authoritative": true,
                    "summary": "target description matched"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "wait",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "confirmed_target_description"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "active_execution_runtime"
    );
}

#[test]
fn format_response_adds_observation_guidance_for_confirmed_follow_up_activity() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-click".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "element" },
            "result": {
                "gesture": "single",
                "outcome_summary": {
                    "class": "confirmed_follow_up_activity",
                    "authoritative": true,
                    "summary": "follow-up activity observed",
                    "activity": {
                        "surface": "network_requests",
                        "terminal_request_count": 2,
                        "last_request": {
                            "request_id": "req-9",
                            "method": "POST",
                            "url": "https://example.test/api/signup",
                            "status": 202,
                            "lifecycle": "completed",
                            "resource_type": "XHR"
                        }
                    }
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "click",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["continuation_kind"],
        "same_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "confirmed_follow_up_activity"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The action produced authoritative write-like follow-up activity. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "observation_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub inspect network --id \"req-9\""
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub inspect list ... --wait-field ... --wait-contains ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub explain blockers"
    );
}

#[test]
fn format_response_falls_back_to_recent_network_hint_without_request_id() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-click".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "element" },
            "result": {
                "gesture": "single",
                "outcome_summary": {
                    "class": "confirmed_follow_up_activity",
                    "authoritative": true,
                    "summary": "follow-up activity observed",
                    "activity": {
                        "surface": "network_requests",
                        "terminal_request_count": 2,
                        "last_request": {
                            "method": "POST",
                            "url": "https://example.test/api/signup",
                            "status": 202,
                            "lifecycle": "completed",
                            "resource_type": "XHR"
                        }
                    }
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "click",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub inspect network --last 5"
    );
}

#[test]
fn format_response_prefers_local_runtime_checks_for_same_origin_follow_up_reads() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-click".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "interaction": {
                "frame_context": {
                    "url": "https://try.discourse.org/signup"
                }
            },
            "subject": { "kind": "element" },
            "result": {
                "gesture": "single",
                "outcome_summary": {
                    "class": "confirmed_follow_up_activity",
                    "authoritative": true,
                    "summary": "follow-up activity observed",
                    "activity": {
                        "surface": "network_requests",
                        "terminal_request_count": 4,
                        "last_request": {
                            "request_id": "req-9",
                            "method": "GET",
                            "url": "https://try.discourse.org/u/check_username?username=rub62626",
                            "status": 200,
                            "lifecycle": "completed",
                            "resource_type": "XHR"
                        }
                    }
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "click",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub inspect network --id \"req-9\""
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub find --content ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub extract ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The action produced authoritative same-runtime read-like follow-up activity. Re-check content/read surfaces in the current runtime before branching to any external downstream surface."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "content_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "same_runtime_read_like_follow_up"
    );
}

#[test]
fn format_response_prefers_local_recovery_for_failed_follow_up_activity() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-click".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": { "kind": "element" },
            "result": {
                "gesture": "single",
                "outcome_summary": {
                    "class": "confirmed_follow_up_activity",
                    "authoritative": true,
                    "summary": "follow-up activity observed",
                    "activity": {
                        "surface": "network_requests",
                        "terminal_request_count": 1,
                        "last_request": {
                            "request_id": "req-15",
                            "method": "POST",
                            "url": "https://example.test/api/signup",
                            "status": 422,
                            "lifecycle": "completed",
                            "resource_type": "XHR"
                        }
                    }
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "click",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The action produced authoritative failed follow-up activity in this runtime. Re-check the current page and the failed request before assuming any downstream effect."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub inspect network --id \"req-15\""
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub explain blockers"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub state a11y"
    );
}

#[test]
fn format_response_prefers_local_runtime_checks_for_read_like_network_request_records() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request",
                "request_id": "req-9"
            },
            "result": {
                "request": {
                    "request_id": "req-9",
                    "method": "GET",
                    "url": "https://try.discourse.org/u/check_username?username=rub62626",
                    "status": 200,
                    "lifecycle": "completed",
                    "resource_type": "XHR"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative read-like network evidence for the observed GET request to https://try.discourse.org/u/check_username?username=rub62626. Re-check content/read surfaces in the current runtime before branching to any external downstream surface."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub find --content ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub extract ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "content_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "read_like_network_request"
    );
}

#[test]
fn format_response_prefers_local_runtime_checks_for_read_like_network_request_registries() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request_registry"
            },
            "result": {
                "items": [
                    {
                        "request_id": "req-9",
                        "method": "GET",
                        "url": "https://try.discourse.org/u/check_username?username=rub62626",
                        "status": 200,
                        "lifecycle": "completed",
                        "resource_type": "XHR"
                    },
                    {
                        "request_id": "req-10",
                        "method": "GET",
                        "url": "https://try.discourse.org/u/check_email?email=rub62626@example.test",
                        "status": 200,
                        "lifecycle": "completed",
                        "resource_type": "Fetch"
                    }
                ]
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "network_request_registry"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative read-like network registry evidence. Re-check content/read surfaces in the current runtime before branching to any external downstream surface."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub find --content ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub extract ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "content_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "read_like_network_registry"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["read_like_requests"],
        2
    );
}

#[test]
fn format_response_prefers_mixed_guidance_for_mixed_network_request_registries() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request_registry"
            },
            "result": {
                "items": [
                    {
                        "request_id": "req-9",
                        "method": "GET",
                        "url": "https://example.test/api/missing",
                        "status": 200,
                        "lifecycle": "completed",
                        "resource_type": "Fetch"
                    },
                    {
                        "request_id": "req-11",
                        "method": "POST",
                        "url": "https://example.test/api/signup",
                        "status": 202,
                        "lifecycle": "completed",
                        "resource_type": "XHR"
                    }
                ]
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "network_request_registry"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative mixed network evidence. Re-check local content/read surfaces first, then verify any downstream side effect or local blocker before branching away from this runtime."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub find --content ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub extract ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub inspect list ... --wait-field ... --wait-contains ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][4]["command"],
        "rub explain blockers"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "observation_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "mixed_network_registry"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["read_like_requests"],
        1
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["write_like_requests"],
        1
    );
}

#[test]
fn format_response_prefers_local_runtime_checks_for_terminal_read_like_requests() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network-wait".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request_wait",
                "lifecycle": "terminal"
            },
            "result": {
                "matched": true,
                "request": {
                    "request_id": "req-10",
                    "method": "GET",
                    "url": "https://try.discourse.org/u/check_username?username=rub62626",
                    "status": 200,
                    "lifecycle": "completed",
                    "resource_type": "Fetch"
                },
                "outcome_summary": {
                    "class": "confirmed_terminal_request",
                    "authoritative": true,
                    "summary": "A matching network request reached the requested terminal lifecycle."
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub inspect network --id \"req-10\""
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub find --content ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub extract ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative read-like network evidence for the observed GET request to https://try.discourse.org/u/check_username?username=rub62626. Re-check content/read surfaces in the current runtime before branching to any external downstream surface."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["evidence_kind"],
        "read_like_network_request"
    );
}

#[test]
fn format_response_prefers_downstream_observation_for_write_like_network_request_records() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request",
                "request_id": "req-11"
            },
            "result": {
                "request": {
                    "request_id": "req-11",
                    "method": "POST",
                    "url": "https://example.test/api/signup",
                    "status": 202,
                    "lifecycle": "completed",
                    "resource_type": "XHR"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative write-like network evidence for the observed POST request to https://example.test/api/signup. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub inspect list ... --wait-field ... --wait-contains ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub explain blockers"
    );
}

#[test]
fn format_response_prefers_downstream_observation_for_terminal_write_like_requests() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network-wait".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request_wait",
                "lifecycle": "terminal"
            },
            "result": {
                "matched": true,
                "request": {
                    "request_id": "req-12",
                    "method": "POST",
                    "url": "https://example.test/api/signup",
                    "status": 202,
                    "lifecycle": "completed",
                    "resource_type": "XHR"
                },
                "outcome_summary": {
                    "class": "confirmed_terminal_request",
                    "authoritative": true,
                    "summary": "A matching network request reached the requested terminal lifecycle."
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub inspect network --id \"req-12\""
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub inspect list ... --wait-field ... --wait-contains ..."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][3]["command"],
        "rub explain blockers"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative write-like network evidence for the observed POST request to https://example.test/api/signup. Keep this runtime available while you verify any downstream effect in the owning runtime or inbox/list surface."
    );
}

#[test]
fn format_response_prefers_local_recovery_for_failed_network_request_records() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request",
                "request_id": "req-13"
            },
            "result": {
                "request": {
                    "request_id": "req-13",
                    "method": "POST",
                    "url": "https://example.test/api/signup",
                    "status": 422,
                    "lifecycle": "completed",
                    "resource_type": "XHR"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative failed network evidence for the observed POST request to https://example.test/api/signup. Re-check the current page and the failed request before assuming any downstream effect."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub explain blockers"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub state a11y"
    );
}

#[test]
fn format_response_prefers_local_rechecks_for_in_flight_write_request_records() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-inspect-network".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "network_request",
                "request_id": "req-14"
            },
            "result": {
                "request": {
                    "request_id": "req-14",
                    "method": "POST",
                    "url": "https://example.test/api/signup",
                    "status": 102,
                    "lifecycle": "responded",
                    "resource_type": "XHR"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "inspect",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "The current runtime now has authoritative in-flight write-like network evidence for the observed POST request to https://example.test/api/signup. Keep this runtime available while the request reaches a terminal lifecycle and local state settles."
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub state compact"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub explain blockers"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][2]["command"],
        "rub inspect network --id ..."
    );
}

#[test]
fn format_response_summarizes_redirected_navigation_commit() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-open".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "tab_navigation",
                "action": "open",
                "requested_url": "https://example.test/login",
                "normalized_url": "https://example.test/login",
            },
            "result": {
                "page": {
                    "url": "https://example.test/dashboard",
                    "title": "Dashboard",
                    "final_url": "https://example.test/dashboard",
                    "http_status": 200,
                    "navigation_warning": null
                },
                "active_tab": {
                    "index": 0,
                    "target_id": "target-1",
                    "url": "https://example.test/dashboard",
                    "title": "Dashboard",
                    "active": true
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "open",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "requested_navigation_redirected"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["normalized_url"],
        "https://example.test/login"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["final_page_url"],
        "https://example.test/dashboard"
    );
}

#[test]
fn format_response_summarizes_new_tab_like_navigation_drift() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-open".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "tab_navigation",
                "action": "open",
                "requested_url": "https://example.test/app",
                "normalized_url": "https://example.test/app",
            },
            "result": {
                "page": {
                    "url": "https://example.test/app",
                    "title": "App",
                    "final_url": "https://example.test/app",
                    "http_status": 200,
                    "navigation_warning": null
                },
                "active_tab": {
                    "index": 1,
                    "target_id": "target-2",
                    "url": "chrome://newtab/",
                    "title": "New Tab",
                    "active": true
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "open",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "active_tab_new_tab_drift"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["runtime_roles"]["current_runtime"]["role"],
        "tab_authority_runtime"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub tabs"
    );
}

#[test]
fn format_response_summarizes_interstitial_runtime_interference() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-runtime".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "runtime_surface",
                "surface": "interference"
            },
            "runtime": {
                "status": "active",
                "current_interference": {
                    "kind": "interstitial_navigation",
                    "summary": "interstitial-like navigation drift detected",
                    "current_url": "https://example.test/interstitial#vignette",
                    "primary_url": "https://example.test/app"
                }
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "runtime",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "interstitial_navigation"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub interference recover"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["interference_kind"],
        "interstitial_navigation"
    );
}

#[test]
fn format_response_summarizes_stale_frame_runtime_authority() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-runtime-frame".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "runtime_surface",
                "surface": "frame",
            },
            "runtime": {
                "status": "stale",
                "current_frame": {
                    "frame_id": "child-frame",
                    "name": "child-frame",
                    "depth": 1,
                },
                "primary_frame": {
                    "frame_id": "main-frame",
                    "name": "main",
                    "depth": 0,
                },
                "degraded_reason": "selected_frame_not_found",
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "runtime",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "frame_runtime_stale"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["frame_status"],
        "stale"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["degraded_reason"],
        "selected_frame_not_found"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][0]["command"],
        "rub frames"
    );
}

#[test]
fn format_response_summarizes_degraded_frame_runtime_authority() {
    let response = IpcResponse {
        ipc_protocol_version: rub_ipc::protocol::IPC_PROTOCOL_VERSION.to_string(),
        command_id: Some("019-runtime-frame".to_string()),
        daemon_session_id: None,
        request_id: "019-request".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({
            "subject": {
                "kind": "runtime_surface",
                "surface": "frame",
            },
            "runtime": {
                "status": "degraded",
                "current_frame": null,
                "primary_frame": {
                    "frame_id": "main-frame",
                    "name": "main",
                    "depth": 0,
                },
                "degraded_reason": "frame_probe_failed:no_page",
            }
        })),
        error: None,
        timing: Timing::default(),
    };

    let output = format_response(
        &response,
        "runtime",
        "forum",
        rub_home(),
        false,
        InteractionTraceMode::Compact,
    );
    let json: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        json["data"]["workflow_continuity"]["source_signal"],
        "frame_runtime_degraded"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["authority_observation"]["frame_status"],
        "degraded"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["next_command_hints"][1]["command"],
        "rub frame --top or rub frame --name <frame-name>"
    );
    assert_eq!(
        json["data"]["workflow_continuity"]["summary"],
        "Frame authority is degraded in this runtime (frame_probe_failed:no_page). Re-establish a live frame view before continuing."
    );
}
