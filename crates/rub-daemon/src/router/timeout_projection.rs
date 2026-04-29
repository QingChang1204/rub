use std::sync::{Arc, Mutex as StdMutex};

use tokio::task_local;

use rub_core::command::{TimeoutRecoverySurface, command_metadata};
use rub_core::model::OrchestrationStepResultInfo;
use rub_core::recovery_contract::{
    command_possible_commit_contract, orchestration_partial_commit_steps_contract,
    partial_commit_steps_contract, registry_commit_contract,
};

task_local! {
    static ACTIVE_TIMEOUT_PROJECTION: Option<Arc<ExecutionTimeoutProjectionRecorder>>;
}

#[derive(Default)]
pub(crate) struct ExecutionTimeoutProjectionRecorder {
    aggregate: StdMutex<Option<serde_json::Value>>,
    nested_aggregate: StdMutex<Option<serde_json::Value>>,
    in_flight_step: StdMutex<Option<serde_json::Value>>,
}

impl ExecutionTimeoutProjectionRecorder {
    pub(crate) fn snapshot(&self) -> Option<serde_json::Value> {
        let aggregate = self.aggregate.lock().ok().and_then(|guard| guard.clone());
        let nested_aggregate = self
            .nested_aggregate
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let in_flight_step = self
            .in_flight_step
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        match (aggregate, nested_aggregate, in_flight_step) {
            (
                Some(serde_json::Value::Object(mut aggregate)),
                Some(nested_aggregate),
                in_flight_step,
            ) => {
                aggregate.insert(
                    "nested_owner_projection".to_string(),
                    merge_nested_timeout_projection(Some(nested_aggregate), in_flight_step)
                        .expect("nested aggregate should produce a projection"),
                );
                Some(serde_json::Value::Object(aggregate))
            }
            (Some(serde_json::Value::Object(mut aggregate)), None, Some(in_flight_step)) => {
                aggregate.insert("in_flight_step_projection".to_string(), in_flight_step);
                Some(serde_json::Value::Object(aggregate))
            }
            (Some(aggregate), None, None) => Some(aggregate),
            (Some(other), Some(nested_aggregate), in_flight_step) => Some(serde_json::json!({
                "previous_context": other,
                "nested_owner_projection": merge_nested_timeout_projection(Some(nested_aggregate), in_flight_step),
            })),
            (Some(other), None, Some(in_flight_step)) => Some(serde_json::json!({
                "previous_context": other,
                "in_flight_step_projection": in_flight_step,
            })),
            (None, nested_aggregate, in_flight_step) => {
                merge_nested_timeout_projection(nested_aggregate, in_flight_step)
            }
        }
    }

    fn record_aggregate(&self, projection: serde_json::Value) {
        let incoming_subject = projection_subject_kind(&projection);
        let mut aggregate_guard = match self.aggregate.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let mut nested_guard = match self.nested_aggregate.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        let replace_outer = match aggregate_guard.as_ref().and_then(projection_subject_kind) {
            None => true,
            Some(subject) => Some(subject) == incoming_subject,
        };
        if replace_outer {
            *aggregate_guard = Some(projection);
            *nested_guard = None;
        } else {
            *nested_guard = Some(projection);
        }
        if let Ok(mut guard) = self.in_flight_step.lock() {
            *guard = None;
        }
    }

    fn record_in_flight_step(&self, projection: serde_json::Value) {
        if let Ok(mut guard) = self.in_flight_step.lock() {
            *guard = Some(projection);
        }
    }
}

fn merge_nested_timeout_projection(
    nested_aggregate: Option<serde_json::Value>,
    in_flight_step: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (nested_aggregate, in_flight_step) {
        (Some(serde_json::Value::Object(mut nested)), Some(in_flight_step)) => {
            nested.insert("in_flight_step_projection".to_string(), in_flight_step);
            Some(serde_json::Value::Object(nested))
        }
        (Some(other), Some(in_flight_step)) => Some(serde_json::json!({
            "previous_context": other,
            "in_flight_step_projection": in_flight_step,
        })),
        (Some(nested), None) => Some(nested),
        (None, Some(in_flight_step)) => Some(in_flight_step),
        (None, None) => None,
    }
}

fn projection_subject_kind(projection: &serde_json::Value) -> Option<&str> {
    projection
        .get("subject")
        .and_then(|value| value.get("kind"))
        .and_then(|value| value.as_str())
}

