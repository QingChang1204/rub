use std::cell::Cell;

use crate::router::{interaction, navigation, observe, storage, workflow};
use rub_ipc::protocol::IpcRequest;

thread_local! {
    static REPLAY_FINGERPRINT_INVOCATIONS: Cell<u64> = const { Cell::new(0) };
    static REPLAY_FINGERPRINT_OUTPUT_BYTES: Cell<u64> = const { Cell::new(0) };
    static REPLAY_FINGERPRINT_ARRAY_ENTRY_STEPS: Cell<u64> = const { Cell::new(0) };
    static REPLAY_FINGERPRINT_OBJECT_KEY_SORT_STEPS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayFingerprintMetrics {
    pub(crate) invocations: u64,
    pub(crate) output_bytes: u64,
    pub(crate) array_entry_steps: u64,
    pub(crate) object_key_sort_steps: u64,
}

pub(super) fn replay_request_fingerprint(request: &IpcRequest) -> String {
    REPLAY_FINGERPRINT_INVOCATIONS.with(|count| count.set(count.get().saturating_add(1)));
    let semantic_args = semantic_replay_args(&request.command, &request.args);
    let mut fingerprint = String::with_capacity(request.command.len() + 64);
    fingerprint.push_str(&request.command);
    fingerprint.push('\u{1f}');
    append_canonical_json(&semantic_args, &mut fingerprint);
    REPLAY_FINGERPRINT_OUTPUT_BYTES.with(|count| {
        count.set(
            count
                .get()
                .saturating_add(fingerprint.len().try_into().unwrap_or(u64::MAX)),
        )
    });
    fingerprint
}

fn semantic_replay_args(command: &str, args: &serde_json::Value) -> serde_json::Value {
    if command == "_orchestration_target_dispatch"
        && let Some(projected) = semantic_orchestration_target_dispatch_args(args)
    {
        return projected;
    }
    navigation::semantic_replay_args(command, args)
        .or_else(|| interaction::semantic_replay_args(command, args))
        .or_else(|| match command {
            "observe" => observe::semantic_replay_args(args),
            "storage" => storage::semantic_replay_args(args),
            "fill" | "_trigger_fill" | "pipe" | "_trigger_pipe" => {
                workflow::semantic_replay_args(command, args)
            }
            _ => None,
        })
        .unwrap_or_else(|| scrub_deadline_projected_timeout_authority(args))
}

fn scrub_deadline_projected_timeout_authority(args: &serde_json::Value) -> serde_json::Value {
    let mut projected = args.clone();
    if let Some(object) = projected.as_object_mut() {
        object.remove("timeout_ms");
        object.remove("wait_timeout_ms");
    }
    projected
}

fn semantic_orchestration_target_dispatch_args(
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    let target = args.get("target")?;
    let request: IpcRequest = serde_json::from_value(args.get("request")?.clone()).ok()?;
    Some(serde_json::json!({
        "target": {
            "session_id": target.get("session_id"),
            "tab_target_id": target.get("tab_target_id"),
            "frame_id": target.get("frame_id"),
        },
        "request": {
            "command": request.command,
            "args": semantic_replay_args(&request.command, &request.args),
        }
    }))
}

fn append_canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(value) => append_json_string_literal(value, out),
        serde_json::Value::Array(values) => {
            REPLAY_FINGERPRINT_ARRAY_ENTRY_STEPS.with(|count| {
                count.set(
                    count
                        .get()
                        .saturating_add(values.len().try_into().unwrap_or(u64::MAX)),
                )
            });
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                append_canonical_json(value, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(values) => {
            REPLAY_FINGERPRINT_OBJECT_KEY_SORT_STEPS.with(|count| {
                count.set(
                    count
                        .get()
                        .saturating_add(values.len().try_into().unwrap_or(u64::MAX)),
                )
            });
            out.push('{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                append_json_string_literal(key, out);
                out.push(':');
                append_canonical_json(value, out);
            }
            out.push('}');
        }
    }
}

fn append_json_string_literal(value: &str, out: &mut String) {
    if !requires_json_string_escaping(value) {
        out.push('"');
        out.push_str(value);
        out.push('"');
        return;
    }

    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1F}' => {
                let code = ch as u32;
                out.push_str("\\u00");
                out.push(char::from_digit((code >> 4) & 0xF, 16).expect("hex digit"));
                out.push(char::from_digit(code & 0xF, 16).expect("hex digit"));
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
}

