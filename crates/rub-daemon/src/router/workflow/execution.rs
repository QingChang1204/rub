use super::args::{FillArgs, submit_args};
use super::command_build::{
    attach_snapshot_id, attach_step_wait_after, build_fill_step_command,
    build_fill_step_command_for_resolved_target, build_fill_step_locator_args,
    build_submit_command_for_resolved_target,
};
use super::fill_atomic::execute_atomic_fill;
use super::pipe_execution::cmd_pipe_with_policy;
use super::projection::workflow_step_projection;
use super::spec::parse_fill_steps;
use super::*;
use crate::router::addressing::resolve_element;
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::dispatch::execute_named_command_with_fence;
use crate::router::request_args::parse_json_args;
use crate::router::secret_resolution::{
    attach_secret_resolution_projection, redact_json_value, redact_rub_error,
};
use rub_core::error::RubError;

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
    if parsed_args.atomic {
        let mut data = execute_atomic_fill(
            router,
            args,
            deadline,
            state,
            &parsed_args,
            &steps,
            inheritance_policy,
        )
        .await
        .map_err(|error| redact_rub_error(error, &metadata))?;
        attach_secret_resolution_projection(&mut data, &metadata);
        redact_json_value(&mut data, &metadata);
        return Ok(data);
    }
    let mut results = Vec::with_capacity(steps.len() + 1);
    let snapshot_plan = if let Some(snapshot_id) = parsed_args._snapshot_id.as_deref() {
        Some(
            preflight_fill_snapshot_plan(
                router,
                state,
                deadline,
                snapshot_id,
                &steps,
                &parsed_args,
                inheritance_policy,
            )
            .await
            .map_err(|error| redact_rub_error(error, &metadata))?,
        )
    } else {
        None
    };

    for (step_index, step) in steps.iter().enumerate() {
        let (command, mut command_args) = if let Some(plan) = &snapshot_plan {
            let planned = &plan.steps[step_index];
            (planned.command, planned.args.clone())
        } else {
            let mut locator_args = build_fill_step_locator_args(step);
            inherit_orchestration_metadata(
                &mut locator_args,
                parsed_args._orchestration.as_ref(),
                inheritance_policy,
            );
            match build_fill_step_command(router, state, deadline, step, &locator_args).await {
                Ok(result) => result,
                Err(error) => return Err(redact_rub_error(error, &metadata)),
            }
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

    if let Some(mut submit_args) = if let Some(plan) = &snapshot_plan {
        plan.submit.clone()
    } else {
        submit_args(&parsed_args.submit)
    } {
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
    if let Some(snapshot_id) = parsed_args._snapshot_id.as_deref() {
        data["snapshot_preflight"] = serde_json::json!({
            "requested": true,
            "snapshot_id": snapshot_id,
            "scope": "resolution_only",
        });
    }
    attach_secret_resolution_projection(&mut data, &metadata);
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

#[derive(Debug, Clone)]
struct PlannedFillCommand {
    command: &'static str,
    args: serde_json::Value,
}

#[derive(Debug, Clone)]
struct FillSnapshotPlan {
    steps: Vec<PlannedFillCommand>,
    submit: Option<serde_json::Value>,
}
async fn preflight_fill_snapshot_plan(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    snapshot_id: &str,
    steps: &[super::args::FillStepSpec],
    parsed_args: &FillArgs,
    inheritance_policy: OrchestrationMetadataInheritancePolicy,
) -> Result<FillSnapshotPlan, RubError> {
    let mut planned_steps = Vec::with_capacity(steps.len());
    for step in steps {
        let mut locator_args = build_fill_step_locator_args(step);
        attach_snapshot_id(&mut locator_args, Some(snapshot_id));
        inherit_orchestration_metadata(
            &mut locator_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        let resolved = resolve_element(router, &locator_args, state, deadline, "fill").await?;
        let (command, args) = build_fill_step_command_for_resolved_target(step, &resolved.element)?;
        planned_steps.push(PlannedFillCommand { command, args });
    }

    let submit = if let Some(mut submit_args) = submit_args(&parsed_args.submit) {
        attach_snapshot_id(&mut submit_args, Some(snapshot_id));
        inherit_orchestration_metadata(
            &mut submit_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        let resolved =
            resolve_element(router, &submit_args, state, deadline, "fill submit").await?;
        Some(build_submit_command_for_resolved_target(&resolved.element)?)
    } else {
        None
    };

    Ok(FillSnapshotPlan {
        steps: planned_steps,
        submit,
    })
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