pub(crate) async fn scope_timeout_projection<F, T>(
    recorder: Arc<ExecutionTimeoutProjectionRecorder>,
    future: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    ACTIVE_TIMEOUT_PROJECTION
        .scope(Some(recorder), future)
        .await
}

fn current_timeout_projection_recorder() -> Option<Arc<ExecutionTimeoutProjectionRecorder>> {
    ACTIVE_TIMEOUT_PROJECTION
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

pub(crate) fn record_workflow_partial_commit_timeout_projection(
    subject_kind: &'static str,
    atomic: bool,
    committed_steps: &[serde_json::Value],
) {
    record_workflow_pending_step_timeout_projection(
        subject_kind,
        atomic,
        committed_steps,
        committed_steps.len(),
    );
}

pub(crate) fn record_workflow_pending_step_timeout_projection(
    subject_kind: &'static str,
    atomic: bool,
    committed_steps: &[serde_json::Value],
    failed_step_index: usize,
) {
    let Some(recorder) = current_timeout_projection_recorder() else {
        return;
    };
    recorder.record_aggregate(workflow_partial_commit_timeout_projection(
        subject_kind,
        atomic,
        committed_steps,
        failed_step_index,
    ));
}

fn workflow_partial_commit_timeout_projection(
    subject_kind: &'static str,
    atomic: bool,
    committed_steps: &[serde_json::Value],
    failed_step_index: usize,
) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": subject_kind,
            "source": "live_execution",
        },
        "transaction": {
            "atomic": atomic,
            "status": "timed_out",
            "failure_class": "outer_timeout_after_partial_commit",
            "failed_step_index": failed_step_index,
            "committed_step_count": committed_steps.len(),
            "rollback_attempted": false,
            "rollback_failed": false,
            "recovery_contract": partial_commit_steps_contract(),
        },
        "steps": committed_steps,
    })
}

pub(crate) fn record_orchestration_partial_commit_timeout_projection(
    rule_id: u32,
    total_steps: u32,
    committed_steps: &[OrchestrationStepResultInfo],
) {
    record_orchestration_pending_step_timeout_projection(
        rule_id,
        total_steps,
        committed_steps,
        committed_steps.len() as u32,
    );
}

pub(crate) fn record_orchestration_pending_step_timeout_projection(
    rule_id: u32,
    total_steps: u32,
    committed_steps: &[OrchestrationStepResultInfo],
    failed_step_index: u32,
) {
    record_orchestration_pending_step_timeout_projection_with_recovery(
        rule_id,
        total_steps,
        committed_steps,
        failed_step_index,
        None,
    );
}

pub(crate) fn record_orchestration_pending_step_timeout_projection_with_recovery(
    rule_id: u32,
    total_steps: u32,
    committed_steps: &[OrchestrationStepResultInfo],
    failed_step_index: u32,
    possible_commit_recovery: Option<serde_json::Value>,
) {
    let Some(recorder) = current_timeout_projection_recorder() else {
        return;
    };
    recorder.record_aggregate(orchestration_partial_commit_timeout_projection(
        rule_id,
        total_steps,
        committed_steps,
        failed_step_index,
        possible_commit_recovery,
    ));
}

fn orchestration_partial_commit_timeout_projection(
    rule_id: u32,
    total_steps: u32,
    committed_steps: &[OrchestrationStepResultInfo],
    failed_step_index: u32,
    possible_commit_recovery: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "subject": {
            "kind": "orchestration",
            "source": "live_execution",
        },
        "execution": {
            "status": "timed_out",
            "rule_id": rule_id,
            "failed_step_index": failed_step_index,
            "summary": format!(
                "orchestration rule {rule_id} timed out while action {} was in flight after committing {}/{} action(s)",
                failed_step_index + 1,
                committed_steps.len(),
                total_steps
            ),
            "committed_steps": committed_steps.len(),
            "total_steps": total_steps,
            "steps": committed_steps,
            "recovery_contract": orchestration_partial_commit_steps_contract(possible_commit_recovery),
        },
    })
}

pub(crate) fn post_wait_partial_commit_timeout_projection(
    command: &str,
    committed_projection: serde_json::Value,
    dom_epoch: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "reason": "post_wait_failed_after_partial_commit",
        "partial_commit": {
            "kind": "post_wait_after_commit",
            "command": command,
            "dom_epoch": dom_epoch,
            "committed_projection_authoritative": true,
            "recovery_contract": partial_commit_steps_contract(),
        },
        "committed_response_projection": committed_projection,
    })
}

