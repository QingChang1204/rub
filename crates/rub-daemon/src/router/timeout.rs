use std::sync::Arc;

use super::request_args::{
    LocatorParseOptions, LocatorRequestArgs, locator_json, parse_canonical_locator_from_value,
    parse_json_args,
};
use super::{DaemonRouter, frame_scope};
use crate::session::SessionState;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::locator::LocatorSelection;
use rub_core::model::{WaitCondition, WaitKind, WaitState};
use rub_core::port::BrowserPort;
use rub_core::{DEFAULT_WAIT_AFTER_TIMEOUT_MS, DEFAULT_WAIT_TIMEOUT_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeoutPhase {
    Queue,
    Execution,
    WaitAfter,
}

impl TimeoutPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Execution => "execution",
            Self::WaitAfter => "wait_after",
        }
    }
}

pub(super) struct ParsedWaitCondition {
    pub condition: WaitCondition,
    pub kind_name: &'static str,
    pub probe_value: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WaitProbeArgs {
    text: Option<String>,
    state: Option<String>,
    timeout_ms: Option<u64>,
    #[serde(flatten)]
    locator: LocatorRequestArgs,
}

pub(super) fn timeout_context(
    command: &str,
    phase: TimeoutPhase,
    timeout_ms: u64,
    queue_ms: u64,
    exec_budget_ms: Option<u64>,
) -> serde_json::Value {
    let mut context = serde_json::Map::new();
    context.insert("command".to_string(), serde_json::json!(command));
    context.insert("phase".to_string(), serde_json::json!(phase.as_str()));
    context.insert(
        "transaction_timeout_ms".to_string(),
        serde_json::json!(timeout_ms),
    );
    context.insert("queue_ms".to_string(), serde_json::json!(queue_ms));
    if let Some(exec_budget_ms) = exec_budget_ms {
        context.insert(
            "exec_budget_ms".to_string(),
            serde_json::json!(exec_budget_ms),
        );
    }
    serde_json::Value::Object(context)
}

fn merge_object_context(
    base: Option<serde_json::Value>,
    extra: serde_json::Value,
) -> Option<serde_json::Value> {
    let mut object = match base {
        Some(serde_json::Value::Object(existing)) => existing,
        Some(other) => {
            let mut object = serde_json::Map::new();
            object.insert("previous_context".to_string(), other);
            object
        }
        None => serde_json::Map::new(),
    };

    if let serde_json::Value::Object(extra_object) = extra {
        for (key, value) in extra_object {
            object.insert(key, value);
        }
    }

    Some(serde_json::Value::Object(object))
}

fn wait_probe_context(args: &serde_json::Value) -> serde_json::Value {
    let Ok(parsed) = wait_probe_args(args) else {
        return serde_json::json!({});
    };
    if let Ok(Some(locator)) = parse_canonical_locator_from_value(
        &locator_json(parsed.locator.clone()),
        LocatorParseOptions::OPTIONAL_WAIT,
    ) {
        let mut context = serde_json::json!({
            "kind": locator.kind_name(),
            "value": locator.probe_value(),
        });
        attach_wait_selection_context(&mut context, locator.selection());
        context
    } else if let Some(text) = parsed.text.as_deref() {
        serde_json::json!({
            "kind": "text",
            "value": text,
        })
    } else {
        serde_json::json!({})
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn wait_timeout_error(
    args: &serde_json::Value,
    timeout_ms: u64,
    queue_ms: u64,
    elapsed_ms: Option<u64>,
) -> RubError {
    let mut context = timeout_context(
        "wait",
        TimeoutPhase::Execution,
        timeout_ms,
        queue_ms,
        Some(timeout_ms.saturating_sub(queue_ms)),
    );
    if let Some(object) = context.as_object_mut() {
        if let serde_json::Value::Object(probe) = wait_probe_context(args) {
            for (key, value) in probe {
                object.insert(key, value);
            }
        }
        if let Some(elapsed_ms) = elapsed_ms {
            object.insert("elapsed_ms".to_string(), serde_json::json!(elapsed_ms));
        }
    }

    RubError::Domain(
        ErrorEnvelope::new(ErrorCode::WaitTimeout, "Wait condition timed out")
            .with_context(context),
    )
}

pub(super) fn post_wait_timeout_error(command: &str, args: &serde_json::Value) -> RubError {
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_WAIT_AFTER_TIMEOUT_MS);

    let context = merge_object_context(
        Some(timeout_context(
            command,
            TimeoutPhase::WaitAfter,
            timeout_ms,
            0,
            Some(timeout_ms),
        )),
        wait_probe_context(args),
    )
    .unwrap_or_else(|| serde_json::json!({}));

    RubError::Domain(
        ErrorEnvelope::new(
            ErrorCode::WaitTimeout,
            format!("Post-action wait timed out for '{command}'"),
        )
        .with_context(context),
    )
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn augment_wait_timeout_error(
    err: RubError,
    args: &serde_json::Value,
    timeout_ms: u64,
    queue_ms: u64,
) -> RubError {
    match err {
        RubError::Domain(mut envelope) if envelope.code == ErrorCode::WaitTimeout => {
            envelope.context = merge_object_context(
                envelope.context.take(),
                timeout_context(
                    "wait",
                    TimeoutPhase::Execution,
                    timeout_ms,
                    queue_ms,
                    Some(timeout_ms.saturating_sub(queue_ms)),
                ),
            );

            let probe_context = wait_probe_context(args);
            if probe_context
                .as_object()
                .is_some_and(|object| !object.is_empty())
            {
                envelope.context = merge_object_context(envelope.context.take(), probe_context);
            }

            RubError::Domain(envelope)
        }
        other => other,
    }
}

pub(super) fn parse_wait_condition(
    args: &serde_json::Value,
    default_timeout_ms: u64,
) -> Result<ParsedWaitCondition, RubError> {
    let parsed = wait_probe_args(args)?;
    let timeout_ms = parsed.timeout_ms.unwrap_or(default_timeout_ms);

    let state = parse_wait_state(parsed.state.as_deref())?;
    let locator = parse_canonical_locator_from_value(
        &locator_json(parsed.locator.clone()),
        LocatorParseOptions::OPTIONAL_WAIT,
    )?;
    let has_text = parsed.text.as_deref().is_some();

    if locator.is_some() && has_text {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe is ambiguous: provide either page text or a single locator, not both",
        ));
    }
    if locator.is_none() && !has_text {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Missing required wait probe: selector, target_text, role, label, testid, or text",
        ));
    }
    if has_text && parsed.locator.has_selection() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Match selection is not supported for page text waits",
        ));
    }

    let (kind, kind_name, probe_value) = if let Some(locator) = locator {
        (
            WaitKind::Locator {
                locator: locator.clone(),
                state,
            },
            locator.kind_name(),
            locator.probe_value(),
        )
    } else if let Some(text) = parsed.text.as_deref() {
        (
            WaitKind::Text {
                text: text.to_string(),
            },
            "text",
            text.to_string(),
        )
    } else {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Missing required wait probe",
        ));
    };

    Ok(ParsedWaitCondition {
        condition: WaitCondition {
            kind,
            timeout_ms,
            frame_id: None,
        },
        kind_name,
        probe_value,
    })
}

