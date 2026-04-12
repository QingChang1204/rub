use rub_core::error::{ErrorCode, ErrorEnvelope};

pub(crate) fn ensure_committed_automation_result(
    command: &str,
    data: Option<&serde_json::Value>,
) -> Result<(), ErrorEnvelope> {
    let wait_after_matched = data
        .and_then(|value| value.get("wait_after"))
        .and_then(|value| value.get("matched"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let Some(interaction) = data
        .and_then(|value| value.get("interaction"))
        .and_then(|value| value.as_object())
    else {
        return Ok(());
    };

    let Some(status) = interaction
        .get("confirmation_status")
        .and_then(|value| value.as_str())
    else {
        return Ok(());
    };

    if status == "confirmed" {
        return Ok(());
    }

    if wait_after_matched {
        return Ok(());
    }

    Err(ErrorEnvelope::new(
        ErrorCode::WaitTimeout,
        format!(
            "Automation step '{command}' did not reach a committed interaction confirmation fence"
        ),
    )
    .with_context(serde_json::json!({
        "reason": "automation_interaction_confirmation_not_committed",
        "command": command,
        "confirmation_status": status,
        "confirmation_kind": interaction
            .get("confirmation_kind")
            .and_then(|value| value.as_str()),
        "confirmation_details": interaction.get("confirmation_details").cloned(),
    })))
}

#[cfg(test)]
mod tests {
    use super::ensure_committed_automation_result;
    use rub_core::error::ErrorCode;

    #[test]
    fn confirmed_interaction_is_automation_committed() {
        ensure_committed_automation_result(
            "click",
            Some(&serde_json::json!({
                "interaction": {
                    "confirmation_status": "confirmed",
                    "confirmation_kind": "page_mutation",
                }
            })),
        )
        .expect("confirmed automation interaction should pass");
    }

    #[test]
    fn degraded_interaction_fails_automation_commit_fence() {
        let error = ensure_committed_automation_result(
            "click",
            Some(&serde_json::json!({
                "interaction": {
                    "confirmation_status": "degraded",
                    "confirmation_kind": "value_applied",
                }
            })),
        )
        .expect_err("degraded interaction should fail closed in automation");
        assert_eq!(error.code, ErrorCode::WaitTimeout);
    }

    #[test]
    fn matched_wait_after_is_explicit_fallback_automation_authority() {
        ensure_committed_automation_result(
            "click",
            Some(&serde_json::json!({
                "interaction": {
                    "confirmation_status": "degraded",
                    "confirmation_kind": "page_mutation",
                },
                "wait_after": {
                    "matched": true,
                    "kind": "text",
                    "value": "Saved",
                }
            })),
        )
        .expect("matched wait_after should satisfy automation commit fence");
    }
}
