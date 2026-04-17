use serde_json::{Map, Value, json};
use std::path::Path;

use super::shared::{WorkflowContinuityDescriptor, same_runtime_roles, workflow_continuity_base};

pub(super) fn blocker_workflow_continuity_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    guidance: &Map<String, Value>,
) -> Option<Value> {
    let continuation_kind = guidance.get("continuation_kind")?.as_str()?;
    let summary = guidance.get("summary")?.as_str()?;
    let signal = guidance
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or(continuation_kind);
    let next_command_hints = guidance
        .get("next_command_hints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let runtime_roles = guidance
        .get("runtime_roles")
        .cloned()
        .unwrap_or_else(|| blocker_workflow_runtime_roles(continuation_kind, signal));
    let recommended_runtime = match guidance.get("recommended_runtime")?.as_object() {
        Some(runtime) => {
            let kind = runtime.get("kind").and_then(Value::as_str)?;
            match kind {
                "fresh_rub_home" => json!({
                    "kind": "fresh_rub_home",
                    "rub_home_hint": runtime
                        .get("rub_home_hint")
                        .and_then(Value::as_str)
                        .unwrap_or("<fresh RUB_HOME>"),
                    "session": runtime.get("session").and_then(Value::as_str).unwrap_or("default"),
                    "reason": runtime
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("isolated_runtime_recommended"),
                }),
                "current_runtime" => json!({
                    "kind": "current_runtime",
                    "rub_home": rub_home.display().to_string(),
                    "session": session,
                    "reason": runtime
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("same_runtime_authoritative_followup"),
                }),
                _ => return None,
            }
        }
        None => return None,
    };

    Some(workflow_continuity_base(
        command,
        session,
        rub_home,
        WorkflowContinuityDescriptor {
            continuation_kind,
            signal,
            summary,
            next_command_hints,
            recommended_runtime,
            runtime_roles: Some(runtime_roles),
            authority_observation: None,
        },
    ))
}

fn blocker_workflow_runtime_roles(continuation_kind: &str, signal: &str) -> Value {
    match continuation_kind {
        "fresh_rub_home" => json!({
            "current_runtime": {
                "role": "gated_or_inspection_runtime",
                "summary": "Keep the current runtime for inspection or recovery while the fresh RUB_HOME continues the broader workflow."
            },
            "recommended_runtime": {
                "role": "continuation_runtime",
                "summary": "Use the fresh RUB_HOME as the primary continuation path for the next workflow step."
            }
        }),
        "same_runtime" => match signal {
            "handoff_active" | "overlay_interference" => same_runtime_roles(
                "manual_recovery_runtime",
                "Keep using the current runtime as the recovery surface while you clear the blocker.",
            ),
            "route_stability_transitioning"
            | "loading_present"
            | "skeleton_present"
            | "degraded_runtime" => same_runtime_roles(
                "observation_runtime",
                "Keep using the current runtime as the observation surface while you verify blocker recovery.",
            ),
            _ => same_runtime_roles(
                "active_execution_runtime",
                "Keep using the current runtime as the primary workflow continuation surface.",
            ),
        },
        _ => same_runtime_roles(
            "active_execution_runtime",
            "Keep using the current runtime as the primary workflow continuation surface.",
        ),
    }
}