fn parse_wait_state(state: Option<&str>) -> Result<WaitState, RubError> {
    let state_str = state.unwrap_or("visible");
    match state_str {
        "visible" => Ok(WaitState::Visible),
        "hidden" => Ok(WaitState::Hidden),
        "attached" => Ok(WaitState::Attached),
        "detached" => Ok(WaitState::Detached),
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid wait state: '{other}'. Valid: visible, hidden, attached, detached"),
        )),
    }
}

fn attach_wait_selection_context(
    context: &mut serde_json::Value,
    selection: Option<LocatorSelection>,
) {
    let Some(selection) = selection else {
        return;
    };
    let value = match selection {
        LocatorSelection::First => serde_json::json!("first"),
        LocatorSelection::Last => serde_json::json!("last"),
        LocatorSelection::Nth(nth) => serde_json::json!({ "nth": nth }),
    };
    if let Some(object) = context.as_object_mut() {
        object.insert("selection".to_string(), value);
    }
}

fn wait_probe_args(args: &serde_json::Value) -> Result<WaitProbeArgs, RubError> {
    let mut filtered = serde_json::Map::new();
    for key in ["text", "state", "timeout_ms"] {
        if let Some(value) = args.get(key) {
            filtered.insert(key.to_string(), value.clone());
        }
    }
    for key in [
        "index",
        "selector",
        "target_text",
        "role",
        "label",
        "testid",
        "first",
        "last",
        "nth",
    ] {
        if let Some(value) = args.get(key) {
            filtered.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = args.get("element_ref").or_else(|| args.get("ref")) {
        filtered.insert("element_ref".to_string(), value.clone());
    }
    parse_json_args(&serde_json::Value::Object(filtered), "wait")
}

pub(super) async fn execute_wait_command(
    router: &DaemonRouter,
    browser: Arc<dyn BrowserPort>,
    args: serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let mut parsed = parse_wait_condition(&args, DEFAULT_WAIT_TIMEOUT_MS)?;
    parsed.condition.frame_id =
        frame_scope::effective_request_frame_id(router, &args, state).await?;
    let timeout_ms = parsed.condition.timeout_ms;
    let start = std::time::Instant::now();
    match browser.wait_for(&parsed.condition).await {
        Ok(()) => Ok(serde_json::json!({
            "subject": {
                "kind": "wait_condition",
                "wait_kind": parsed.kind_name,
                "probe_value": parsed.probe_value,
                "frame_id": parsed.condition.frame_id,
            },
            "result": {
                "matched": true,
                "elapsed_ms": start.elapsed().as_millis() as u64,
            }
        })),
        Err(RubError::Domain(envelope)) if envelope.code == ErrorCode::WaitTimeout => {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            Err(RubError::Domain(
                ErrorEnvelope::new(
                    ErrorCode::WaitTimeout,
                    format!(
                        "Timeout after {}ms waiting for {} '{}'",
                        timeout_ms, parsed.kind_name, parsed.probe_value
                    ),
                )
                .with_context(serde_json::json!({
                    "kind": parsed.kind_name,
                    "value": parsed.probe_value,
                    "timeout_ms": timeout_ms,
                    "elapsed_ms": elapsed_ms,
                })),
            ))
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_wait_condition, post_wait_timeout_error};
    use rub_core::error::ErrorCode;
    use rub_core::{DEFAULT_WAIT_AFTER_TIMEOUT_MS, DEFAULT_WAIT_TIMEOUT_MS};

    #[test]
    fn explicit_wait_uses_long_default_timeout() {
        let parsed = parse_wait_condition(
            &serde_json::json!({
                "selector": "#ready",
            }),
            DEFAULT_WAIT_TIMEOUT_MS,
        )
        .expect("wait condition should parse");

        assert_eq!(parsed.condition.timeout_ms, DEFAULT_WAIT_TIMEOUT_MS);
    }

    #[test]
    fn post_wait_uses_bounded_default_timeout() {
        let parsed = parse_wait_condition(
            &serde_json::json!({
                "selector": "#ready",
            }),
            DEFAULT_WAIT_AFTER_TIMEOUT_MS,
        )
        .expect("post wait condition should parse");

        assert_eq!(parsed.condition.timeout_ms, DEFAULT_WAIT_AFTER_TIMEOUT_MS);
    }

    #[test]
    fn post_wait_timeout_error_uses_bounded_default_in_context() {
        let err = post_wait_timeout_error("open", &serde_json::json!({ "selector": "#ready" }));
        let envelope = err.into_envelope();
        assert_eq!(envelope.code, ErrorCode::WaitTimeout);
        assert_eq!(
            envelope
                .context
                .and_then(|ctx| ctx["transaction_timeout_ms"].as_u64()),
            Some(DEFAULT_WAIT_AFTER_TIMEOUT_MS)
        );
    }
}
