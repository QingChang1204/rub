use std::sync::Arc;

use super::dispatch::command_supports_post_wait;
use super::timeout::{parse_wait_condition, post_wait_timeout_error};
use super::{DaemonRouter, frame_scope};
use crate::session::SessionState;
use rub_core::DEFAULT_WAIT_AFTER_TIMEOUT_MS;
use rub_core::error::{ErrorCode, RubError};
use rub_core::port::BrowserPort;

pub(super) async fn apply_post_wait_if_requested(
    router: &DaemonRouter,
    browser: Arc<dyn BrowserPort>,
    command: &str,
    args: &serde_json::Value,
    mut data: serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    if !command_supports_post_wait(command) {
        return Ok(data);
    }

    let Some(wait_args) = wait_after_args(args) else {
        return Ok(data);
    };

    let mut parsed = parse_wait_condition(&wait_args, DEFAULT_WAIT_AFTER_TIMEOUT_MS)?;
    parsed.condition.frame_id =
        frame_scope::effective_request_frame_id(router, args, state).await?;
    let start = std::time::Instant::now();
    match browser.wait_for(&parsed.condition).await {
        Ok(()) => {
            if let Some(object) = data.as_object_mut() {
                object.insert(
                    "wait_after".to_string(),
                    serde_json::json!({
                        "matched": true,
                        "kind": parsed.kind_name,
                        "value": parsed.probe_value,
                        "elapsed_ms": start.elapsed().as_millis() as u64,
                    }),
                );
            }
            Ok(data)
        }
        Err(RubError::Domain(envelope)) if envelope.code == ErrorCode::WaitTimeout => {
            Err(post_wait_timeout_error(command, &wait_args))
        }
        Err(other) => Err(other),
    }
}

pub(super) fn wait_after_args(args: &serde_json::Value) -> Option<serde_json::Value> {
    let wait_after = args.get("wait_after")?.clone();
    let has_probe = wait_after
        .get("selector")
        .and_then(|value| value.as_str())
        .is_some()
        || wait_after
            .get("target_text")
            .and_then(|value| value.as_str())
            .is_some()
        || wait_after
            .get("role")
            .and_then(|value| value.as_str())
            .is_some()
        || wait_after
            .get("label")
            .and_then(|value| value.as_str())
            .is_some()
        || wait_after
            .get("testid")
            .and_then(|value| value.as_str())
            .is_some()
        || wait_after
            .get("text")
            .and_then(|value| value.as_str())
            .is_some();
    has_probe.then_some(wait_after)
}

#[cfg(test)]
mod tests {
    use super::command_supports_post_wait;

    #[test]
    fn wait_after_supports_forward_and_reload() {
        assert!(command_supports_post_wait("forward"));
        assert!(command_supports_post_wait("reload"));
    }
}
