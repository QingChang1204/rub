use rub_core::error::ErrorEnvelope;
use rub_core::model::{OrchestrationSessionInfo, TriggerConditionSpec};
use rub_ipc::protocol::IpcRequest;

use super::OrchestrationProbeResult;
use crate::orchestration_executor::{
    RemoteDispatchContract, decode_orchestration_success_payload,
    dispatch_remote_orchestration_request,
};

pub(crate) async fn dispatch_remote_orchestration_probe(
    session: &OrchestrationSessionInfo,
    tab_target_id: &str,
    frame_id: Option<&str>,
    condition: &TriggerConditionSpec,
    after_sequence: u64,
    last_observed_drop_count: u64,
) -> Result<OrchestrationProbeResult, ErrorEnvelope> {
    let response = dispatch_remote_orchestration_request(
        session,
        "source",
        IpcRequest::new(
            "_orchestration_probe",
            serde_json::json!({
                "tab_target_id": tab_target_id,
                "frame_id": frame_id,
                "condition": condition,
                "after_sequence": after_sequence,
                "last_observed_drop_count": last_observed_drop_count,
            }),
            30_000,
        ),
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
