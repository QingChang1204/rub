use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::model::{TriggerEvidenceInfo, TriggerInfo, TriggerResultInfo, TriggerStatus};
use tracing::warn;

use crate::session::SessionState;

use super::action::trigger_action_execution_info;
use super::condition::trigger_evidence_consumption_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TriggerEvidenceDisposition {
    Preserve,
    ConsumeOnPermanentActionFailure,
}

pub(super) async fn record_trigger_failure(
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    envelope: ErrorEnvelope,
    evidence: Option<TriggerEvidenceInfo>,
    command_id: Option<String>,
    evidence_disposition: TriggerEvidenceDisposition,
) {
    let result_status = classify_trigger_error_status(envelope.code);
    let consumed_evidence_fingerprint = evidence.as_ref().and_then(|evidence| {
        trigger_failure_consumes_evidence(&envelope, result_status, evidence_disposition)
            .then(|| trigger_evidence_consumption_key(evidence))
    });
    let summary = format!(
        "trigger action failed: {}: {}",
        envelope.code, envelope.message
    );
    let reason = envelope
        .context
        .as_ref()
        .and_then(|context| context.get("reason"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let error_context = envelope.context.clone();
    warn!(
        trigger_id = trigger.id,
        result_status = ?result_status,
        summary = %summary,
        "Trigger failure"
    );
    let _ = state
        .record_trigger_outcome_with_fallback(
            trigger,
            trigger.lifecycle_generation,
            evidence,
            TriggerResultInfo {
                trigger_id: trigger.id,
                status: result_status,
                next_status: trigger.status,
                summary,
                command_id,
                action: Some(trigger_action_execution_info(trigger, &state.rub_home)),
                result: None,
                error_code: Some(envelope.code),
                reason,
                error_context,
                consumed_evidence_fingerprint,
            },
        )
        .await;
}

pub(super) fn contextualize_trigger_error(error: RubError, prefix: &str) -> ErrorEnvelope {
    let mut envelope = error.into_envelope();
    envelope.message = format!("{prefix}: {}", envelope.message);
    envelope
}

pub(super) fn classify_trigger_error_status(code: ErrorCode) -> TriggerStatus {
    match code {
        ErrorCode::InvalidInput
        | ErrorCode::ElementNotFound
        | ErrorCode::ElementNotInteractable
        | ErrorCode::StaleSnapshot
        | ErrorCode::StaleIndex
        | ErrorCode::WaitTimeout
        | ErrorCode::TabNotFound
        | ErrorCode::NoMatchingOption
        | ErrorCode::FileNotFound
        | ErrorCode::AutomationPaused => TriggerStatus::Blocked,
        _ => TriggerStatus::Degraded,
    }
}

pub(super) fn trigger_failure_consumes_evidence(
    envelope: &ErrorEnvelope,
    result_status: TriggerStatus,
    evidence_disposition: TriggerEvidenceDisposition,
) -> bool {
    matches!(
        evidence_disposition,
        TriggerEvidenceDisposition::ConsumeOnPermanentActionFailure
    ) && matches!(result_status, TriggerStatus::Blocked)
        && trigger_failure_phase(envelope) == Some("action")
        && permanently_irretryable_trigger_action_failure(envelope)
}

fn trigger_failure_phase(envelope: &ErrorEnvelope) -> Option<&str> {
    envelope
        .context
        .as_ref()
        .and_then(|context| context.get("phase"))
        .and_then(|value| value.as_str())
}

fn permanently_irretryable_trigger_action_failure(envelope: &ErrorEnvelope) -> bool {
    match envelope.code {
        ErrorCode::InvalidInput => true,
        ErrorCode::FileNotFound => trigger_failure_action_command(envelope) != Some("upload"),
        _ => false,
    }
}

fn trigger_failure_action_command(envelope: &ErrorEnvelope) -> Option<&str> {
    envelope
        .context
        .as_ref()
        .and_then(|context| context.get("action_command"))
        .and_then(|value| value.as_str())
}

#[cfg(test)]
mod tests {
    use super::{
        TriggerEvidenceDisposition, classify_trigger_error_status,
        trigger_failure_consumes_evidence,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::TriggerStatus;
    use serde_json::json;

    #[test]
    fn pre_action_blocked_failure_does_not_consume_evidence() {
        let envelope = ErrorEnvelope::new(ErrorCode::AutomationPaused, "paused")
            .with_context(json!({ "phase": "target_switch" }));

        assert!(!trigger_failure_consumes_evidence(
            &envelope,
            classify_trigger_error_status(envelope.code),
            TriggerEvidenceDisposition::ConsumeOnPermanentActionFailure,
        ));
    }

    #[test]
    fn transient_action_failure_does_not_consume_evidence() {
        let envelope = ErrorEnvelope::new(ErrorCode::AutomationPaused, "paused")
            .with_context(json!({ "phase": "action" }));

        assert_eq!(
            classify_trigger_error_status(envelope.code),
            TriggerStatus::Blocked
        );
        assert!(!trigger_failure_consumes_evidence(
            &envelope,
            classify_trigger_error_status(envelope.code),
            TriggerEvidenceDisposition::ConsumeOnPermanentActionFailure,
        ));
    }

    #[test]
    fn permanent_action_failure_consumes_evidence() {
        let envelope = ErrorEnvelope::new(ErrorCode::InvalidInput, "invalid action")
            .with_context(json!({ "phase": "action" }));

        assert!(trigger_failure_consumes_evidence(
            &envelope,
            classify_trigger_error_status(envelope.code),
            TriggerEvidenceDisposition::ConsumeOnPermanentActionFailure,
        ));
    }

    #[test]
    fn upload_file_not_found_action_failure_preserves_evidence() {
        let envelope = ErrorEnvelope::new(ErrorCode::FileNotFound, "missing upload file")
            .with_context(json!({
                "phase": "action",
                "action_command": "upload",
            }));

        assert!(!trigger_failure_consumes_evidence(
            &envelope,
            classify_trigger_error_status(envelope.code),
            TriggerEvidenceDisposition::ConsumeOnPermanentActionFailure,
        ));
    }
}
