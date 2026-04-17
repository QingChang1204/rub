use super::args::{FillArgs, FillStepSpec, SubmitLocatorArgs, submit_args};
use super::command_build::{
    atomic_fill_write_mode_supported, attach_snapshot_id,
    build_atomic_rollback_command_for_resolved_target, build_fill_step_command_for_resolved_target,
    build_fill_step_locator_args, classify_fill_value_target, project_fill_target_summary,
};
use super::execution::{OrchestrationMetadataInheritancePolicy, inherit_orchestration_metadata};
use super::projection::workflow_error_projection;
use super::*;
use crate::router::addressing::{load_snapshot, resolve_element};
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::dispatch::execute_named_command_with_fence;
use crate::router::request_args::{LocatorParseOptions, parse_canonical_locator};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};

#[derive(Debug, Clone)]
struct AtomicFillPlannedStep {
    step_index: usize,
    forward_command: &'static str,
    forward_args: serde_json::Value,
    rollback_command: &'static str,
    rollback_args: serde_json::Value,
    target: serde_json::Value,
    write_mode: &'static str,
    rollback_class: &'static str,
}

#[derive(Debug, Clone)]
struct AtomicFillPlan {
    snapshot_id: String,
    snapshot_requested: bool,
    steps: Vec<AtomicFillPlannedStep>,
}

struct AtomicRollbackContext<'a> {
    router: &'a DaemonRouter,
    deadline: TransactionDeadline,
    state: &'a Arc<SessionState>,
    orchestration: Option<&'a serde_json::Value>,
    inheritance_policy: OrchestrationMetadataInheritancePolicy,
}

