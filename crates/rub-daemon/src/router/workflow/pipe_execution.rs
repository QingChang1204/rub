use super::args::PipeArgs;
use super::execution::{OrchestrationMetadataInheritancePolicy, inherit_orchestration_metadata};
use super::projection::{
    workflow_error_projection, workflow_failed_step_projection, workflow_step_projection,
};
use super::spec::{build_embedded_orchestration_args, parse_pipe_spec, resolve_step_references};
use super::*;
use crate::router::automation_fence::ensure_committed_automation_result;
use crate::router::dispatch::execute_named_command_with_fence;
use crate::router::request_args::parse_json_args;
use crate::router::secret_resolution::{
    attach_secret_resolution_projection, redact_json_value, redact_rub_error,
};
use crate::router::timeout_projection::{
    record_workflow_partial_commit_timeout_projection,
    record_workflow_pending_step_timeout_projection,
};
use crate::workflow_policy::{
    trigger_workflow_allowed_step_descriptions, trigger_workflow_request_allowed,
    workflow_allowed_step_descriptions, workflow_request_allowed,
};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_core::recovery_contract::partial_commit_steps_contract;

pub(super) async fn cmd_pipe_with_policy(
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
    let mut publish_current_dom_epoch = false;
    let trigger_owned_workflow = matches!(
        inheritance_policy,
        OrchestrationMetadataInheritancePolicy::TriggerAuthoritativeFrame
    );

    for (index, step) in steps.into_iter().enumerate() {
        let command = step.command.as_str();
        let allowed = if trigger_owned_workflow {
            trigger_workflow_request_allowed(command, &step.args)
        } else {
            workflow_request_allowed(command, &step.args)
        };
        if !allowed {
            let allowed_commands = if trigger_owned_workflow {
                trigger_workflow_allowed_step_descriptions()
            } else {
                workflow_allowed_step_descriptions()
            };
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("pipe step command '{command}' is not allowed"),
                serde_json::json!({
                    "step_index": index,
                    "allowed_commands": allowed_commands,
                    "trigger_owned_workflow": trigger_owned_workflow,
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
                "reference_resolution_failed",
            ));
        }

        record_workflow_pending_step_timeout_projection("pipe", false, &completed, index);
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
                    "step_execution_failed",
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
                "step_commit_fence_failed",
            ));
        }
        publish_current_dom_epoch |= response_carries_dom_epoch(&data);
        let mut data = data;
        redact_json_value(&mut data, &metadata);

        completed.push(workflow_step_projection(
            index, command, step.label, None, data,
        ));
        record_workflow_partial_commit_timeout_projection("pipe", false, &completed);
    }

    for orchestration in orchestrations {
        let step_index = completed.len();
        let label = orchestration.label.clone();
        let command_args = build_embedded_orchestration_args(
            parsed_args.spec_source.as_ref(),
            &orchestration,
            step_index,
        )?;
        record_workflow_pending_step_timeout_projection("pipe", false, &completed, step_index);
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
                    "embedded_orchestration_failed",
                ));
            }
        };
        if let Err(error) = ensure_embedded_orchestration_registration_committed(&data) {
            return Err(pipe_step_error(
                redact_rub_error(RubError::Domain(error), &metadata),
                step_index,
                "orchestration",
                label.as_deref(),
                &completed,
                "embedded_orchestration_registration_fence_failed",
            ));
        }
        publish_current_dom_epoch |= response_carries_dom_epoch(&data);
        let mut data = data;
        redact_json_value(&mut data, &metadata);
        completed.push(workflow_step_projection(
            step_index,
            "orchestration",
            label,
            None,
            data,
        ));
        record_workflow_partial_commit_timeout_projection("pipe", false, &completed);
    }

    let mut data = serde_json::json!({
        "steps": completed,
    });
    if publish_current_dom_epoch {
        data = attach_response_metadata(data, Some(state.current_epoch()));
    }
    attach_secret_resolution_projection(&mut data, &metadata);
    redact_json_value(&mut data, &metadata);
    Ok(data)
}

fn response_carries_dom_epoch(data: &serde_json::Value) -> bool {
    data.get("dom_epoch")
        .and_then(serde_json::Value::as_u64)
        .is_some()
}

fn ensure_embedded_orchestration_registration_committed(
    data: &serde_json::Value,
) -> Result<(), ErrorEnvelope> {
    let result = data.get("result").ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            "Embedded orchestration response did not include result payload",
        )
        .with_context(serde_json::json!({
            "reason": "embedded_orchestration_result_missing",
        }))
    })?;

    let rule = result.get("rule").ok_or_else(|| {
        ErrorEnvelope::new(
            ErrorCode::IpcProtocolError,
            "Embedded orchestration response did not include registered rule truth",
        )
        .with_context(serde_json::json!({
            "reason": "embedded_orchestration_rule_missing",
            "result": result,
        }))
    })?;

    let rule_id = rule
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                "Embedded orchestration response did not include rule.id",
            )
            .with_context(serde_json::json!({
                "reason": "embedded_orchestration_rule_id_missing",
                "rule": rule,
            }))
        })?;

    let status = rule
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::IpcProtocolError,
                "Embedded orchestration response did not include rule.status",
            )
            .with_context(serde_json::json!({
                "reason": "embedded_orchestration_rule_status_missing",
                "rule": rule,
            }))
        })?;

    if matches!(status, "armed" | "paused") {
        return Ok(());
    }

    Err(ErrorEnvelope::new(
        ErrorCode::IpcProtocolError,
        format!(
            "Embedded orchestration step returned unexpected rule status '{status}' after registration"
        ),
    )
    .with_context(serde_json::json!({
        "reason": "embedded_orchestration_rule_status_invalid",
        "rule_id": rule_id,
        "rule_status": status,
        "rule": rule,
    })))
}

