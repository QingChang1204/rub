use serde_json::Value;

use crate::DEFAULT_WAIT_AFTER_TIMEOUT_MS;

pub fn wait_after_timeout_ms(timeout_ms: Option<u64>) -> u64 {
    timeout_ms.unwrap_or(DEFAULT_WAIT_AFTER_TIMEOUT_MS)
}

pub fn wait_after_budget_ms_for_args(args: &Value) -> u64 {
    args.get("wait_after")
        .and_then(Value::as_object)
        .filter(|wait_after| !wait_after.is_empty())
        .map(|wait_after| {
            wait_after_timeout_ms(wait_after.get("timeout_ms").and_then(Value::as_u64))
        })
        .unwrap_or(0)
}

pub fn command_additional_timeout_ms(command: &str, args: &Value) -> u64 {
    let mut extra = wait_after_budget_ms_for_args(args);
    match command {
        "wait" => {
            extra = extra.saturating_add(
                args.get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
            );
        }
        "fill" => {
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
            if let Some(fill_spec) = args.get("spec") {
                extra = extra
                    .saturating_add(fill_workflow_additional_timeout_ms(fill_spec, has_submit));
            }
        }
        "pipe" => {
            if let Some(pipe_spec) = args.get("spec") {
                extra = extra.saturating_add(pipe_workflow_additional_timeout_ms(pipe_spec));
            }
        }
        _ => {}
    }
    extra
}

pub fn fill_workflow_additional_timeout_ms(resolved_spec: &Value, _has_submit: bool) -> u64 {
    let has_submit = _has_submit;
    let Some(steps) = resolved_spec.as_array() else {
        return if has_submit {
            DEFAULT_WAIT_AFTER_TIMEOUT_MS
        } else {
            0
        };
    };

    let extra = steps.iter().fold(0u64, |extra, step| {
        extra.saturating_add(wait_after_budget_ms_for_args(step))
    });

    if has_submit {
        extra.saturating_add(DEFAULT_WAIT_AFTER_TIMEOUT_MS)
    } else {
        extra
    }
}

pub fn pipe_workflow_additional_timeout_ms(resolved_spec: &Value) -> u64 {
    let Some(steps) = resolved_spec
        .as_array()
        .or_else(|| resolved_spec.get("steps").and_then(Value::as_array))
    else {
        return 0;
    };

    steps.iter().fold(0u64, |extra, step| {
        let command = step
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = step.get("args").unwrap_or(&Value::Null);
        extra.saturating_add(command_additional_timeout_ms(command, args))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        command_additional_timeout_ms, fill_workflow_additional_timeout_ms,
        pipe_workflow_additional_timeout_ms, wait_after_budget_ms_for_args,
    };

    #[test]
    fn shared_timeout_helper_counts_nested_pipe_wait_costs() {
        let spec = serde_json::json!({
            "steps": [
                {
                    "command": "pipe",
                    "args": {
                        "spec": { "steps": [{ "command": "wait", "args": {"timeout_ms":1200} }] }
                    }
                }
            ]
        });

        let extra = pipe_workflow_additional_timeout_ms(&spec);
        assert_eq!(extra, 1_200);
    }

    #[test]
    fn shared_timeout_helper_counts_legacy_array_form_pipe_specs() {
        let spec = serde_json::json!([
            {
                "command": "wait",
                "args": { "timeout_ms": 900 }
            }
        ]);

        let extra = pipe_workflow_additional_timeout_ms(&spec);
        assert_eq!(extra, 900);
    }

    #[test]
    fn shared_timeout_helper_counts_command_wait_after_and_wait_budget() {
        let args = serde_json::json!({
            "wait_after": { "timeout_ms": 750 },
            "timeout_ms": 900,
        });
        assert_eq!(wait_after_budget_ms_for_args(&args), 750);
        assert_eq!(command_additional_timeout_ms("wait", &args), 1_650);
    }

    #[test]
    fn shared_timeout_helper_counts_fill_step_wait_after_budget() {
        let spec = serde_json::json!([
            { "field": "name", "value": "Alice", "wait_after": { "timeout_ms": 500 } },
            { "field": "email", "value": "a@example.com" }
        ]);
        let extra = fill_workflow_additional_timeout_ms(&spec, false);
        assert_eq!(extra, 500);
    }

    #[test]
    fn shared_timeout_helper_counts_optional_fill_submit_step() {
        let spec = serde_json::json!([
            { "field": "name", "value": "Alice" }
        ]);
        let extra = fill_workflow_additional_timeout_ms(&spec, true);
        assert_eq!(extra, crate::DEFAULT_WAIT_AFTER_TIMEOUT_MS);
    }

    #[test]
    fn shared_timeout_helper_counts_structured_fill_spec_values() {
        let args = serde_json::json!({
            "spec": [
                {
                    "selector": "#email",
                    "value": "alice@example.com",
                    "wait_after": { "timeout_ms": 600 }
                }
            ]
        });

        assert_eq!(command_additional_timeout_ms("fill", &args), 600);
    }

    #[test]
    fn shared_timeout_helper_counts_structured_pipe_spec_values() {
        let args = serde_json::json!({
            "spec": {
                "steps": [
                    {
                        "command": "wait",
                        "args": { "timeout_ms": 1_200 }
                    }
                ]
            }
        });

        assert_eq!(command_additional_timeout_ms("pipe", &args), 1_200);
    }

    #[test]
    fn shared_timeout_helper_counts_legacy_array_form_pipe_values() {
        let args = serde_json::json!({
            "spec": [
                {
                    "command": "wait",
                    "args": { "timeout_ms": 1_050 }
                }
            ]
        });

        assert_eq!(command_additional_timeout_ms("pipe", &args), 1_050);
    }
}
