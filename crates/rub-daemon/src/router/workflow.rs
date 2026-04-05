use std::sync::Arc;

use super::addressing::resolve_element;
use super::automation_fence::ensure_committed_automation_result;
use super::dispatch::execute_named_command_with_fence;
use super::request_args::{
    LocatorParseOptions, LocatorRequestArgs, canonical_locator_json, locator_json,
    parse_canonical_locator, parse_json_args,
};
use super::secret_resolution::{
    parse_json_spec_with_secret_resolution, redact_json_value, redact_rub_error,
};
use super::*;
use crate::workflow_policy::{workflow_allowed_step_descriptions, workflow_request_allowed};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FillStepSpec {
    index: Option<u32>,
    #[serde(rename = "ref")]
    element_ref: Option<String>,
    selector: Option<String>,
    target_text: Option<String>,
    role: Option<String>,
    label: Option<String>,
    testid: Option<String>,
    #[serde(default)]
    first: bool,
    #[serde(default)]
    last: bool,
    nth: Option<u32>,
    value: Option<String>,
    activate: Option<bool>,
    clear: Option<bool>,
    #[serde(default)]
    wait_after: Option<StepWaitSpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StepWaitSpec {
    selector: Option<String>,
    target_text: Option<String>,
    role: Option<String>,
    label: Option<String>,
    testid: Option<String>,
    text: Option<String>,
    #[serde(default)]
    first: bool,
    #[serde(default)]
    last: bool,
    nth: Option<u32>,
    timeout_ms: Option<u64>,
    state: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PipeStepSpec {
    command: String,
    #[serde(default)]
    args: serde_json::Value,
    label: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PipeWorkflowAssetSpec {
    #[serde(default)]
    steps: Vec<PipeStepSpec>,
    #[serde(default)]
    orchestrations: Vec<PipeEmbeddedOrchestrationSpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PipeEmbeddedOrchestrationSpec {
    #[serde(default)]
    label: Option<String>,
    spec: serde_json::Value,
}

#[derive(Debug)]
struct ParsedPipeWorkflowSpec {
    steps: Vec<PipeStepSpec>,
    orchestrations: Vec<PipeEmbeddedOrchestrationSpec>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FillArgs {
    spec: String,
    #[serde(default, rename = "spec_source")]
    _spec_source: Option<serde_json::Value>,
    #[serde(flatten)]
    submit: SubmitLocatorArgs,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PipeArgs {
    spec: String,
    #[serde(default)]
    spec_source: Option<serde_json::Value>,
    #[serde(default, rename = "wait_after")]
    _wait_after: Option<serde_json::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct SubmitLocatorArgs {
    #[serde(rename = "submit_index")]
    index: Option<u32>,
    #[serde(rename = "submit_ref")]
    element_ref: Option<String>,
    #[serde(rename = "submit_selector")]
    selector: Option<String>,
    #[serde(rename = "submit_target_text")]
    target_text: Option<String>,
    #[serde(rename = "submit_role")]
    role: Option<String>,
    #[serde(rename = "submit_label")]
    label: Option<String>,
    #[serde(rename = "submit_testid")]
    testid: Option<String>,
    #[serde(rename = "submit_first")]
    first: bool,
    #[serde(rename = "submit_last")]
    last: bool,
    #[serde(rename = "submit_nth")]
    nth: Option<u32>,
}

impl SubmitLocatorArgs {
    fn locator_args(&self) -> LocatorRequestArgs {
        LocatorRequestArgs {
            index: self.index,
            element_ref: self.element_ref.clone(),
            selector: self.selector.clone(),
            target_text: self.target_text.clone(),
            role: self.role.clone(),
            label: self.label.clone(),
            testid: self.testid.clone(),
            first: self.first,
            last: self.last,
            nth: self.nth,
        }
    }
}

fn workflow_step_projection(
    step_index: usize,
    command: &str,
    label: Option<String>,
    role: Option<&str>,
    data: serde_json::Value,
) -> serde_json::Value {
    let mut action = serde_json::Map::new();
    action.insert("kind".to_string(), serde_json::json!("command"));
    action.insert("command".to_string(), serde_json::json!(command));
    if let Some(label) = label {
        action.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(role) = role {
        action.insert("role".to_string(), serde_json::json!(role));
    }

    serde_json::json!({
        "step_index": step_index,
        "status": "committed",
        "action": action,
        "result": data,
    })
}

pub(super) async fn cmd_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed_args: FillArgs = parse_json_args(args, "fill")?;
    let parsed = parse_fill_steps(&parsed_args.spec, &state.rub_home)?;
    let steps = parsed.value;
    let metadata = parsed.metadata;
    let mut results = Vec::with_capacity(steps.len() + 1);

    for (step_index, step) in steps.into_iter().enumerate() {
        let (command, mut command_args) = match build_fill_step_command(router, state, &step).await
        {
            Ok(result) => result,
            Err(error) => return Err(redact_rub_error(error, &metadata)),
        };
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

    if let Some(submit_args) = submit_args(&parsed_args.submit) {
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

        let data =
            match execute_named_command_with_fence(router, command, &step.args, deadline, state)
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

async fn build_fill_step_command(
    router: &DaemonRouter,
    state: &Arc<SessionState>,
    step: &FillStepSpec,
) -> Result<(&'static str, serde_json::Value), RubError> {
    let locator_args = locator_json(LocatorRequestArgs {
        index: step.index,
        element_ref: step.element_ref.clone(),
        selector: step.selector.clone(),
        target_text: step.target_text.clone(),
        role: step.role.clone(),
        label: step.label.clone(),
        testid: step.testid.clone(),
        first: step.first,
        last: step.last,
        nth: step.nth,
    });
    let resolved = resolve_element(router, &locator_args, state, "fill").await?;

    if let Some(value) = &step.value {
        return match resolved.element.tag {
            rub_core::model::ElementTag::Select => Ok((
                "select",
                serde_json::json!({
                    "index": resolved.element.index,
                    "snapshot_id": resolved.snapshot_id,
                    "value": value,
                }),
            )),
            rub_core::model::ElementTag::Input | rub_core::model::ElementTag::TextArea => Ok((
                "type",
                serde_json::json!({
                    "index": resolved.element.index,
                    "snapshot_id": resolved.snapshot_id,
                    "text": value,
                    "clear": step.clear.unwrap_or(true),
                }),
            )),
            tag => Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("fill value is unsupported for target tag '{tag:?}'"),
                serde_json::json!({
                    "index": resolved.element.index,
                    "tag": tag,
                }),
            )),
        };
    }

    if step.activate.unwrap_or(false) {
        return Ok((
            "click",
            serde_json::json!({
                "index": resolved.element.index,
                "snapshot_id": resolved.snapshot_id,
            }),
        ));
    }

    Err(RubError::domain(
        ErrorCode::InvalidInput,
        "fill step requires either 'value' or 'activate: true'",
    ))
}

fn attach_step_wait_after(target: &mut serde_json::Value, wait_after: &StepWaitSpec) {
    let mut wait = serde_json::Map::new();
    if let Some(selector) = &wait_after.selector {
        wait.insert("selector".to_string(), serde_json::json!(selector));
    }
    if let Some(target_text) = &wait_after.target_text {
        wait.insert("target_text".to_string(), serde_json::json!(target_text));
    }
    if let Some(role) = &wait_after.role {
        wait.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(label) = &wait_after.label {
        wait.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(testid) = &wait_after.testid {
        wait.insert("testid".to_string(), serde_json::json!(testid));
    }
    if let Some(text) = &wait_after.text {
        wait.insert("text".to_string(), serde_json::json!(text));
    }
    if wait_after.first {
        wait.insert("first".to_string(), serde_json::json!(true));
    }
    if wait_after.last {
        wait.insert("last".to_string(), serde_json::json!(true));
    }
    if let Some(nth) = wait_after.nth {
        wait.insert("nth".to_string(), serde_json::json!(nth));
    }
    if let Some(timeout_ms) = wait_after.timeout_ms {
        wait.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
    }
    if let Some(state) = &wait_after.state {
        wait.insert("state".to_string(), serde_json::json!(state));
    }
    if let Some(object) = target.as_object_mut()
        && !wait.is_empty()
    {
        object.insert("wait_after".to_string(), serde_json::Value::Object(wait));
    }
}

fn submit_args(args: &SubmitLocatorArgs) -> Option<serde_json::Value> {
    let locator = args.locator_args();
    if !locator.is_requested() {
        return None;
    }

    parse_canonical_locator(
        &locator_json(locator),
        LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS,
    )
    .ok()
    .flatten()
    .map(|locator| canonical_locator_json(&locator))
}

fn parse_fill_steps(
    raw: &str,
    rub_home: &std::path::Path,
) -> Result<super::secret_resolution::ResolvedJsonSpec<Vec<FillStepSpec>>, RubError> {
    parse_json_spec_with_secret_resolution(raw, "fill", rub_home)
}

fn parse_pipe_spec(
    raw: &str,
    rub_home: &std::path::Path,
) -> Result<super::secret_resolution::ResolvedJsonSpec<ParsedPipeWorkflowSpec>, RubError> {
    let trimmed = raw.trim_start();
    let parsed = if trimmed.starts_with('[') {
        let parsed =
            parse_json_spec_with_secret_resolution::<Vec<PipeStepSpec>>(raw, "pipe", rub_home)?;
        super::secret_resolution::ResolvedJsonSpec {
            value: ParsedPipeWorkflowSpec {
                steps: parsed.value,
                orchestrations: Vec::new(),
            },
            metadata: parsed.metadata,
        }
    } else {
        let parsed =
            parse_json_spec_with_secret_resolution::<PipeWorkflowAssetSpec>(raw, "pipe", rub_home)?;
        super::secret_resolution::ResolvedJsonSpec {
            value: ParsedPipeWorkflowSpec {
                steps: parsed.value.steps,
                orchestrations: parsed.value.orchestrations,
            },
            metadata: parsed.metadata,
        }
    };

    if parsed.value.steps.is_empty() && parsed.value.orchestrations.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "pipe workflow objects must declare at least one step or embedded orchestration block",
        ));
    }

    Ok(parsed)
}

fn build_embedded_orchestration_args(
    workflow_source: Option<&serde_json::Value>,
    orchestration: &PipeEmbeddedOrchestrationSpec,
    block_index: usize,
) -> Result<serde_json::Value, RubError> {
    let spec = serde_json::to_string(&orchestration.spec).map_err(RubError::from)?;
    let workflow_source = workflow_source
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "kind": "inline" }));
    Ok(serde_json::json!({
        "sub": "add",
        "spec": spec,
        "spec_source": {
            "kind": "workflow_embedded",
            "workflow_source": workflow_source,
            "block_index": block_index,
            "label": orchestration.label,
        },
    }))
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

#[cfg(test)]
mod tests {
    use super::{FillArgs, SubmitLocatorArgs, parse_pipe_spec, submit_args};
    use crate::router::automation_fence::ensure_committed_automation_result;
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn parse_pipe_spec_accepts_legacy_steps_array_shorthand() {
        let parsed = parse_pipe_spec(
            r#"[{"command":"open","args":{"url":"https://example.com"}}]"#,
            Path::new("/tmp/rub-workflow-parse-array"),
        )
        .expect("legacy pipe array shorthand should be normalized");
        assert_eq!(parsed.value.steps.len(), 1);
        assert!(parsed.value.orchestrations.is_empty());
    }

    #[test]
    fn parse_pipe_spec_rejects_watch_alias() {
        let error = parse_pipe_spec(
            r##"{
              "steps": [{"command":"state","args":{"format":"compact"}}],
              "watch": [{
                "label": "reply",
                "spec": {
                  "source": {"session_id":"source-session"},
                  "target": {"session_id":"target-session"},
                  "mode": "once",
                  "condition": {"kind":"text_present","text":"Ready"},
                  "actions": [{
                    "kind":"browser_command",
                    "command":"click",
                    "payload":{"selector":"#apply"}
                  }]
                }
              }]
            }"##,
            Path::new("/tmp/rub-workflow-parse-object"),
        )
        .expect_err("watch alias should be rejected");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn parse_pipe_spec_rejects_empty_workflow_object() {
        let error = parse_pipe_spec(
            r#"{"steps":[],"orchestrations":[]}"#,
            Path::new("/tmp/rub-workflow-parse-empty"),
        )
        .expect_err("empty workflow object should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn parse_pipe_spec_rejects_unknown_step_fields() {
        let error = parse_pipe_spec(
            r##"{
              "steps": [
                {"command":"click","args":{"selector":"#go"},"argz":{"selector":"#wrong"}}
              ]
            }"##,
            Path::new("/tmp/rub-workflow-parse-unknown"),
        )
        .expect_err("unknown step fields should fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn automation_step_commit_fence_fails_closed_on_degraded_interaction() {
        let error = ensure_committed_automation_result(
            "click",
            Some(&serde_json::json!({
                "interaction": {
                    "confirmation_status": "degraded",
                    "confirmation_kind": "value_applied",
                }
            })),
        )
        .expect_err("non-confirmed interaction must stop workflow automation");
        assert_eq!(error.code, ErrorCode::WaitTimeout);
    }

    #[test]
    fn fill_args_parse_submit_locator_and_wait_after() {
        let parsed: FillArgs = parse_json_args(
            &json!({
                "spec": "[]",
                "submit_label": "Send",
                "submit_first": true,
                "wait_after": {"selector":"#done"},
            }),
            "fill",
        )
        .expect("fill args should parse through typed envelope");

        assert_eq!(parsed.submit.label.as_deref(), Some("Send"));
        assert!(parsed.submit.first);
        assert!(parsed._wait_after.is_some());
    }

    #[test]
    fn typed_submit_locator_ignores_selection_without_locator() {
        let submit = SubmitLocatorArgs {
            first: true,
            ..SubmitLocatorArgs::default()
        };
        assert!(
            submit_args(&submit).is_none(),
            "selection-only submit args should not fabricate a locator"
        );
    }
}
