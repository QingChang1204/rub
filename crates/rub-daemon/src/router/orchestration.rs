use std::sync::Arc;

use rub_core::command::command_metadata as shared_command_metadata;
use rub_core::error::ErrorEnvelope;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::OrchestrationAddressInfo;
use rub_ipc::protocol::{IPC_PROTOCOL_VERSION, IpcRequest};

use crate::orchestration_executor::run_orchestration_future_with_outer_deadline;
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
    deadline: crate::router::TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    OrchestrationCommand::parse(args)?
        .execute(router, deadline, state)
        .await
}

pub(super) async fn cmd_orchestration_probe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: crate::router::TransactionDeadline,
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

    let result = run_orchestration_future_with_outer_deadline(
        Some(deadline),
        || orchestration_probe_timeout_error(state, &tab_target_id, frame_id),
        evaluate_orchestration_probe_for_tab(
            &router.browser_port(),
            state,
            &tab_target_id,
            frame_id,
            &condition,
            after_sequence,
            last_observed_drop_count,
        ),
    )
    .await?;

    serde_json::to_value(result).map_err(RubError::from)
}

pub(super) async fn cmd_orchestration_tab_frames(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: crate::router::TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let tab_target_id = required_string_arg(args, "tab_target_id")?;
    let frames = run_orchestration_future_with_outer_deadline(
        Some(deadline),
        || orchestration_tab_frames_timeout_error(state, &tab_target_id),
        async {
            router
                .browser
                .list_frames_for_tab(&tab_target_id)
                .await
                .map_err(|error| {
                    RubError::Domain(orchestration_degraded_authority_error(
                        "Unable to inspect orchestration tab frame inventory because authoritative frame continuity is currently unavailable",
                        "orchestration_tab_frames_query_failed",
                        serde_json::json!({
                            "tab_target_id": tab_target_id,
                            "cause": error.to_string(),
                        }),
                    ))
                })
        },
    )
    .await?;
    Ok(serde_json::json!({
        "result": {
            "items": frames,
        },
    }))
}

pub(crate) fn orchestration_degraded_authority_error(
    message: impl Into<String>,
    reason: &'static str,
    extra_context: serde_json::Value,
) -> ErrorEnvelope {
    let mut context = serde_json::json!({
        "reason": reason,
    });
    if let (Some(context_object), Some(extra_object)) =
        (context.as_object_mut(), extra_context.as_object())
    {
        for (key, value) in extra_object {
            context_object.insert(key.clone(), value.clone());
        }
    }
    ErrorEnvelope::new(ErrorCode::SessionBusy, message).with_context(context)
}

pub(super) async fn cmd_orchestration_workflow_source_vars(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: crate::router::TransactionDeadline,
    state: &Arc<SessionState>,
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

    let bindings = run_orchestration_future_with_outer_deadline(
        Some(deadline),
        || orchestration_workflow_source_vars_timeout_error(state, &tab_target_id, frame_id),
        resolve_trigger_workflow_source_bindings(
            &router.browser_port(),
            &tab_target_id,
            frame_id,
            payload,
        ),
    )
    .await?;
    serde_json::to_value(bindings).map_err(RubError::from)
}

pub(super) async fn cmd_orchestration_target_dispatch(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: crate::router::TransactionDeadline,
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
    ensure_target_dispatch_request_congruence(&target, &request)?;

    let current_session = projected_orchestration_session(
        state.session_id.clone(),
        state.session_name.clone(),
        std::process::id(),
        state.socket_path().display().to_string(),
        true,
        IPC_PROTOCOL_VERSION.to_string(),
        rub_core::model::OrchestrationSessionAvailability::Addressable,
        router.browser.launch_policy().user_data_dir,
    );
    ensure_orchestration_target_continuity(
        router,
        state,
        &current_session,
        &target,
        Some(deadline),
    )
    .await
    .map_err(RubError::Domain)?;
    let response =
        dispatch_to_target_session(router, state, &current_session, request, Some(deadline))
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

fn orchestration_probe_timeout_error(
    state: &Arc<SessionState>,
    tab_target_id: &str,
    frame_id: Option<&str>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcTimeout,
        "Orchestration probe exhausted the caller-owned timeout budget before authoritative evaluation completed",
        serde_json::json!({
            "reason": "orchestration_probe_timeout_budget_exhausted",
            "session_id": state.session_id,
            "session_name": state.session_name,
            "tab_target_id": tab_target_id,
            "frame_id": frame_id,
        }),
    )
}

