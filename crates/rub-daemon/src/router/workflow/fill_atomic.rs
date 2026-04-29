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
use crate::router::timeout_projection::record_mutating_possible_commit_timeout_projection;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::recovery_contract::{
    atomic_fill_rollback_contract, fill_atomic_possible_commit_contract,
};

const ATOMIC_FILL_ROLLBACK_RESERVE_MS_PER_STEP: u64 = 1_000;

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
        record_atomic_fill_possible_commit_timeout_projection(planned, &committed_indices);
        let rollback_target_count = committed_indices.len().saturating_add(1);
        let forward_result: Result<serde_json::Value, (RubError, Option<usize>)> =
            match atomic_fill_step_timeout(deadline, rollback_target_count) {
                Some(timeout) => {
                    match tokio::time::timeout(
                        timeout,
                        execute_named_command_with_fence(
                            router,
                            planned.forward_command,
                            &forward_args,
                            deadline,
                            state,
                        ),
                    )
                    .await
                    {
                        Ok(result) => result.map_err(|error| (error, Some(index))),
                        Err(_) => Err((
                            RubError::domain_with_context(
                                ErrorCode::IpcTimeout,
                                "fill --atomic step exhausted its rollback fence budget after possible commit",
                                serde_json::json!({
                                    "reason": "fill_atomic_step_possible_commit_timeout",
                                    "step_index": index,
                                    "command": planned.forward_command,
                                    "rollback_required": true,
                                }),
                            ),
                            Some(index),
                        )),
                    }
                }
                None => Err((
                    RubError::domain_with_context(
                        ErrorCode::IpcTimeout,
                        "fill --atomic exhausted its timeout budget before starting a rollback-safe step",
                        serde_json::json!({
                            "reason": "fill_atomic_step_timeout_budget_exhausted",
                            "step_index": index,
                            "command": planned.forward_command,
                        }),
                    ),
                    None,
                )),
            };
        match forward_result {
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
            Err((error, failing_step_index)) => {
                let rollback = rollback_atomic_fill_steps(
                    &rollback_ctx,
                    &plan.steps,
                    &committed_indices,
                    failing_step_index,
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

fn atomic_fill_step_timeout(
    deadline: TransactionDeadline,
    rollback_target_count: usize,
) -> Option<std::time::Duration> {
    let remaining = deadline.remaining_duration()?;
    let rollback_reserve_ms = (rollback_target_count.max(1) as u64)
        .saturating_mul(ATOMIC_FILL_ROLLBACK_RESERVE_MS_PER_STEP);
    remaining
        .checked_sub(std::time::Duration::from_millis(rollback_reserve_ms))
        .filter(|timeout| !timeout.is_zero())
}

fn record_atomic_fill_possible_commit_timeout_projection(
    planned: &AtomicFillPlannedStep,
    committed_indices: &[usize],
) {
    record_mutating_possible_commit_timeout_projection(
        planned.forward_command,
        fill_atomic_possible_commit_contract(
            planned.step_index,
            committed_indices,
            planned.rollback_command,
            planned.rollback_class,
        ),
    );
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
    let rollback_targets = atomic_fill_rollback_targets(committed_indices, failing_step_index);

    let mut errors = Vec::new();
    let rollback_target_count = rollback_targets.len();
    for (position, step_index) in rollback_targets.into_iter().enumerate() {
        let planned = &plan_steps[step_index];
        let mut rollback_args = planned.rollback_args.clone();
        inherit_orchestration_metadata(
            &mut rollback_args,
            ctx.orchestration,
            ctx.inheritance_policy,
        );
        let remaining_targets = rollback_target_count.saturating_sub(position);
        let Some(rollback_timeout) =
            atomic_fill_rollback_step_timeout(ctx.deadline, remaining_targets)
        else {
            let envelope = atomic_fill_rollback_timeout_error(
                planned,
                step_index,
                "fill_atomic_rollback_budget_exhausted",
            );
            step_results[step_index] =
                atomic_step_projection(planned, "rollback_failed", None, Some(&envelope));
            errors.push(format!(
                "step {} rollback budget exhausted before execution",
                step_index
            ));
            continue;
        };
        let rollback_deadline = TransactionDeadline::new(duration_ms_u64(rollback_timeout));
        let rollback_result = tokio::time::timeout(
            rollback_timeout,
            execute_named_command_with_fence(
                ctx.router,
                planned.rollback_command,
                &rollback_args,
                rollback_deadline,
                ctx.state,
            ),
        )
        .await;
        match rollback_result {
            Err(_) => {
                let envelope = atomic_fill_rollback_timeout_error(
                    planned,
                    step_index,
                    "fill_atomic_rollback_step_timeout",
                );
                step_results[step_index] =
                    atomic_step_projection(planned, "rollback_failed", None, Some(&envelope));
                errors.push(format!(
                    "step {} rollback timed out after its rollback fence budget",
                    step_index
                ));
            }
            Ok(Ok(data)) => {
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
            Ok(Err(error)) => {
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

fn atomic_fill_rollback_targets(
    committed_indices: &[usize],
    failing_step_index: Option<usize>,
) -> Vec<usize> {
    let mut rollback_targets: Vec<usize> = committed_indices.to_vec();
    if let Some(failing_index) = failing_step_index
        && !rollback_targets.contains(&failing_index)
    {
        rollback_targets.push(failing_index);
    }
    rollback_targets.sort_unstable();
    rollback_targets.reverse();
    rollback_targets
}

fn atomic_fill_rollback_step_timeout(
    deadline: TransactionDeadline,
    remaining_targets: usize,
) -> Option<std::time::Duration> {
    if remaining_targets == 0 {
        return None;
    }
    let remaining = deadline.remaining_duration()?;
    let fair_share = remaining / remaining_targets as u32;
    let per_target_cap = std::time::Duration::from_millis(ATOMIC_FILL_ROLLBACK_RESERVE_MS_PER_STEP);
    Some(fair_share.min(per_target_cap)).filter(|timeout| !timeout.is_zero())
}

fn duration_ms_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX).max(1)
}

fn atomic_fill_rollback_timeout_error(
    planned: &AtomicFillPlannedStep,
    step_index: usize,
    reason: &'static str,
) -> ErrorEnvelope {
    let mut envelope = ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        "fill --atomic rollback exhausted its per-step rollback fence budget",
    );
    envelope.context = Some(serde_json::json!({
        "reason": reason,
        "step_index": step_index,
        "command": planned.rollback_command,
        "rollback_required": true,
    }));
    envelope
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
                "recovery_contract": atomic_fill_rollback_contract(rollback.is_err()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_fill_step_timeout_reserves_rollback_budget_per_target() {
        let single = atomic_fill_step_timeout(TransactionDeadline::new(2_500), 1)
            .expect("single rollback target should leave forward budget");
        assert!(
            single.as_millis() <= 1_500,
            "forward timeout must reserve one rollback budget"
        );

        let two = atomic_fill_step_timeout(TransactionDeadline::new(3_500), 2)
            .expect("two rollback targets should leave forward budget");
        assert!(
            two.as_millis() <= 1_500,
            "forward timeout must reserve committed plus possible rollback budgets"
        );

        assert!(
            atomic_fill_step_timeout(TransactionDeadline::new(2_000), 2).is_none(),
            "forward step must not start when it would consume the rollback reserve"
        );
    }

    #[test]
    fn atomic_fill_rollback_step_timeout_slices_budget_across_remaining_targets() {
        let first = atomic_fill_rollback_step_timeout(TransactionDeadline::new(2_500), 2)
            .expect("first rollback target should receive a step budget");
        assert!(
            first.as_millis() <= ATOMIC_FILL_ROLLBACK_RESERVE_MS_PER_STEP as u128,
            "rollback target must not consume more than its per-step reserve"
        );

        let constrained = atomic_fill_rollback_step_timeout(TransactionDeadline::new(1_500), 2)
            .expect("rollback should still slice constrained remaining budget");
        assert!(
            constrained.as_millis() <= 750,
            "first rollback must leave budget for the remaining target"
        );

        assert!(
            atomic_fill_rollback_step_timeout(TransactionDeadline::new(0), 2).is_none(),
            "rollback must fail closed when no per-step budget remains"
        );
    }

    #[test]
    fn atomic_fill_rollback_targets_exclude_not_started_current_step() {
        assert_eq!(
            atomic_fill_rollback_targets(&[0, 1], None),
            vec![1, 0],
            "budget-exhausted-before-start must only rollback committed steps"
        );
        assert_eq!(
            atomic_fill_rollback_targets(&[0, 1], Some(2)),
            vec![2, 1, 0],
            "possible-commit current step remains a rollback target"
        );
    }

    #[test]
    fn atomic_fill_failure_projects_rollback_recovery_contract() {
        let plan = AtomicFillPlan {
            snapshot_id: "snap-1".to_string(),
            snapshot_requested: true,
            steps: Vec::new(),
        };
        let envelope = atomic_fill_failure_from_source(
            RubError::domain(ErrorCode::WaitTimeout, "forward command timed out"),
            &plan,
            &[],
            Err(vec!["step 0 rollback timed out".to_string()]),
            "fill --atomic failed during live execution and rolled the transaction back",
        )
        .into_envelope();

        let context = envelope.context.expect("atomic failure context");
        assert_eq!(context["transaction"]["status"], "rollback_failed");
        assert_eq!(
            context["transaction"]["recovery_contract"]["kind"],
            "atomic_fill_rollback"
        );
        assert_eq!(
            context["transaction"]["recovery_contract"]["rollback_authority"],
            "fill_atomic"
        );
        assert_eq!(
            context["transaction"]["recovery_contract"]["rollback_failed"],
            true
        );
        assert_eq!(
            context["transaction"]["recovery_contract"]["retry_same_command_safe"],
            false
        );
        assert_eq!(context["rollback_errors"][0], "step 0 rollback timed out");
    }
}
