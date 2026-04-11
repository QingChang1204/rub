use std::sync::Arc;

use rub_core::command::command_metadata as shared_command_metadata;
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

pub(super) async fn cmd_orchestration_tab_frames(
    router: &DaemonRouter,
    args: &serde_json::Value,
    _state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let tab_target_id = required_string_arg(args, "tab_target_id")?;
    let frames = router
        .browser
        .list_frames_for_tab(&tab_target_id)
        .await
        .map_err(|error| {
            RubError::domain_with_context(
                ErrorCode::BrowserCrashed,
                format!(
                    "Unable to inspect orchestration tab frame inventory for '{tab_target_id}': {error}"
                ),
                serde_json::json!({
                    "reason": "orchestration_tab_frames_query_failed",
                    "tab_target_id": tab_target_id,
                }),
            )
        })?;
    Ok(serde_json::json!({
        "result": {
            "items": frames,
        },
    }))
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
    ensure_transport_safe_target_dispatch_request(&request)?;

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

fn ensure_transport_safe_target_dispatch_request(request: &IpcRequest) -> Result<(), RubError> {
    let metadata = shared_command_metadata(request.command.as_str());
    if metadata.internal {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            format!(
                "_orchestration_target_dispatch cannot execute internal command '{}'",
                request.command
            ),
            serde_json::json!({
                "reason": "orchestration_target_dispatch_inner_command_internal",
                "inner_command": request.command,
            }),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::execution::{
        ensure_orchestration_addressing_available, ensure_orchestration_execution_available,
    };
    use super::{
        OrchestrationIdArgs, cmd_orchestration_target_dispatch, command::required_u32_arg,
        ensure_transport_safe_target_dispatch_request,
    };
    use crate::router::DaemonRouter;
    use crate::router::request_args::parse_json_args;
    use crate::session::SessionState;
    use rub_core::error::ErrorCode;
    use rub_core::model::OrchestrationRuntimeInfo;
    use rub_ipc::protocol::IpcRequest;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

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

    #[test]
    fn target_dispatch_rejects_internal_inner_command() {
        let request = IpcRequest::new("_trigger_pipe", serde_json::json!({ "spec": "[]" }), 1_000);

        let error = ensure_transport_safe_target_dispatch_request(&request)
            .expect_err("target dispatch must reject internal inner commands");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason")),
            Some(&serde_json::json!(
                "orchestration_target_dispatch_inner_command_internal"
            ))
        );
    }

    fn test_router() -> DaemonRouter {
        let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
            rub_cdp::browser::BrowserLaunchOptions {
                headless: true,
                ignore_cert_errors: false,
                user_data_dir: None,
                download_dir: None,
                profile_directory: None,
                hide_infobars: true,
                stealth: true,
            },
        ));
        let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
            manager,
            Arc::new(AtomicU64::new(0)),
            rub_cdp::humanize::HumanizeConfig {
                enabled: false,
                speed: rub_cdp::humanize::HumanizeSpeed::Normal,
            },
        ));
        DaemonRouter::new(adapter)
    }

    #[tokio::test]
    async fn orchestration_target_dispatch_fails_closed_before_in_process_dispatch_for_internal_inner_command()
     {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-target-dispatch"),
            None,
        ));
        let error = cmd_orchestration_target_dispatch(
            &router,
            &serde_json::json!({
                "target": {
                    "session_id": "sess-local",
                    "session_name": "default",
                    "tab_target_id": "tab-target",
                },
                "request": {
                    "ipc_protocol_version": rub_ipc::protocol::IPC_PROTOCOL_VERSION,
                    "command": "_trigger_pipe",
                    "args": { "spec": "[]" },
                    "timeout_ms": 1_000,
                }
            }),
            &state,
        )
        .await
        .expect_err("wrapper must reject internal inner command before local dispatch");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("inner_command")),
            Some(&serde_json::json!("_trigger_pipe"))
        );
    }
}
