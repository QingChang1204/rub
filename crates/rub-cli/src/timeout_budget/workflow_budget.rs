use super::ATOMIC_FILL_ROLLBACK_RESERVE_MS_PER_STEP;
use super::budget::humanize_budget_ms_for_command_args;
use crate::commands::ElementAddressArgs;
use rub_core::error::{ErrorCode, RubError};
use serde_json::Value;

pub(super) fn validate_click_projection_inputs(
    index: Option<u32>,
    target: &ElementAddressArgs,
    xy: Option<&[f64]>,
) -> Result<(), RubError> {
    if xy.is_none() {
        return Ok(());
    }
    let has_locator_target = index.is_some()
        || target.snapshot.is_some()
        || target.element_ref.is_some()
        || target.selector.is_some()
        || target.target_text.is_some()
        || target.role.is_some()
        || target.label.is_some()
        || target.testid.is_some()
        || target.first
        || target.last
        || target.nth.is_some();
    if has_locator_target {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "`click --xy` cannot be combined with index, ref, selector, target-text, role, label, testid, snapshot, or match-selection targeting",
        ));
    }
    Ok(())
}

pub(super) fn validate_scroll_projection_inputs(
    direction: &str,
    y: Option<i32>,
) -> Result<(), RubError> {
    if y.is_some() && !direction.eq_ignore_ascii_case("down") {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "`scroll --y` cannot be combined with an explicit direction argument",
        ));
    }
    Ok(())
}

pub(super) fn fill_workflow_budget_ms(
    resolved_spec: &Value,
    humanize: bool,
    humanize_speed: &str,
    has_submit: bool,
    atomic: bool,
) -> u64 {
    let mut extra = rub_core::automation_timeout::fill_workflow_additional_timeout_ms(
        resolved_spec,
        has_submit,
    );
    let Some(steps) = resolved_spec.as_array() else {
        return humanize_budget_ms_for_command_args(
            "click",
            &serde_json::json!({}),
            humanize && has_submit,
            humanize_speed,
        );
    };

    for step in steps {
        if let Some(value) = step.get("value").and_then(Value::as_str) {
            extra = extra.saturating_add(humanize_budget_ms_for_command_args(
                "type",
                &serde_json::json!({ "text": value }),
                humanize,
                humanize_speed,
            ));
        } else if step
            .get("activate")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            extra = extra.saturating_add(humanize_budget_ms_for_command_args(
                "click",
                &serde_json::json!({}),
                humanize,
                humanize_speed,
            ));
        }
    }

    if has_submit {
        extra = extra.saturating_add(humanize_budget_ms_for_command_args(
            "click",
            &serde_json::json!({}),
            humanize,
            humanize_speed,
        ));
    }

    if atomic {
        extra = extra.saturating_add(atomic_fill_rollback_budget_ms(resolved_spec));
    }

    extra
}

fn atomic_fill_rollback_budget_ms(resolved_spec: &Value) -> u64 {
    let step_count = resolved_spec
        .as_array()
        .map(|steps| steps.len() as u64)
        .filter(|count| *count > 0)
        .unwrap_or(1);
    step_count.saturating_mul(ATOMIC_FILL_ROLLBACK_RESERVE_MS_PER_STEP)
}

pub(super) fn pipe_workflow_budget_ms(
    resolved_spec: &Value,
    humanize: bool,
    humanize_speed: &str,
) -> u64 {
    let mut extra =
        rub_core::automation_timeout::pipe_workflow_additional_timeout_ms(resolved_spec);
    let Some(steps) = resolved_spec
        .as_array()
        .or_else(|| resolved_spec.get("steps").and_then(Value::as_array))
    else {
        return 0;
    };

    for step in steps {
        let command = step
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = step.get("args").unwrap_or(&Value::Null);

        extra = extra.saturating_add(humanize_budget_ms_for_command_args(
            command,
            args,
            humanize,
            humanize_speed,
        ));
        if command == "fill"
            && let Some(fill_spec) = args.get("spec")
        {
            let has_submit = [
                "submit_index",
                "submit_selector",
                "submit_target_text",
                "submit_ref",
                "submit_role",
                "submit_label",
                "submit_testid",
            ]
            .into_iter()
            .any(|key| args.get(key).is_some_and(|value| !value.is_null()));
            extra = extra.saturating_add(fill_workflow_budget_ms(
                fill_spec,
                humanize,
                humanize_speed,
                has_submit,
                args.get("atomic").and_then(Value::as_bool).unwrap_or(false),
            ));
            extra = extra.saturating_sub(
                rub_core::automation_timeout::fill_workflow_additional_timeout_ms(
                    fill_spec, has_submit,
                ),
            );
        }
    }

    extra
}