pub(crate) fn record_post_wait_partial_commit_timeout_projection(
    command: &str,
    committed_projection: serde_json::Value,
    dom_epoch: Option<u64>,
) {
    let Some(recorder) = current_timeout_projection_recorder() else {
        return;
    };
    recorder.record_in_flight_step(post_wait_partial_commit_timeout_projection(
        command,
        committed_projection,
        dom_epoch,
    ));
}

pub(crate) fn record_mutating_possible_commit_timeout_projection(
    command: &str,
    recovery: serde_json::Value,
) {
    let Some(recorder) = current_timeout_projection_recorder() else {
        return;
    };
    recorder.record_in_flight_step(serde_json::json!({
        "reason": "mutating_command_possible_commit",
        "partial_commit": {
            "kind": "possible_commit",
            "command": command,
            "effect_commit_state": "possible_commit",
            "projection_authoritative": false,
            "recovery_contract": recovery,
        },
    }));
}

pub(crate) fn record_registry_control_commit_timeout_projection(
    subject_kind: &'static str,
    operation: &'static str,
    entity_key: &'static str,
    committed_entity: serde_json::Value,
) {
    let Some(recorder) = current_timeout_projection_recorder() else {
        return;
    };
    let mut committed = serde_json::Map::new();
    committed.insert(entity_key.to_string(), committed_entity);
    recorder.record_aggregate(serde_json::json!({
        "subject": {
            "kind": subject_kind,
            "source": "registry_control",
        },
        "transaction": {
            "status": "committed",
            "failure_class": "outer_timeout_after_registry_commit",
            "operation": operation,
            "commit_authority": "session_state_registry",
            "projection_authoritative": true,
            "recovery_contract": registry_commit_contract(),
        },
        "committed": serde_json::Value::Object(committed),
    }));
}

pub(crate) fn record_effectful_command_possible_commit_timeout_projection(
    command: &str,
    command_id: Option<&str>,
) {
    if !command_has_effectful_timeout_surface(command) {
        return;
    }
    record_mutating_possible_commit_timeout_projection(
        command,
        command_possible_commit_contract(command, command_id),
    );
}

fn command_has_effectful_timeout_surface(command: &str) -> bool {
    matches!(
        command_metadata(command).timeout_recovery_surface,
        TimeoutRecoverySurface::PossibleCommit
    )
}

