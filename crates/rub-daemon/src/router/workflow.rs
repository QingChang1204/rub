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

        let mut resolved_args = step.args.clone();
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

/// Resolve `{{prev.PATH}}` and `{{steps[N].PATH}}` (or `{{steps[LABEL].PATH}}`) references
/// in step args by walking all JSON string values and replacing matching patterns with
/// the resolved value from a prior step's result.
fn resolve_step_references(
    args: &mut serde_json::Value,
    completed: &[serde_json::Value],
    step_index: usize,
) -> Result<(), RubError> {
    match args {
        serde_json::Value::String(s) => {
            *s = resolve_template_string(s, completed, step_index)?;
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                resolve_step_references(value, completed, step_index)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for value in arr.iter_mut() {
                resolve_step_references(value, completed, step_index)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_template_string(
    input: &str,
    completed: &[serde_json::Value],
    step_index: usize,
) -> Result<String, RubError> {
    if !input.contains("{{") {
        return Ok(input.to_string());
    }

    let mut result = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + 2..];
        let Some(end) = after_open.find("}}") else {
            // No closing }}, treat as literal
            result.push_str("{{");
            remaining = after_open;
            continue;
        };

        let reference = after_open[..end].trim();
        let resolved = resolve_single_reference(reference, completed, step_index)?;
        result.push_str(&resolved);
        remaining = &after_open[end + 2..];
    }
    result.push_str(remaining);

    Ok(result)
}

fn resolve_single_reference(
    reference: &str,
    completed: &[serde_json::Value],
    step_index: usize,
) -> Result<String, RubError> {
    // Parse "prev.PATH" or "steps[N].PATH" or "steps[LABEL].PATH"
    let (step_value, path) = if let Some(path) = reference.strip_prefix("prev.") {
        if step_index == 0 {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "{{prev.*}} reference used in step 0, but there is no previous step".to_string(),
                serde_json::json!({
                    "reference": format!("{{{{{reference}}}}}"),
                    "step_index": step_index,
                }),
            ));
        }
        (&completed[step_index - 1], path)
    } else if let Some(rest) = reference.strip_prefix("steps[") {
        let Some(bracket_end) = rest.find(']') else {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Malformed step reference: missing ']' in '{{{{{reference}}}}}'"),
            ));
        };
        let index_or_label = &rest[..bracket_end];
        let path = rest[bracket_end + 1..].strip_prefix('.').unwrap_or("");

        let target_index = if let Ok(n) = index_or_label.parse::<usize>() {
            if n >= step_index {
                return Err(RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!(
                        "Step reference '{{{{steps[{n}]}}}}' at step {step_index} references a non-completed step"
                    ),
                    serde_json::json!({
                        "reference": format!("{{{{{reference}}}}}"),
                        "step_index": step_index,
                        "requested_index": n,
                    }),
                ));
            }
            n
        } else {
            // Label lookup
            completed
                .iter()
                .position(|step| {
                    step.get("action")
                        .and_then(|a| a.get("label"))
                        .and_then(|l| l.as_str())
                        == Some(index_or_label)
                })
                .ok_or_else(|| {
                    RubError::domain_with_context(
                        ErrorCode::InvalidInput,
                        format!("Step label '{index_or_label}' not found in completed steps"),
                        serde_json::json!({
                            "reference": format!("{{{{{reference}}}}}"),
                            "label": index_or_label,
                            "step_index": step_index,
                        }),
                    )
                })?
        };
        (&completed[target_index], path)
    } else {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown reference '{{{{{reference}}}}}': must start with 'prev.' or 'steps['"),
        ));
    };

    // Navigate the JSON path
    let resolved = navigate_json_path(step_value, path)?;

    // Convert to string representation
    match resolved {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Null => Ok("null".to_string()),
        other => Ok(other.to_string()),
    }
}