fn orchestration_tab_frames_timeout_error(
    state: &Arc<SessionState>,
    tab_target_id: &str,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcTimeout,
        "Orchestration tab frame inventory query exhausted the caller-owned timeout budget",
        serde_json::json!({
            "reason": "orchestration_tab_frames_timeout_budget_exhausted",
            "session_id": state.session_id,
            "session_name": state.session_name,
            "tab_target_id": tab_target_id,
        }),
    )
}

fn orchestration_workflow_source_vars_timeout_error(
    state: &Arc<SessionState>,
    tab_target_id: &str,
    frame_id: Option<&str>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::IpcTimeout,
        "Orchestration workflow source_vars exhausted the caller-owned timeout budget before authoritative source reads completed",
        serde_json::json!({
            "reason": "orchestration_workflow_source_vars_timeout_budget_exhausted",
            "session_id": state.session_id,
            "session_name": state.session_name,
            "tab_target_id": tab_target_id,
            "frame_id": frame_id,
        }),
    )
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

fn ensure_target_dispatch_request_congruence(
    target: &OrchestrationAddressInfo,
    request: &IpcRequest,
) -> Result<(), RubError> {
    let Some(orchestration) = request
        .args
        .get("_orchestration")
        .and_then(|value| value.as_object())
    else {
        return Err(RubError::domain_with_context(
            ErrorCode::IpcProtocolError,
            "_orchestration_target_dispatch requires inner request _orchestration metadata",
            serde_json::json!({
                "reason": "orchestration_target_dispatch_inner_metadata_missing",
                "inner_command": request.command,
            }),
        ));
    };
    ensure_target_dispatch_metadata_matches(
        "target_session_id",
        orchestration
            .get("target_session_id")
            .and_then(|value| value.as_str()),
        Some(target.session_id.as_str()),
        "orchestration_target_dispatch_inner_target_session_mismatch",
        request,
    )?;
    ensure_target_dispatch_metadata_matches(
        "target_tab_target_id",
        orchestration
            .get("target_tab_target_id")
            .and_then(|value| value.as_str()),
        target.tab_target_id.as_deref(),
        "orchestration_target_dispatch_inner_target_tab_mismatch",
        request,
    )?;
    ensure_target_dispatch_metadata_matches(
        "frame_id",
        orchestration
            .get("frame_id")
            .and_then(|value| value.as_str()),
        target.frame_id.as_deref(),
        "orchestration_target_dispatch_inner_target_frame_mismatch",
        request,
    )?;
    Ok(())
}