fn pipe_step_error(
    error: RubError,
    step_index: usize,
    step_command: &str,
    step_label: Option<&str>,
    completed: &[serde_json::Value],
    failure_class: &'static str,
) -> RubError {
    let source = error.into_envelope();
    let mut steps = completed.to_vec();
    steps.push(workflow_failed_step_projection(
        step_index,
        step_command,
        step_label.map(ToOwned::to_owned),
        None,
        &source,
    ));

    let context = serde_json::json!({
        "subject": {
            "kind": "pipe",
            "source": "live_execution",
        },
        "transaction": {
            "atomic": false,
            "status": "failed",
            "failure_class": failure_class,
            "failed_step_index": step_index,
            "committed_step_count": completed.len(),
            "rollback_attempted": false,
            "rollback_failed": false,
            "source_error": workflow_error_projection(&source),
            "recovery_contract": partial_commit_steps_contract(),
        },
        "steps": steps,
    });

    RubError::Domain(
        ErrorEnvelope::new(
            source.code,
            format!(
                "pipe step {} ('{}') failed: {}",
                step_index, step_command, source.message
            ),
        )
        .with_suggestion(source.suggestion)
        .with_context(context),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mock_completed_steps() -> Vec<serde_json::Value> {
        vec![
            workflow_step_projection(
                0,
                "extract",
                Some("get_title".to_string()),
                None,
                json!({ "title": "Hello" }),
            ),
            workflow_step_projection(1, "exec", None, None, json!({ "value": "computed" })),
        ]
    }

    #[test]
    fn pipe_step_error_projects_failure_authority_with_committed_scope() {
        let error = RubError::domain_with_context(
            ErrorCode::ElementNotFound,
            "element disappeared",
            json!({
                "element_ref": "frame-main:7",
            }),
        );
        let envelope = pipe_step_error(
            error,
            2,
            "click",
            Some("submit"),
            &mock_completed_steps(),
            "step_execution_failed",
        )
        .into_envelope();

        assert_eq!(envelope.code, ErrorCode::ElementNotFound);
        assert_eq!(
            envelope.message,
            "pipe step 2 ('click') failed: element disappeared"
        );
        let context = envelope.context.expect("pipe failure context");
        assert_eq!(context["subject"]["kind"], "pipe");
        assert_eq!(context["transaction"]["atomic"], false);
        assert_eq!(context["transaction"]["status"], "failed");
        assert_eq!(
            context["transaction"]["failure_class"],
            "step_execution_failed"
        );
        assert_eq!(context["transaction"]["failed_step_index"], 2);
        assert_eq!(context["transaction"]["committed_step_count"], 2);
        assert_eq!(context["transaction"]["rollback_attempted"], false);
        assert_eq!(
            context["transaction"]["recovery_contract"]["kind"],
            "partial_commit"
        );
        assert_eq!(
            context["transaction"]["source_error"]["context"]["element_ref"],
            "frame-main:7"
        );
        let steps = context["steps"].as_array().expect("steps array");
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["status"], "committed");
        assert_eq!(steps[1]["status"], "committed");
        assert_eq!(steps[2]["status"], "failed");
        assert_eq!(steps[2]["action"]["command"], "click");
        assert_eq!(steps[2]["action"]["label"], "submit");
        assert_eq!(steps[2]["error"]["code"], "ELEMENT_NOT_FOUND");
    }

    #[test]
    fn pipe_step_error_preserves_non_domain_source_via_transaction_projection() {
        let envelope = pipe_step_error(
            RubError::Internal("broken renderer".to_string()),
            0,
            "state",
            None,
            &[],
            "step_execution_failed",
        )
        .into_envelope();

        assert_eq!(envelope.code, ErrorCode::InternalError);
        let context = envelope.context.expect("pipe failure context");
        assert_eq!(context["transaction"]["committed_step_count"], 0);
        assert_eq!(
            context["transaction"]["source_error"]["message"],
            "broken renderer"
        );
        assert_eq!(context["steps"][0]["status"], "failed");
    }

    #[test]
    fn pipe_response_publishes_dom_epoch_only_when_a_child_step_commits_dom_effect() {
        assert!(!response_carries_dom_epoch(&json!({
            "result": { "title": "Hello" }
        })));
        assert!(response_carries_dom_epoch(&json!({
            "dom_epoch": 7,
            "result": { "status": "clicked" }
        })));
    }

    #[test]
    fn embedded_orchestration_registration_requires_registered_rule_truth() {
        ensure_embedded_orchestration_registration_committed(&json!({
            "result": {
                "rule": {
                    "id": 7,
                    "status": "armed"
                }
            }
        }))
        .expect("registered embedded orchestration should count as committed");

        let error = ensure_embedded_orchestration_registration_committed(&json!({
            "result": {
                "rule": {
                    "id": 7,
                    "status": "blocked"
                }
            }
        }))
        .expect_err("embedded orchestration must fail closed on non-registration rule status");
        assert_eq!(error.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("reason"))
                .and_then(|value| value.as_str()),
            Some("embedded_orchestration_rule_status_invalid")
        );
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("rule_status"))
                .and_then(|value| value.as_str()),
            Some("blocked")
        );
    }
}