pub(crate) fn merge_timeout_projection_context(
    base: Option<serde_json::Value>,
    extra: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let Some(extra) = extra else {
        return base;
    };

    let mut object = match base {
        Some(serde_json::Value::Object(existing)) => existing,
        Some(other) => {
            let mut object = serde_json::Map::new();
            object.insert("previous_context".to_string(), other);
            object
        }
        None => serde_json::Map::new(),
    };

    if let serde_json::Value::Object(extra_object) = extra {
        for (key, value) in extra_object {
            object.insert(key, value);
        }
    }

    Some(serde_json::Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutionTimeoutProjectionRecorder, command_has_effectful_timeout_surface,
        merge_timeout_projection_context, post_wait_partial_commit_timeout_projection,
        record_effectful_command_possible_commit_timeout_projection,
        record_mutating_possible_commit_timeout_projection,
        record_orchestration_partial_commit_timeout_projection,
        record_orchestration_pending_step_timeout_projection,
        record_post_wait_partial_commit_timeout_projection,
        record_registry_control_commit_timeout_projection,
        record_workflow_partial_commit_timeout_projection,
        record_workflow_pending_step_timeout_projection, scope_timeout_projection,
    };
    use rub_core::command::CommandName;
    use rub_core::model::{OrchestrationStepResultInfo, OrchestrationStepStatus};
    use std::sync::Arc;

    #[test]
    fn effectful_timeout_surface_matches_command_manifest_contract() {
        let effectful = [
            "open",
            "back",
            "forward",
            "reload",
            "scroll",
            "switch",
            "close-tab",
            "click",
            "exec",
            "keys",
            "type",
            "hover",
            "upload",
            "select",
            "fill",
            "pipe",
            "dialog",
            "cookies",
            "intercept",
            "storage",
            "orchestration",
            "trigger",
            "_trigger_fill",
            "_trigger_pipe",
            "_orchestration_target_dispatch",
        ];

        for command in effectful {
            assert!(
                command_has_effectful_timeout_surface(command),
                "{command} should expose possible-commit timeout recovery"
            );
        }

        for command in CommandName::ALL {
            let wire = command.as_str();
            assert_eq!(
                command_has_effectful_timeout_surface(wire),
                effectful.contains(&wire),
                "{wire} timeout surface drifted from command manifest policy"
            );
        }
    }

    #[tokio::test]
    async fn workflow_partial_commit_projection_is_recorded_for_timeout_surface() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        let committed = serde_json::json!({
            "step_index": 0,
            "status": "committed",
            "action": {"kind": "command", "command": "wait"},
            "result": {"matched": true},
        });
        scope_timeout_projection(recorder.clone(), async {
            record_workflow_partial_commit_timeout_projection("pipe", false, &[committed]);
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "pipe");
        assert_eq!(
            projection["transaction"]["failure_class"],
            "outer_timeout_after_partial_commit"
        );
        assert_eq!(
            projection["transaction"]["recovery_contract"]["kind"],
            "partial_commit"
        );
        assert_eq!(projection["steps"][0]["status"], "committed");
    }

    #[tokio::test]
    async fn orchestration_partial_commit_projection_is_recorded_for_timeout_surface() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        let committed_steps = vec![OrchestrationStepResultInfo {
            step_index: 0,
            status: OrchestrationStepStatus::Committed,
            summary: "step 1 committed".to_string(),
            attempts: 1,
            action: None,
            result: Some(serde_json::json!({"matched": true})),
            error_code: None,
            reason: None,
            error_context: None,
        }];

        scope_timeout_projection(recorder.clone(), async {
            record_orchestration_partial_commit_timeout_projection(7, 2, &committed_steps);
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "orchestration");
        assert_eq!(projection["execution"]["status"], "timed_out");
        assert_eq!(projection["execution"]["rule_id"], 7);
        assert_eq!(projection["execution"]["committed_steps"], 1);
        assert_eq!(
            projection["execution"]["recovery_contract"]["kind"],
            "partial_commit"
        );
    }

    #[test]
    fn timeout_projection_merges_into_existing_timeout_context() {
        let merged = merge_timeout_projection_context(
            Some(serde_json::json!({
                "command": "pipe",
                "phase": "execution",
            })),
            Some(serde_json::json!({
                "transaction": {
                    "status": "timed_out",
                },
            })),
        )
        .expect("merged context");

        assert_eq!(merged["command"], "pipe");
        assert_eq!(merged["phase"], "execution");
        assert_eq!(merged["transaction"]["status"], "timed_out");
    }

    #[test]
    fn post_wait_partial_commit_projection_keeps_committed_response_truth() {
        let projection = post_wait_partial_commit_timeout_projection(
            "click",
            serde_json::json!({
                "interaction": {
                    "semantic_class": "click",
                }
            }),
            Some(9),
        );

        assert_eq!(
            projection["reason"],
            "post_wait_failed_after_partial_commit"
        );
        assert_eq!(
            projection["partial_commit"]["kind"],
            "post_wait_after_commit"
        );
        assert_eq!(
            projection["partial_commit"]["recovery_contract"]["kind"],
            "partial_commit"
        );
        assert_eq!(
            projection["committed_response_projection"]["interaction"]["semantic_class"],
            "click"
        );
    }

    #[tokio::test]
    async fn snapshot_preserves_owner_projection_and_in_flight_step_projection() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        let committed = serde_json::json!({
            "step_index": 0,
            "status": "committed",
            "action": {"kind": "command", "command": "wait"},
            "result": {"matched": true},
        });

        scope_timeout_projection(recorder.clone(), async {
            record_workflow_partial_commit_timeout_projection("pipe", false, &[committed]);
            record_post_wait_partial_commit_timeout_projection(
                "click",
                serde_json::json!({
                    "interaction": {"semantic_class": "click"},
                }),
                Some(7),
            );
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "pipe");
        assert_eq!(projection["steps"][0]["status"], "committed");
        assert_eq!(
            projection["in_flight_step_projection"]["partial_commit"]["kind"],
            "post_wait_after_commit"
        );
        assert_eq!(
            projection["in_flight_step_projection"]["committed_response_projection"]["interaction"]
                ["semantic_class"],
            "click"
        );
    }

    #[tokio::test]
    async fn pending_workflow_timeout_projection_preserves_owner_lane_before_first_step_returns() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_workflow_pending_step_timeout_projection("pipe", false, &[], 0);
            record_post_wait_partial_commit_timeout_projection(
                "click",
                serde_json::json!({
                    "interaction": {"semantic_class": "click"},
                }),
                Some(7),
            );
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "pipe");
        assert_eq!(projection["transaction"]["failed_step_index"], 0);
        assert_eq!(
            projection["in_flight_step_projection"]["partial_commit"]["kind"],
            "post_wait_after_commit"
        );
    }

    #[tokio::test]
    async fn mutating_possible_commit_timeout_projection_records_recovery_contract() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_mutating_possible_commit_timeout_projection(
                "click",
                serde_json::json!({
                    "kind": "interaction_possible_commit",
                    "same_command_retry_requires_same_command_id": true,
                }),
            );
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["reason"], "mutating_command_possible_commit");
        assert_eq!(
            projection["partial_commit"]["effect_commit_state"],
            "possible_commit"
        );
        assert_eq!(
            projection["partial_commit"]["recovery_contract"]["kind"],
            "interaction_possible_commit"
        );
    }

    #[tokio::test]
    async fn registry_control_commit_projection_records_committed_authority() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_registry_control_commit_timeout_projection(
                "trigger",
                "remove",
                "removed",
                serde_json::json!({
                    "id": 7,
                    "status": "armed",
                }),
            );
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "trigger");
        assert_eq!(projection["transaction"]["status"], "committed");
        assert_eq!(
            projection["transaction"]["failure_class"],
            "outer_timeout_after_registry_commit"
        );
        assert_eq!(
            projection["transaction"]["recovery_contract"]["kind"],
            "registry_commit"
        );
        assert_eq!(projection["committed"]["removed"]["id"], 7);
    }

    #[tokio::test]
    async fn effectful_command_possible_commit_projection_records_safe_retry_policy() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_effectful_command_possible_commit_timeout_projection("scroll", Some("cmd-1"));
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["reason"], "mutating_command_possible_commit");
        assert_eq!(projection["partial_commit"]["command"], "scroll");
        assert_eq!(
            projection["partial_commit"]["recovery_contract"]["kind"],
            "command_possible_commit"
        );
        assert_eq!(
            projection["partial_commit"]["recovery_contract"]["fresh_command_retry_safe"],
            false
        );
        assert_eq!(
            projection["partial_commit"]["recovery_contract"]["command_id"],
            "cmd-1"
        );
    }

    #[tokio::test]
    async fn read_only_command_possible_commit_projection_is_not_recorded() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_effectful_command_possible_commit_timeout_projection("state", Some("cmd-1"));
        })
        .await;

        assert!(recorder.snapshot().is_none());
    }

    #[tokio::test]
    async fn pending_orchestration_timeout_projection_preserves_owner_lane_before_first_step_returns()
     {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_orchestration_pending_step_timeout_projection(7, 2, &[], 0);
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "orchestration");
        assert_eq!(projection["execution"]["failed_step_index"], 0);
        assert_eq!(projection["execution"]["committed_steps"], 0);
    }

    #[tokio::test]
    async fn snapshot_preserves_outer_and_nested_owner_lanes() {
        let recorder = Arc::new(ExecutionTimeoutProjectionRecorder::default());
        scope_timeout_projection(recorder.clone(), async {
            record_workflow_pending_step_timeout_projection("pipe", false, &[], 1);
            record_orchestration_pending_step_timeout_projection(7, 2, &[], 0);
            record_post_wait_partial_commit_timeout_projection(
                "click",
                serde_json::json!({
                    "interaction": {"semantic_class": "click"},
                }),
                Some(9),
            );
        })
        .await;

        let projection = recorder.snapshot().expect("projection should be recorded");
        assert_eq!(projection["subject"]["kind"], "pipe");
        assert_eq!(
            projection["nested_owner_projection"]["subject"]["kind"],
            "orchestration"
        );
        assert_eq!(
            projection["nested_owner_projection"]["in_flight_step_projection"]["partial_commit"]["kind"],
            "post_wait_after_commit"
        );
    }
}
