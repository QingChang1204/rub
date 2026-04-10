use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::OrchestrationAddressInfo;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest};

use crate::orchestration_executor::target::{
    dispatch_to_target_session, ensure_orchestration_target_continuity,
};
use crate::orchestration_probe::evaluate_orchestration_probe_for_tab;
use crate::orchestration_runtime::projected_orchestration_session;
use crate::session::SessionState;
use crate::trigger_workflow_bridge::resolve_trigger_workflow_source_bindings;

use super::DaemonRouter;
use super::request_args::required_string_arg;

mod addressing;
mod command;
mod execution;
mod projection;
mod registry;
mod rule;

use command::OrchestrationCommand;
#[cfg(test)]
use command::OrchestrationIdArgs;

const ORCHESTRATION_ADDRESS_TIMEOUT_MS: u64 = 5_000;

pub(super) async fn cmd_orchestration(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    OrchestrationCommand::parse(args)?
        .execute(router, state)
        .await
}

pub(super) async fn cmd_orchestration_probe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let tab_target_id = required_string_arg(args, "tab_target_id")?;
    let condition_value = args.get("condition").cloned().ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "Missing required argument: 'condition'",
        )
    })?;
    let condition = serde_json::from_value(condition_value).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("_orchestration_probe condition payload is invalid: {error}"),
        )
    })?;
    let after_sequence = args
        .get("after_sequence")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let last_observed_drop_count = args
        .get("last_observed_drop_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let frame_id = args
        .get("frame_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let result = evaluate_orchestration_probe_for_tab(
        &router.browser_port(),
        state,
        &tab_target_id,
        frame_id,
        &condition,
        after_sequence,
        last_observed_drop_count,
    )
    .await?;

    serde_json::to_value(result).map_err(RubError::from)
}

pub(super) async fn cmd_orchestration_workflow_source_vars(
    router: &DaemonRouter,
    args: &serde_json::Value,
    _state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let tab_target_id = required_string_arg(args, "tab_target_id")?;
    let frame_id = args
        .get("frame_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let payload = args
        .get("payload")
        .and_then(|value| value.as_object())
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required argument: 'payload' (expected workflow action payload object)",
            )
        })?;

    let bindings = resolve_trigger_workflow_source_bindings(
        &router.browser_port(),
        &tab_target_id,
        frame_id,
        payload,
    )
    .await?;
    serde_json::to_value(bindings).map_err(RubError::from)
}

pub(super) async fn cmd_orchestration_target_dispatch(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let target: OrchestrationAddressInfo =
        serde_json::from_value(args.get("target").cloned().ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required argument: 'target'",
            )
        })?)
        .map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("_orchestration_target_dispatch target payload is invalid: {error}"),
            )
        })?;
    let request: IpcRequest =
        serde_json::from_value(args.get("request").cloned().ok_or_else(|| {
            RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required argument: 'request'",
            )
        })?)
        .map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("_orchestration_target_dispatch request payload is invalid: {error}"),
            )
        })?;

    if target.session_id != state.session_id {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "Target dispatch request does not belong to this session authority",
            serde_json::json!({
                "reason": "orchestration_target_session_mismatch",
                "requested_session_id": target.session_id,
                "current_session_id": state.session_id,
            }),
        ));
    }

    let current_session = projected_orchestration_session(
        state.session_id.clone(),
        state.session_name.clone(),
        std::process::id(),
        state.socket_path().display().to_string(),
        true,
        IPC_PROTOCOL_VERSION.to_string(),
        router.browser.launch_policy().user_data_dir,
    );
    ensure_orchestration_target_continuity(router, state, &current_session, &target)
        .await
        .map_err(RubError::Domain)?;
    let response = dispatch_to_target_session(router, state, &current_session, request)
        .await
        .map_err(RubError::Domain)?;
    response.data.ok_or_else(|| {
        RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "_orchestration_target_dispatch succeeded without a payload",
            serde_json::json!({
                "reason": "orchestration_target_dispatch_payload_missing",
                "target_session_id": state.session_id,
                "target_session_name": state.session_name,
            }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::execution::{
        ensure_orchestration_addressing_available, ensure_orchestration_execution_available,
    };
    use super::{OrchestrationIdArgs, command::required_u32_arg};
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use rub_core::model::OrchestrationRuntimeInfo;

    #[test]
    fn orchestration_rule_id_rejects_values_larger_than_u32() {
        let error = required_u32_arg(&serde_json::json!({"id": u64::from(u32::MAX) + 1}), "id")
            .expect_err("oversized id should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn typed_orchestration_id_payload_rejects_values_larger_than_u32() {
        let error = parse_json_args::<OrchestrationIdArgs>(
            &serde_json::json!({"sub": "export", "id": u64::from(u32::MAX) + 1}),
            "orchestration export",
        )
        .expect_err("oversized export id should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn degraded_orchestration_runtime_rejects_addressing_and_execution() {
        let runtime = OrchestrationRuntimeInfo {
            addressing_supported: false,
            execution_supported: false,
            degraded_reason: Some("registry_read_failed:boom".to_string()),
            ..OrchestrationRuntimeInfo::default()
        };
        let addressing = ensure_orchestration_addressing_available(&runtime)
            .expect_err("degraded runtime must reject addressing");
        let execution = ensure_orchestration_execution_available(&runtime)
            .expect_err("degraded runtime must reject execution");
        assert_eq!(addressing.into_envelope().code, ErrorCode::SessionBusy);
        assert_eq!(execution.into_envelope().code, ErrorCode::SessionBusy);
    }
}
