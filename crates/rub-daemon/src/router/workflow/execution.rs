use super::args::{FillArgs, PipeArgs, submit_args};
use super::command_build::{
    attach_step_wait_after, build_fill_step_command, build_fill_step_locator_args,
};
use super::projection::workflow_step_projection;
use super::spec::{
    build_embedded_orchestration_args, parse_fill_steps, parse_pipe_spec, resolve_step_references,
};
use super::*;
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::dispatch::execute_named_command_with_fence;
use crate::router::request_args::parse_json_args;
use crate::router::secret_resolution::{redact_json_value, redact_rub_error};
use crate::workflow_policy::{workflow_allowed_step_descriptions, workflow_request_allowed};
use rub_core::error::ErrorEnvelope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrchestrationMetadataInheritancePolicy {
    PreserveChildOverrides,
    TriggerAuthoritativeFrame,
}

pub(super) async fn cmd_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_fill_with_policy(
        router,
        args,
        deadline,
        state,
        OrchestrationMetadataInheritancePolicy::PreserveChildOverrides,
    )
    .await
}

pub(super) async fn cmd_trigger_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_fill_with_policy(
        router,
        args,
        deadline,
        state,
        OrchestrationMetadataInheritancePolicy::TriggerAuthoritativeFrame,
    )
    .await
}