fn navigate_json_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Result<&'a serde_json::Value, RubError> {
    if path.is_empty() {
        return Ok(value);
    }

    let mut current = value;
    for segment in path.split('.') {
        // Support array indexing: "items[0]"
        if let Some(bracket_start) = segment.find('[') {
            let key = &segment[..bracket_start];
            if !key.is_empty() {
                current = current.get(key).ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::InvalidInput,
                        format!("Path segment '{key}' not found in step result"),
                    )
                })?;
            }
            let bracket_content = &segment[bracket_start + 1..];
            let idx_str = bracket_content.strip_suffix(']').unwrap_or(bracket_content);
            let idx: usize = idx_str.parse().map_err(|_| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Invalid array index '{idx_str}' in path"),
                )
            })?;
            current = current.get(idx).ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Array index {idx} out of bounds in step result"),
                )
            })?;
        } else {
            current = current.get(segment).ok_or_else(|| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Path segment '{segment}' not found in step result"),
                )
            })?;
        }
    }
    Ok(current)
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
    use super::{
        FillArgs, SubmitLocatorArgs, parse_pipe_spec, resolve_step_references,
        resolve_template_string, submit_args,
    };
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

    fn mock_completed_steps() -> Vec<serde_json::Value> {
        vec![
            json!({
                "step_index": 0,
                "status": "committed",
                "action": { "kind": "command", "command": "extract", "label": "get_title" },
                "result": {
                    "field_count": 1,
                    "fields": { "title": "Hello World", "count": 42 },
                    "items": [{ "name": "A" }, { "name": "B" }]
                }
            }),
            json!({
                "step_index": 1,
                "status": "committed",
                "action": { "kind": "command", "command": "exec" },
                "result": { "value": "computed" }
            }),
        ]
    }

    #[test]
    fn resolve_prev_result_field() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "console.log('{{prev.result.value}}')" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["code"], "console.log('computed')");
    }

    #[test]
    fn resolve_steps_by_index() {
        let completed = mock_completed_steps();
        let mut args = json!({ "url": "https://example.com/{{steps[0].result.fields.title}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["url"], "https://example.com/Hello World");
    }

    #[test]
    fn resolve_steps_by_label() {
        let completed = mock_completed_steps();
        let mut args = json!({ "text": "{{steps[get_title].result.fields.title}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["text"], "Hello World");
    }

    #[test]
    fn resolve_array_index_in_path() {
        let completed = mock_completed_steps();
        let mut args = json!({ "name": "{{steps[0].result.items[1].name}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["name"], "B");
    }

    #[test]
    fn resolve_number_as_string() {
        let completed = mock_completed_steps();
        let mut args = json!({ "count": "{{steps[0].result.fields.count}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["count"], "42");
    }

    #[test]
    fn resolve_multiple_references_in_one_string() {
        let completed = mock_completed_steps();
        let mut args = json!({ "msg": "Title: {{steps[0].result.fields.title}}, Value: {{prev.result.value}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["msg"], "Title: Hello World, Value: computed");
    }

    #[test]
    fn resolve_no_references_passthrough() {
        let completed = mock_completed_steps();
        let mut args = json!({ "url": "https://example.com" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["url"], "https://example.com");
    }

    #[test]
    fn resolve_prev_at_step_0_fails() {
        let completed = vec![];
        let mut args = json!({ "code": "{{prev.result.value}}" });
        let err = resolve_step_references(&mut args, &completed, 0).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_forward_reference_fails() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "{{steps[2].result.value}}" });
        let err = resolve_step_references(&mut args, &completed, 2).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_unknown_label_fails() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "{{steps[nonexistent].result.value}}" });
        let err = resolve_step_references(&mut args, &completed, 2).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_invalid_path_fails() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "{{prev.result.missing_key}}" });
        let err = resolve_step_references(&mut args, &completed, 2).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_template_string_passthrough_no_braces() {
        let result = resolve_template_string("hello world", &[], 0).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn resolve_template_string_unclosed_braces_literal() {
        let completed = mock_completed_steps();
        let result = resolve_template_string("prefix {{ no close", &completed, 2).unwrap();
        assert_eq!(result, "prefix {{ no close");
    }

    #[test]
    fn resolve_nested_array_in_args() {
        let completed = mock_completed_steps();
        let mut args = json!([{"value": "{{prev.result.value}}"}]);
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args[0]["value"], "computed");
    }
}