fn requires_json_string_escaping(value: &str) -> bool {
    value
        .bytes()
        .any(|byte| byte <= 0x1F || byte == b'"' || byte == b'\\')
}

#[cfg(test)]
pub(crate) fn replay_fingerprint_metrics_snapshot() -> ReplayFingerprintMetrics {
    ReplayFingerprintMetrics {
        invocations: REPLAY_FINGERPRINT_INVOCATIONS.with(Cell::get),
        output_bytes: REPLAY_FINGERPRINT_OUTPUT_BYTES.with(Cell::get),
        array_entry_steps: REPLAY_FINGERPRINT_ARRAY_ENTRY_STEPS.with(Cell::get),
        object_key_sort_steps: REPLAY_FINGERPRINT_OBJECT_KEY_SORT_STEPS.with(Cell::get),
    }
}

#[cfg(test)]
pub(crate) fn reset_replay_fingerprint_metrics() {
    REPLAY_FINGERPRINT_INVOCATIONS.with(|count| count.set(0));
    REPLAY_FINGERPRINT_OUTPUT_BYTES.with(|count| count.set(0));
    REPLAY_FINGERPRINT_ARRAY_ENTRY_STEPS.with(|count| count.set(0));
    REPLAY_FINGERPRINT_OBJECT_KEY_SORT_STEPS.with(|count| count.set(0));
}

#[cfg(test)]
mod tests {
    use super::{
        ReplayFingerprintMetrics, replay_fingerprint_metrics_snapshot, replay_request_fingerprint,
        reset_replay_fingerprint_metrics,
    };
    use rub_ipc::protocol::IpcRequest;

    #[test]
    fn replay_fingerprint_metrics_capture_nested_payload_baseline() {
        reset_replay_fingerprint_metrics();
        let request = IpcRequest::new(
            "pipe",
            serde_json::json!({
                "spec": [
                    {
                        "command": "fill",
                        "args": {
                            "label": "Email",
                            "text": "$RUB_EMAIL",
                            "visible": true
                        }
                    },
                    {
                        "command": "extract",
                        "args": {
                            "fields": {
                                "title": { "kind": "text", "selector": "h1" },
                                "items": {
                                    "kind": "collection",
                                    "fields": {
                                        "label": { "kind": "text", "selector": ".item-label" }
                                    }
                                }
                            }
                        }
                    }
                ],
                "timeout_ms": 5000
            }),
            5_000,
        );

        let fingerprint = replay_request_fingerprint(&request);
        let metrics = replay_fingerprint_metrics_snapshot();

        assert!(fingerprint.starts_with("pipe\u{1f}{"));
        assert_eq!(
            metrics,
            ReplayFingerprintMetrics {
                invocations: 1,
                output_bytes: fingerprint.len() as u64,
                array_entry_steps: 2,
                object_key_sort_steps: 18,
            }
        );
    }

    #[test]
    fn replay_fingerprint_is_stable_across_object_key_order() {
        reset_replay_fingerprint_metrics();
        let request_a = IpcRequest::new(
            "type",
            serde_json::json!({
                "selector": "#email",
                "text": "user@example.com",
                "clear": true
            }),
            1_000,
        );
        let request_b = IpcRequest::new(
            "type",
            serde_json::json!({
                "text": "user@example.com",
                "clear": true,
                "selector": "#email"
            }),
            1_000,
        );

        let fingerprint_a = replay_request_fingerprint(&request_a);
        let fingerprint_b = replay_request_fingerprint(&request_b);
        let metrics = replay_fingerprint_metrics_snapshot();

        assert_eq!(fingerprint_a, fingerprint_b);
        assert_eq!(
            metrics,
            ReplayFingerprintMetrics {
                invocations: 2,
                output_bytes: (fingerprint_a.len() + fingerprint_b.len()) as u64,
                array_entry_steps: 0,
                object_key_sort_steps: 30,
            }
        );
    }