async fn cmd_fill_with_policy(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    inheritance_policy: OrchestrationMetadataInheritancePolicy,
) -> Result<serde_json::Value, RubError> {
    let parsed_args: FillArgs = parse_json_args(args, "fill")?;
    let parsed = parse_fill_steps(&parsed_args.spec, &state.rub_home)?;
    let steps = parsed.value;
    let metadata = parsed.metadata;
    let mut results = Vec::with_capacity(steps.len() + 1);

    for (step_index, step) in steps.into_iter().enumerate() {
        let mut locator_args = build_fill_step_locator_args(&step);
        inherit_orchestration_metadata(
            &mut locator_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        let (command, mut command_args) =
            match build_fill_step_command(router, state, deadline, &step, &locator_args).await {
                Ok(result) => result,
                Err(error) => return Err(redact_rub_error(error, &metadata)),
            };
        inherit_orchestration_metadata(
            &mut command_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        if let Some(wait_after) = &step.wait_after {
            attach_step_wait_after(&mut command_args, wait_after);
        }
        let data =
            match execute_named_command_with_fence(router, command, &command_args, deadline, state)
                .await
            {
                Ok(data) => data,
                Err(error) => return Err(redact_rub_error(error, &metadata)),
            };
        if let Err(error) = ensure_committed_automation_result(command, Some(&data)) {
            return Err(redact_rub_error(RubError::Domain(error), &metadata));
        }
        results.push(workflow_step_projection(
            step_index, command, None, None, data,
        ));
    }

    if let Some(mut submit_args) = submit_args(&parsed_args.submit) {
        inherit_orchestration_metadata(
            &mut submit_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        let data =
            match execute_named_command_with_fence(router, "click", &submit_args, deadline, state)
                .await
            {
                Ok(data) => data,
                Err(error) => return Err(redact_rub_error(error, &metadata)),
            };
        if let Err(error) = ensure_committed_automation_result("click", Some(&data)) {
            return Err(redact_rub_error(RubError::Domain(error), &metadata));
        }
        results.push(workflow_step_projection(
            results.len(),
            "click",
            None,
            Some("submit"),
            data,
        ));
    }

    let mut data = serde_json::json!({
        "steps": results,
    });
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

pub(super) async fn cmd_pipe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_pipe_with_policy(
        router,
        args,
        deadline,
        state,
        OrchestrationMetadataInheritancePolicy::PreserveChildOverrides,
    )
    .await
}

pub(super) async fn cmd_trigger_pipe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_pipe_with_policy(
        router,
        args,
        deadline,
        state,
        OrchestrationMetadataInheritancePolicy::TriggerAuthoritativeFrame,
    )
    .await
}

async fn cmd_pipe_with_policy(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    inheritance_policy: OrchestrationMetadataInheritancePolicy,
) -> Result<serde_json::Value, RubError> {
    let parsed_args: PipeArgs = parse_json_args(args, "pipe")?;
    let parsed = parse_pipe_spec(&parsed_args.spec, &state.rub_home)?;
    let steps = parsed.value.steps;
    let orchestrations = parsed.value.orchestrations;
    let metadata = parsed.metadata;
    let mut completed = Vec::with_capacity(steps.len() + orchestrations.len());

    for (index, step) in steps.into_iter().enumerate() {
        let command = step.command.as_str();
        if !workflow_request_allowed(command, &step.args) {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("pipe step command '{command}' is not allowed"),
                serde_json::json!({
                    "step_index": index,
                    "allowed_commands": workflow_allowed_step_descriptions(),
                }),
            ));
        }

        let mut resolved_args = step.args.clone();
        inherit_orchestration_metadata(
            &mut resolved_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        if let Err(error) = resolve_step_references(&mut resolved_args, &completed, index) {
            return Err(pipe_step_error(
                redact_rub_error(error, &metadata),
                index,
                command,
                step.label.as_deref(),
                &completed,
            ));
        }

        let data = match execute_named_command_with_fence(
            router,
            command,
            &resolved_args,
            deadline,
            state,
        )
        .await
        {
            Ok(data) => data,
            Err(error) => {
                return Err(pipe_step_error(
                    redact_rub_error(error, &metadata),
                    index,
                    command,
                    step.label.as_deref(),
                    &completed,
                ));
            }
        };
        if let Err(error) = ensure_committed_automation_result(command, Some(&data)) {
            return Err(pipe_step_error(
                redact_rub_error(RubError::Domain(error), &metadata),
                index,
                command,
                step.label.as_deref(),
                &completed,
            ));
        }
        let mut data = data;
        redact_json_value(&mut data, &metadata);

        completed.push(workflow_step_projection(
            index, command, step.label, None, data,
        ));
    }

    for orchestration in orchestrations {
        let step_index = completed.len();
        let label = orchestration.label.clone();
        let command_args = build_embedded_orchestration_args(
            parsed_args.spec_source.as_ref(),
            &orchestration,
            step_index,
        )?;
        let data = match execute_named_command_with_fence(
            router,
            "orchestration",
            &command_args,
            deadline,
            state,
        )
        .await
        {
            Ok(data) => data,
            Err(error) => {
                return Err(pipe_step_error(
                    redact_rub_error(error, &metadata),
                    step_index,
                    "orchestration",
                    label.as_deref(),
                    &completed,
                ));
            }
        };
        let mut data = data;
        redact_json_value(&mut data, &metadata);
        completed.push(workflow_step_projection(
            step_index,
            "orchestration",
            label,
            None,
            data,
        ));
    }

    let mut data = serde_json::json!({
        "steps": completed,
    });
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

pub(super) fn inherit_orchestration_metadata(
    args: &mut serde_json::Value,
    inherited: Option<&serde_json::Value>,
    policy: OrchestrationMetadataInheritancePolicy,
) {
    let Some(inherited) = inherited.and_then(|value| value.as_object()) else {
        return;
    };
    let Some(object) = args.as_object_mut() else {
        return;
    };
    let orchestration = object
        .entry("_orchestration".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !orchestration.is_object() {
        *orchestration = serde_json::json!({});
    }
    let orchestration_object = orchestration
        .as_object_mut()
        .expect("workflow orchestration metadata must normalize to an object");
    for (key, value) in inherited {
        if orchestration_metadata_is_authoritative(key, policy) {
            orchestration_object.insert(key.clone(), value.clone());
        } else {
            orchestration_object
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

fn orchestration_metadata_is_authoritative(
    key: &str,
    policy: OrchestrationMetadataInheritancePolicy,
) -> bool {
    matches!(
        (key, policy),
        (
            "frame_id",
            OrchestrationMetadataInheritancePolicy::TriggerAuthoritativeFrame
        )
    )
}

fn pipe_step_error(
    error: RubError,
    step_index: usize,
    step_command: &str,
    step_label: Option<&str>,
    completed: &[serde_json::Value],
) -> RubError {
    match error {
        RubError::Domain(mut envelope) => {
            envelope.context = Some(serde_json::json!({
                "step_index": step_index,
                "step_command": step_command,
                "step_label": step_label,
                "completed_steps": completed.len(),
                "previous_context": envelope.context,
            }));
            RubError::Domain(envelope)
        }
        other => RubError::Domain(
            ErrorEnvelope::new(
                ErrorCode::InvalidInput,
                format!(
                    "pipe step {} ('{}') failed: {other}",
                    step_index, step_command
                ),
            )
            .with_context(serde_json::json!({
                "step_index": step_index,
                "step_command": step_command,
                "step_label": step_label,
                "completed_steps": completed.len(),
            })),
        ),
    }
}
