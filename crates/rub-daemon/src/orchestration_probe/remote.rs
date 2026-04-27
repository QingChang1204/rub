use crate::router::TransactionDeadline;
use rub_core::error::ErrorEnvelope;
use rub_core::model::{OrchestrationSessionInfo, TriggerConditionSpec};
use rub_ipc::protocol::IpcRequest;

use super::OrchestrationProbeResult;
use crate::orchestration_executor::{
    RemoteDispatchContract, bounded_orchestration_timeout_ms, decode_orchestration_success_payload,
    dispatch_remote_orchestration_request,
};

pub(crate) async fn dispatch_remote_orchestration_probe(
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &TriggerConditionSpec,
    after_sequence: u64,
    last_observed_drop_count: u64,
    outer_deadline: Option<TransactionDeadline>,
) -> Result<OrchestrationProbeResult, ErrorEnvelope> {
    let timeout_ms = bounded_orchestration_timeout_ms(30_000, outer_deadline).ok_or_else(|| {
        ErrorEnvelope::new(
            rub_core::error::ErrorCode::IpcTimeout,
            "Orchestration source probe exhausted the caller-owned timeout budget before remote dispatch",
        )
        .with_context(serde_json::json!({
            "reason": "orchestration_source_probe_timeout_budget_exhausted",
            "source_session_id": session.session_id,
            "source_session_name": session.session_name,
        }))
    })?;
    let request = IpcRequest::new(
        "_orchestration_probe",
        serde_json::json!({
            "tab_target_id": tab_target_id,
            "frame_id": frame_id,
            "condition": condition,
            "after_sequence": after_sequence,
            "last_observed_drop_count": last_observed_drop_count,
        }),
        timeout_ms,
    );
    let response = dispatch_remote_orchestration_request(
        session,
        "source",
        request,
        RemoteDispatchContract {
            dispatch_subject: "probe",
            unreachable_reason: "orchestration_source_session_unreachable",
            transport_failure_reason: "orchestration_source_probe_dispatch_transport_failed",
            protocol_failure_reason: "orchestration_source_probe_dispatch_protocol_failed",
            missing_error_message:
                "remote orchestration probe returned an error without an envelope",
        },
    )
    .await?;

    decode_orchestration_success_payload(
        response,
        session,
        "orchestration_source_probe_payload_missing",
        "orchestration probe returned success without a payload",
        "orchestration_source_probe_payload_invalid",
        "orchestration probe payload",
    )
}
