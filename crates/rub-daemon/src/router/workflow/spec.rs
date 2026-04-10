use super::args::{
    FillStepSpec, ParsedPipeWorkflowSpec, PipeEmbeddedOrchestrationSpec, PipeStepSpec,
    PipeWorkflowAssetSpec,
};
use super::*;

pub(super) fn parse_fill_steps(
    raw: &str,
    rub_home: &std::path::Path,
) -> Result<super::super::secret_resolution::ResolvedJsonSpec<Vec<FillStepSpec>>, RubError> {
    parse_json_spec_with_secret_resolution(raw, "fill", rub_home)
}

pub(super) fn parse_pipe_spec(
    raw: &str,
    rub_home: &std::path::Path,
) -> Result<super::super::secret_resolution::ResolvedJsonSpec<ParsedPipeWorkflowSpec>, RubError> {
    let trimmed = raw.trim_start();
    let parsed = if trimmed.starts_with('[') {
        let parsed =
            parse_json_spec_with_secret_resolution::<Vec<PipeStepSpec>>(raw, "pipe", rub_home)?;
        super::super::secret_resolution::ResolvedJsonSpec {
            value: ParsedPipeWorkflowSpec {
                steps: parsed.value,
                orchestrations: Vec::new(),
            },
            metadata: parsed.metadata,
        }
    } else {
        let parsed =
            parse_json_spec_with_secret_resolution::<PipeWorkflowAssetSpec>(raw, "pipe", rub_home)?;
        super::super::secret_resolution::ResolvedJsonSpec {
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

    let mut seen_labels = std::collections::HashSet::new();
    for (index, step) in parsed.value.steps.iter().enumerate() {
        if let Some(label) = step.label.as_deref()
            && !seen_labels.insert(label.to_string())
        {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("Duplicate step label '{label}' at step index {index}"),
                serde_json::json!({
                    "label": label,
                    "step_index": index,
                }),
            ));
        }
    }

    Ok(parsed)
}

pub(super) fn build_embedded_orchestration_args(
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

pub(super) fn resolve_step_references(
    args: &mut serde_json::Value,
    completed: &[serde_json::Value],
    step_index: usize,
) -> Result<(), RubError> {
    match args {
        serde_json::Value::String(s) => {
            if let Some(json_value) = try_resolve_whole_placeholder(s, completed, step_index)? {
                *args = json_value;
            } else {
                *s = resolve_template_string(s, completed, step_index)?;
            }
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

fn try_resolve_whole_placeholder(
    input: &str,
    completed: &[serde_json::Value],
    step_index: usize,
) -> Result<Option<serde_json::Value>, RubError> {
    let trimmed = input.trim();
    if !trimmed.starts_with("{{") || !trimmed.ends_with("}}") {
        return Ok(None);
    }
    let inner = &trimmed[2..trimmed.len() - 2];
    if inner.contains("{{") || inner.contains("}}") {
        return Ok(None);
    }
    let reference = inner.trim();
    let (step_value, path) = resolve_reference_target(reference, completed, step_index)?;
    let resolved = navigate_json_path(step_value, path)?;
    Ok(Some(resolved.clone()))
}

pub(super) fn resolve_template_string(
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
    let (step_value, path) = resolve_reference_target(reference, completed, step_index)?;
    let resolved = navigate_json_path(step_value, path)?;

    match resolved {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Null => Ok("null".to_string()),
        other => Ok(other.to_string()),
    }
}

fn resolve_reference_target<'a>(
    reference: &'a str,
    completed: &'a [serde_json::Value],
    step_index: usize,
) -> Result<(&'a serde_json::Value, &'a str), RubError> {
    if let Some(path) = reference.strip_prefix("prev.") {
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
        Ok((&completed[step_index - 1], path))
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
        Ok((&completed[target_index], path))
    } else {
        Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown reference '{{{{{reference}}}}}': must start with 'prev.' or 'steps['"),
        ))
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