    #[test]
    fn replay_fingerprint_string_encoding_matches_serde_json_literals() {
        let samples = [
            "",
            "plain-text",
            "quote: \" backslash: \\",
            "line\nfeed\rreturn\tindent",
            "control:\u{0001}\u{001f}",
            "emoji: 🦀",
            "separator:\u{2028}\u{2029}",
        ];

        for sample in samples {
            let request = IpcRequest::new("exec", serde_json::json!({ "text": sample }), 1_000);
            let fingerprint = replay_request_fingerprint(&request);
            let expected = format!(
                "exec\u{1f}{{\"text\":{}}}",
                serde_json::to_string(sample).expect("json string serialization")
            );
            assert_eq!(fingerprint, expected, "sample {sample:?}");
        }
    }

    #[test]
    fn replay_fingerprint_normalizes_interaction_defaults_and_ignored_wrapper_metadata() {
        let request_a = IpcRequest::new(
            "click",
            serde_json::json!({
                "selector": "#submit",
                "_trigger": { "kind": "trigger_action", "trigger_id": "trg-1" },
                "_orchestration": {
                    "frame_id": "frame-7",
                    "command_id": "cmd-a",
                    "correlation_key": "corr-a"
                }
            }),
            1_000,
        );
        let request_b = IpcRequest::new(
            "click",
            serde_json::json!({
                "selector": "#submit",
                "gesture": "single",
                "_orchestration": {
                    "frame_id": "frame-7",
                    "command_id": "cmd-b",
                    "correlation_key": "corr-b"
                }
            }),
            1_000,
        );

        assert_eq!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_preserves_semantic_orchestration_frame_authority() {
        let request_a = IpcRequest::new(
            "keys",
            serde_json::json!({
                "keys": "Enter",
                "_orchestration": { "frame_id": "frame-a" }
            }),
            1_000,
        );
        let request_b = IpcRequest::new(
            "keys",
            serde_json::json!({
                "keys": "Enter",
                "_orchestration": { "frame_id": "frame-b" }
            }),
            1_000,
        );

        assert_ne!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_ignores_observe_path_state_metadata() {
        let request_a = IpcRequest::new(
            "observe",
            serde_json::json!({
                "path": "/tmp/observe.png",
                "path_state": { "path_authority": "cli.observe.path" }
            }),
            1_000,
        );
        let request_b = IpcRequest::new(
            "observe",
            serde_json::json!({
                "path": "/tmp/observe.png"
            }),
            1_000,
        );

        assert_eq!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_normalizes_workflow_spec_and_ignores_wrapper_metadata() {
        let request_a = IpcRequest::new(
            "pipe",
            serde_json::json!({
                "spec": "{\"steps\":[{\"command\":\"open\",\"args\":{\"url\":\"https://example.com\"}}]}",
                "spec_source": { "kind": "inline", "path": "/tmp/workflow.json" },
                "_trigger": { "kind": "trigger_action" },
                "_orchestration": {
                    "frame_id": "frame-9",
                    "command_id": "cmd-a"
                }
            }),
            1_000,
        );
        let request_b = IpcRequest::new(
            "pipe",
            serde_json::json!({
                "spec": {
                    "steps": [
                        {
                            "command": "open",
                            "args": { "url": "https://example.com" }
                        }
                    ]
                },
                "_orchestration": {
                    "frame_id": "frame-9",
                    "command_id": "cmd-b"
                }
            }),
            1_000,
        );

        assert_eq!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_normalizes_orchestration_target_dispatch_inner_execution_metadata() {
        let inner_a = IpcRequest::new(
            "click",
            serde_json::json!({
                "selector": "#apply",
                "wait_after": { "text": "Applied", "timeout_ms": 5000 },
                "_orchestration": {
                    "frame_id": "frame-target",
                    "execution_id": "exec-a",
                    "command_identity_kind": "evidence_key",
                    "command_identity_key": "source_tab_text_present:Ready::Ready",
                    "command_id": "orchestration:idem:source_tab_text_present:Ready::Ready:0",
                    "correlation_key": "corr-a",
                    "idempotency_key": "idem",
                    "target_session_id": "sess-target"
                }
            }),
            1_000,
        )
        .with_command_id("orchestration:idem:source_tab_text_present:Ready::Ready:0")
        .expect("static command id should be valid");
        let inner_b = IpcRequest::new(
            "click",
            serde_json::json!({
                "selector": "#apply",
                "wait_after": { "text": "Applied", "timeout_ms": 5000 },
                "_orchestration": {
                    "frame_id": "frame-target",
                    "execution_id": "exec-b",
                    "command_identity_kind": "evidence_key",
                    "command_identity_key": "source_tab_text_present:Ready::Ready",
                    "command_id": "orchestration:idem:source_tab_text_present:Ready::Ready:0",
                    "correlation_key": "corr-b",
                    "idempotency_key": "idem",
                    "target_session_id": "sess-target"
                }
            }),
            2_000,
        )
        .with_command_id("orchestration:idem:source_tab_text_present:Ready::Ready:0")
        .expect("static command id should be valid");

        let request_a = IpcRequest::new(
            "_orchestration_target_dispatch",
            serde_json::json!({
                "target": {
                    "session_id": "sess-target",
                    "session_name": "target",
                    "tab_target_id": "tab-target",
                    "frame_id": "frame-target",
                },
                "request": inner_a,
            }),
            1_000,
        )
        .with_command_id("orchestration_target_dispatch:sess-target:step-cmd")
        .expect("static command id should be valid");
        let request_b = IpcRequest::new(
            "_orchestration_target_dispatch",
            serde_json::json!({
                "target": {
                    "session_name": "target",
                    "frame_id": "frame-target",
                    "tab_target_id": "tab-target",
                    "session_id": "sess-target",
                },
                "request": inner_b,
            }),
            2_000,
        )
        .with_command_id("orchestration_target_dispatch:sess-target:step-cmd")
        .expect("static command id should be valid");

        assert_eq!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_distinguishes_orchestration_target_dispatch_target_authority() {
        let inner = IpcRequest::new(
            "click",
            serde_json::json!({
                "selector": "#apply",
                "_orchestration": { "frame_id": "frame-target" }
            }),
            1_000,
        )
        .with_command_id("step-cmd")
        .expect("static command id should be valid");

        let request_a = IpcRequest::new(
            "_orchestration_target_dispatch",
            serde_json::json!({
                "target": {
                    "session_id": "sess-a",
                    "tab_target_id": "tab-target",
                    "frame_id": "frame-target",
                },
                "request": inner.clone(),
            }),
            1_000,
        );
        let request_b = IpcRequest::new(
            "_orchestration_target_dispatch",
            serde_json::json!({
                "target": {
                    "session_id": "sess-b",
                    "tab_target_id": "tab-target",
                    "frame_id": "frame-target",
                },
                "request": inner,
            }),
            1_000,
        );

        assert_ne!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_ignores_deadline_projected_timeout_fields_on_raw_fallback_commands() {
        let request_a = IpcRequest::new(
            "download",
            serde_json::json!({
                "sub": "wait",
                "id": "guid-1",
                "timeout_ms": 5_000
            }),
            5_000,
        );
        let request_b = IpcRequest::new(
            "download",
            serde_json::json!({
                "sub": "wait",
                "id": "guid-1",
                "timeout_ms": 250
            }),
            250,
        );

        assert_eq!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }

    #[test]
    fn replay_fingerprint_ignores_deadline_projected_wait_timeout_fields_on_raw_fallback_commands()
    {
        let request_a = IpcRequest::new(
            "inspect",
            serde_json::json!({
                "sub": "list",
                "wait": true,
                "wait_timeout_ms": 5_000,
                "kind": "tabs"
            }),
            5_000,
        );
        let request_b = IpcRequest::new(
            "inspect",
            serde_json::json!({
                "sub": "list",
                "wait": true,
                "wait_timeout_ms": 200,
                "kind": "tabs"
            }),
            200,
        );

        assert_eq!(
            replay_request_fingerprint(&request_a),
            replay_request_fingerprint(&request_b)
        );
    }
}
