use std::sync::Arc;

use rub_core::error::{ErrorCode, ErrorEnvelope};
use rub_core::model::{TriggerEvidenceInfo, TriggerInfo, TriggerResultInfo, TriggerStatus};
use tracing::warn;

use crate::session::SessionState;

use super::action::trigger_action_execution_info;
use super::condition::trigger_evidence_consumption_key;

pub(super) async fn record_trigger_failure(
    state: &Arc<SessionState>,
    trigger: &TriggerInfo,
    envelope: ErrorEnvelope,
    evidence: Option<TriggerEvidenceInfo>,
    command_id: Option<String>,
) {
    let result_status = classify_trigger_error_status(envelope.code);
    let consumed_evidence_fingerprint = evidence.as_ref().and_then(|evidence| {
        matches!(result_status, TriggerStatus::Blocked)
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
    warn!(
        trigger_id = trigger.id,
        result_status = ?result_status,
        summary = %summary,
        "Trigger failure"
    );
    let _ = state
        .record_trigger_outcome(
            trigger.id,
            trigger.status,
            evidence,
            TriggerResultInfo {
                trigger_id: trigger.id,
                status: result_status,
                summary,
                command_id,
                action: Some(trigger_action_execution_info(trigger, &state.rub_home)),
                result: None,
                error_code: Some(envelope.code),
                reason,
                consumed_evidence_fingerprint,
            },
        )
        .await;
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