fn ensure_target_dispatch_metadata_matches(
    field: &'static str,
    actual: Option<&str>,
    expected: Option<&str>,
    reason: &'static str,
    request: &IpcRequest,
) -> Result<(), RubError> {
    if actual == expected {
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::IpcProtocolError,
        format!(
            "_orchestration_target_dispatch inner request {field} does not match wrapper target authority"
        ),
        serde_json::json!({
            "reason": reason,
            "inner_command": request.command,
            "field": field,
            "expected": expected,
            "actual": actual,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::execution::{
        ensure_orchestration_addressing_available, ensure_orchestration_execution_available,
    };
    use super::{
        OrchestrationIdArgs, cmd_orchestration_probe, cmd_orchestration_tab_frames,
        cmd_orchestration_target_dispatch, cmd_orchestration_workflow_source_vars,
        command::required_u32_arg, ensure_transport_safe_target_dispatch_request,
    };
    use crate::router::request_args::parse_json_args;
    use crate::router::{DaemonRouter, TransactionDeadline};
    use crate::session::SessionState;
    use rub_core::error::ErrorCode;
    use rub_core::model::OrchestrationRuntimeInfo;
    use rub_ipc::protocol::IpcRequest;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

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
                managed_profile_ephemeral: false,
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

    fn expired_deadline() -> TransactionDeadline {
        let deadline = TransactionDeadline::new(1);
        std::thread::sleep(Duration::from_millis(5));
        deadline
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
                    "command_id": "orch-inner-cmd-1",
                    "args": { "spec": "[]" },
                    "timeout_ms": 1_000,
                }
            }),
            TransactionDeadline::new(1_000),
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

    #[tokio::test]
    async fn orchestration_probe_fails_closed_when_deadline_is_exhausted_before_local_probe() {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-probe-timeout"),
            None,
        ));

        let error = cmd_orchestration_probe(
            &router,
            &serde_json::json!({
                "tab_target_id": "tab-target",
                "condition": {
                    "kind": "text_present",
                    "text": "ready"
                }
            }),
            expired_deadline(),
            &state,
        )
        .await
        .expect_err("expired deadline should fail closed before local probe");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_probe_timeout_budget_exhausted")
        );
    }

    #[tokio::test]
    async fn orchestration_workflow_source_vars_fails_closed_when_deadline_is_exhausted_before_local_reads()
     {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-source-vars-timeout"),
            None,
        ));

        let error = cmd_orchestration_workflow_source_vars(
            &router,
            &serde_json::json!({
                "tab_target_id": "tab-target",
                "payload": {
                    "source_vars": {
                        "greeting": {
                            "kind": "text",
                            "selector": "#hero"
                        }
                    }
                }
            }),
            expired_deadline(),
            &state,
        )
        .await
        .expect_err("expired deadline should fail closed before local source reads");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_workflow_source_vars_timeout_budget_exhausted")
        );
    }

    #[tokio::test]
    async fn orchestration_tab_frames_fails_closed_when_deadline_is_exhausted_before_local_inventory()
     {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-tab-frames-timeout"),
            None,
        ));

        let error = cmd_orchestration_tab_frames(
            &router,
            &serde_json::json!({
                "tab_target_id": "tab-target",
            }),
            expired_deadline(),
            &state,
        )
        .await
        .expect_err("expired deadline should fail closed before local frame inventory");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_tab_frames_timeout_budget_exhausted")
        );
    }

    #[test]
    fn orchestration_degraded_authority_error_uses_shared_session_busy_family() {
        let envelope = super::orchestration_degraded_authority_error(
            "frame continuity unavailable",
            "orchestration_tab_frames_query_failed",
            serde_json::json!({
                "tab_target_id": "tab-1",
            }),
        );

        assert_eq!(envelope.code, ErrorCode::SessionBusy);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_tab_frames_query_failed")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("tab_target_id"))
                .and_then(|value| value.as_str()),
            Some("tab-1")
        );
    }

    #[tokio::test]
    async fn orchestration_target_dispatch_fails_closed_when_deadline_is_exhausted_before_local_continuity()
     {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-target-dispatch-timeout"),
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
                    "command": "tabs",
                    "command_id": "orch-inner-tabs-1",
                    "args": {
                        "_orchestration": {
                            "target_session_id": "sess-local",
                            "target_tab_target_id": "tab-target",
                            "frame_id": null,
                        }
                    },
                    "timeout_ms": 1_000,
                }
            }),
            expired_deadline(),
            &state,
        )
        .await
        .expect_err("expired deadline should fail closed before local target continuity");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcTimeout);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_target_timeout_budget_exhausted")
        );
    }

    #[tokio::test]
    async fn orchestration_target_dispatch_fails_closed_when_inner_target_tab_does_not_match_wrapper()
     {
        let router = test_router();
        let state = Arc::new(SessionState::new_with_id(
            "default",
            "sess-local",
            PathBuf::from("/tmp/rub-orchestration-target-dispatch-mismatch"),
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
                    "command": "tabs",
                    "command_id": "orch-inner-tabs-1",
                    "args": {
                        "_orchestration": {
                            "target_session_id": "sess-local",
                            "target_tab_target_id": "tab-other",
                            "frame_id": null,
                        }
                    },
                    "timeout_ms": 1_000,
                }
            }),
            TransactionDeadline::new(1_000),
            &state,
        )
        .await
        .expect_err("mismatched inner target metadata must fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(|value| value.as_str()),
            Some("orchestration_target_dispatch_inner_target_tab_mismatch")
        );
    }
}