pub(super) async fn execute_atomic_fill(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
    parsed_args: &FillArgs,
    steps: &[FillStepSpec],
    inheritance_policy: OrchestrationMetadataInheritancePolicy,
) -> Result<serde_json::Value, RubError> {
    let plan = preflight_atomic_fill_plan(
        router,
        raw_args,
        state,
        deadline,
        parsed_args,
        steps,
        inheritance_policy,
    )
    .await?;

    let mut step_results: Vec<serde_json::Value> = plan
        .steps
        .iter()
        .map(|step| atomic_step_projection(step, "staged", None, None))
        .collect();
    let mut committed_indices = Vec::with_capacity(plan.steps.len());
    let rollback_ctx = AtomicRollbackContext {
        router,
        deadline,
        state,
        orchestration: parsed_args._orchestration.as_ref(),
        inheritance_policy,
    };

    for (index, planned) in plan.steps.iter().enumerate() {
        let mut forward_args = planned.forward_args.clone();
        inherit_orchestration_metadata(
            &mut forward_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        match execute_named_command_with_fence(
            router,
            planned.forward_command,
            &forward_args,
            deadline,
            state,
        )
        .await
        {
            Ok(data) => {
                if let Err(error) =
                    ensure_committed_automation_result(planned.forward_command, Some(&data))
                {
                    let rollback = rollback_atomic_fill_steps(
                        &rollback_ctx,
                        &plan.steps,
                        &committed_indices,
                        Some(index),
                        &mut step_results,
                    )
                    .await;
                    let source = RubError::Domain(error);
                    return Err(atomic_fill_failure_from_source(
                        source,
                        &plan,
                        &step_results,
                        rollback,
                        "fill --atomic could not confirm a committed write and rolled the transaction back",
                    ));
                }
                step_results[index] =
                    atomic_step_projection(planned, "committed", Some(data), None);
                committed_indices.push(index);
            }
            Err(error) => {
                let rollback = rollback_atomic_fill_steps(
                    &rollback_ctx,
                    &plan.steps,
                    &committed_indices,
                    Some(index),
                    &mut step_results,
                )
                .await;
                return Err(atomic_fill_failure_from_source(
                    error,
                    &plan,
                    &step_results,
                    rollback,
                    "fill --atomic failed during live execution and rolled the transaction back",
                ));
            }
        }
    }

    Ok(serde_json::json!({
        "subject": {
            "kind": "fill_atomic",
            "source": "live_page",
            "snapshot_id": plan.snapshot_id,
            "snapshot_scope": "resolution_only",
        },
        "transaction": {
            "atomic": true,
            "status": "committed",
            "executed_step_count": plan.steps.len(),
            "rollback_attempted": false,
            "snapshot_preflight": atomic_snapshot_projection(&plan),
        },
        "steps": step_results,
    }))
}

async fn preflight_atomic_fill_plan(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    parsed_args: &FillArgs,
    steps: &[FillStepSpec],
    inheritance_policy: OrchestrationMetadataInheritancePolicy,
) -> Result<AtomicFillPlan, RubError> {
    if parsed_args._wait_after.is_some() {
        return Err(atomic_fill_not_run_error(
            "fill --atomic rejects global wait_after in v1",
            serde_json::json!({
                "reason": "global_wait_after_not_supported",
                "transaction": {
                    "atomic": true,
                    "status": "not_run",
                    "staged_step_count": 0,
                },
            }),
        ));
    }

    if submit_args(&parsed_args.submit).is_some() {
        return Err(atomic_fill_not_run_error(
            "fill --atomic rejects submit in v1",
            serde_json::json!({
                "reason": "submit_not_supported",
                "transaction": {
                    "atomic": true,
                    "status": "not_run",
                    "staged_step_count": 0,
                },
            }),
        ));
    }

    let snapshot = load_snapshot(
        router,
        raw_args,
        state,
        deadline,
        atomic_plan_requires_a11y(steps, &parsed_args.submit),
    )
    .await?;

    let mut planned_steps = Vec::with_capacity(steps.len());
    for (step_index, step) in steps.iter().enumerate() {
        if step.wait_after.is_some() {
            return Err(atomic_fill_not_run_error(
                "fill --atomic rejects per-step wait_after in v1",
                serde_json::json!({
                    "reason": "step_wait_after_not_supported",
                    "step_index": step_index,
                    "transaction": {
                        "atomic": true,
                        "status": "not_run",
                        "staged_step_count": planned_steps.len(),
                        "snapshot_preflight": {
                            "requested": parsed_args._snapshot_id.is_some(),
                            "snapshot_id": snapshot.snapshot_id,
                            "scope": "resolution_only",
                        },
                    },
                }),
            ));
        }

        if step.activate.unwrap_or(false) && step.value.is_none() {
            return Err(atomic_fill_not_run_error(
                "fill --atomic rejects activate-style steps in v1",
                serde_json::json!({
                    "reason": "activate_not_supported",
                    "step_index": step_index,
                    "transaction": {
                        "atomic": true,
                        "status": "not_run",
                        "staged_step_count": planned_steps.len(),
                        "snapshot_preflight": {
                            "requested": parsed_args._snapshot_id.is_some(),
                            "snapshot_id": snapshot.snapshot_id,
                            "scope": "resolution_only",
                        },
                    },
                }),
            ));
        }

        let mut locator_args = build_fill_step_locator_args(step);
        attach_snapshot_id(&mut locator_args, Some(&snapshot.snapshot_id));
        inherit_orchestration_metadata(
            &mut locator_args,
            parsed_args._orchestration.as_ref(),
            inheritance_policy,
        );
        let resolved =
            resolve_element(router, &locator_args, state, deadline, "fill --atomic").await?;
        let element = resolved.element;
        let classification = classify_fill_value_target(&element);
        if !classification.supported || !atomic_fill_write_mode_supported(classification.write_mode)
        {
            return Err(atomic_fill_not_run_error(
                "fill --atomic v1 only supports rollbackable input/textarea/select writes",
                serde_json::json!({
                    "reason": if classification.supported {
                        "atomic_v1_write_mode_not_supported"
                    } else {
                        "unsupported_value_target"
                    },
                    "step_index": step_index,
                    "write_mode": classification.write_mode,
                    "rollback_class": classification.rollback_class,
                    "target": project_fill_target_summary(&element),
                    "transaction": {
                        "atomic": true,
                        "status": "not_run",
                        "staged_step_count": planned_steps.len(),
                        "snapshot_preflight": {
                            "requested": parsed_args._snapshot_id.is_some(),
                            "snapshot_id": snapshot.snapshot_id,
                            "scope": "resolution_only",
                        },
                    },
                }),
            ));
        }
        let original_value = router.browser.get_value(&element).await?;
        let (forward_command, forward_args) =
            build_fill_step_command_for_resolved_target(step, &element)?;
        let (rollback_command, rollback_args) = build_atomic_rollback_command_for_resolved_target(
            &element,
            classification.write_mode,
            &original_value,
        )?;
        planned_steps.push(AtomicFillPlannedStep {
            step_index,
            forward_command,
            forward_args,
            rollback_command,
            rollback_args,
            target: project_fill_target_summary(&element),
            write_mode: classification.write_mode,
            rollback_class: classification.rollback_class,
        });
    }

    Ok(AtomicFillPlan {
        snapshot_id: snapshot.snapshot_id.clone(),
        snapshot_requested: parsed_args._snapshot_id.is_some(),
        steps: planned_steps,
    })
}

async fn rollback_atomic_fill_steps(
    ctx: &AtomicRollbackContext<'_>,
    plan_steps: &[AtomicFillPlannedStep],
    committed_indices: &[usize],
    failing_step_index: Option<usize>,
    step_results: &mut [serde_json::Value],
) -> Result<(), Vec<String>> {
    let mut rollback_targets: Vec<usize> = committed_indices.to_vec();
    if let Some(failing_index) = failing_step_index
        && !rollback_targets.contains(&failing_index)
    {
        rollback_targets.push(failing_index);
    }
    rollback_targets.sort_unstable();
    rollback_targets.reverse();

    let mut errors = Vec::new();
    for step_index in rollback_targets {
        let planned = &plan_steps[step_index];
        let mut rollback_args = planned.rollback_args.clone();
        inherit_orchestration_metadata(
            &mut rollback_args,
            ctx.orchestration,
            ctx.inheritance_policy,
        );
        match execute_named_command_with_fence(
            ctx.router,
            planned.rollback_command,
            &rollback_args,
            ctx.deadline,
            ctx.state,
        )
        .await
        {
            Ok(data) => {
                if let Err(error) =
                    ensure_committed_automation_result(planned.rollback_command, Some(&data))
                {
                    let envelope = error;
                    step_results[step_index] =
                        atomic_step_projection(planned, "rollback_failed", None, Some(&envelope));
                    errors.push(format!(
                        "step {} rollback confirmation failed: {}",
                        step_index, envelope.message
                    ));
                } else {
                    step_results[step_index] =
                        atomic_step_projection(planned, "rolled_back", Some(data), None);
                }
            }
            Err(error) => {
                let envelope = error.into_envelope();
                step_results[step_index] =
                    atomic_step_projection(planned, "rollback_failed", None, Some(&envelope));
                errors.push(format!(
                    "step {} rollback execution failed: {}",
                    step_index, envelope.message
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn atomic_step_projection(
    planned: &AtomicFillPlannedStep,
    status: &str,
    result: Option<serde_json::Value>,
    error: Option<&ErrorEnvelope>,
) -> serde_json::Value {
    serde_json::json!({
        "step_index": planned.step_index,
        "status": status,
        "action": {
            "kind": "command",
            "command": planned.forward_command,
        },
        "target": planned.target,
        "write_mode": planned.write_mode,
        "rollback_class": planned.rollback_class,
        "result": result.unwrap_or(serde_json::Value::Null),
        "error": error.map(project_atomic_error).unwrap_or(serde_json::Value::Null),
    })
}

fn project_atomic_error(envelope: &ErrorEnvelope) -> serde_json::Value {
    workflow_error_projection(envelope)
}

fn atomic_snapshot_projection(plan: &AtomicFillPlan) -> serde_json::Value {
    serde_json::json!({
        "requested": plan.snapshot_requested,
        "snapshot_id": plan.snapshot_id,
        "scope": "resolution_only",
    })
}

fn atomic_fill_not_run_error(message: &str, context: serde_json::Value) -> RubError {
    RubError::domain_with_context(ErrorCode::InvalidInput, message, context)
}

fn atomic_fill_failure_from_source(
    error: RubError,
    plan: &AtomicFillPlan,
    step_results: &[serde_json::Value],
    rollback: Result<(), Vec<String>>,
    message: &str,
) -> RubError {
    let envelope = error.into_envelope();
    let mut context = serde_json::Map::from_iter([
        (
            "transaction".to_string(),
            serde_json::json!({
                "atomic": true,
                "status": if rollback.is_ok() { "rolled_back" } else { "rollback_failed" },
                "snapshot_preflight": atomic_snapshot_projection(plan),
                "rollback_attempted": true,
                "rollback_failed": rollback.is_err(),
                "source_error": project_atomic_error(&envelope),
            }),
        ),
        (
            "steps".to_string(),
            serde_json::Value::Array(step_results.to_vec()),
        ),
    ]);
    if let Err(errors) = rollback {
        context.insert("rollback_errors".to_string(), serde_json::json!(errors));
    }
    RubError::domain_with_context(envelope.code, message, serde_json::Value::Object(context))
}

fn atomic_plan_requires_a11y(steps: &[FillStepSpec], submit: &SubmitLocatorArgs) -> bool {
    steps.iter().any(|step| {
        parse_canonical_locator(
            &build_fill_step_locator_args(step),
            LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
        )
        .ok()
        .flatten()
        .map(|locator| locator.requires_a11y_snapshot())
        .unwrap_or(false)
    }) || submit_args(submit)
        .and_then(|value| {
            parse_canonical_locator(&value, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS).ok()
        })
        .flatten()
        .map(|locator| locator.requires_a11y_snapshot())
        .unwrap_or(false)
}
