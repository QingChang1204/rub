use std::cell::Cell;

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
    let mut fingerprint = String::with_capacity(request.command.len() + 64);
    fingerprint.push_str(&request.command);
    fingerprint.push('\u{1f}');
    append_canonical_json(&request.args, &mut fingerprint);
    REPLAY_FINGERPRINT_OUTPUT_BYTES.with(|count| {
        count.set(
            count
                .get()
                .saturating_add(fingerprint.len().try_into().unwrap_or(u64::MAX)),
        )
    });
    fingerprint
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
                object_key_sort_steps: 19,
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
                object_key_sort_steps: 6,
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
}
