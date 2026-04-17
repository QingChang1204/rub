use rub_core::error::{ErrorCode, RubError};
use serde_json::{Value, json};

use crate::commands::{Commands, EffectiveCli};

use super::write::{commit_asset_writes, pending_asset_write};
use super::{normalize_workflow_name, resolve_named_workflow_path};

pub fn persist_history_export_asset(cli: &EffectiveCli, data: &mut Value) -> Result<(), RubError> {
    let Commands::History {
        export_pipe,
        export_script,
        save_as,
        output,
        ..
    } = &cli.command
    else {
        return Ok(());
    };

    if !(*export_pipe || *export_script) {
        return Ok(());
    }
    if save_as.is_none() && output.is_none() {
        return Ok(());
    }

    let object = data.as_object_mut().ok_or_else(|| {
        RubError::domain(
            ErrorCode::IpcProtocolError,
            "history export response must be a JSON object",
        )
    })?;
    let result = object
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::IpcProtocolError,
                "history export response missing result object",
            )
        })?;
    let format = result
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RubError::domain(ErrorCode::IpcProtocolError, "history export missing format")
        })?;
    validate_history_export_persistability(result)?;
    let mut persisted_artifacts = result
        .get("persisted_artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pending_writes = Vec::new();

    if let Some(name) = save_as {
        let path = resolve_named_workflow_path(&cli.rub_home, name)?;
        let serialized = render_export_asset(result, true)?;
        pending_writes.push(pending_asset_write(
            path.clone(),
            serialized,
            json!({
                "kind": "workflow_asset",
                "role": "output",
                "path": path.display().to_string(),
                "workflow_name": normalize_workflow_name(name)?,
            }),
        )?);
    }

    if let Some(output_path) = output {
        let path = std::path::Path::new(output_path).to_path_buf();
        let serialized = render_export_asset(result, false)?;
        pending_writes.push(pending_asset_write(
            path.clone(),
            serialized,
            json!({
                "kind": "history_export_file",
                "role": "output",
                "path": path.display().to_string(),
                "format": format,
            }),
        )?);
    }

    if !pending_writes.is_empty() {
        persisted_artifacts.extend(commit_asset_writes(pending_writes)?);
    }

    if !persisted_artifacts.is_empty() {
        result.insert(
            "persisted_artifacts".to_string(),
            Value::Array(persisted_artifacts),
        );
    }

    Ok(())
}

fn validate_history_export_persistability(
    result: &serde_json::Map<String, Value>,
) -> Result<(), RubError> {
    let projection_state = result
        .get("projection_state")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "history export cannot be persisted because the response does not declare a durable projection contract",
                json!({
                    "reason": "history_export_projection_state_missing",
                }),
            )
        })?;

    let control_role = projection_state
        .get("control_role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let durability = projection_state
        .get("durability")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let lossy = projection_state
        .get("lossy")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let complete = result
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let persistable =
        control_role != "display_only" && durability == "durable" && !lossy && complete;
    if persistable {
        return Ok(());
    }

    Err(RubError::domain_with_context(
        ErrorCode::InvalidInput,
        "history export cannot be persisted as a durable workflow asset because the export surface is display-only, lossy, or not durably authoritative",
        json!({
            "reason": "history_export_projection_not_durable",
            "projection_state": projection_state,
            "complete": complete,
        }),
    ))
}

fn render_export_asset(
    result: &serde_json::Map<String, Value>,
    for_named_workflow: bool,
) -> Result<Vec<u8>, RubError> {
    let format = result
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RubError::domain(ErrorCode::IpcProtocolError, "history export missing format")
        })?;
    match format {
        "pipe" => {
            let steps = result
                .get("entries")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::IpcProtocolError,
                        "history export pipe response missing entries",
                    )
                })?;
            let replayable_steps = steps
                .iter()
                .map(replayable_pipe_step_json)
                .collect::<Result<Vec<_>, _>>()?;
            serde_json::to_vec_pretty(&json!({ "steps": replayable_steps })).map_err(RubError::from)
        }
        "script" => {
            if for_named_workflow {
                return Err(RubError::domain(
                    ErrorCode::InvalidInput,
                    "--save-as is only supported with --export-pipe",
                ));
            }
            let script = result
                .get("export")
                .and_then(Value::as_object)
                .and_then(|export| export.get("content"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    RubError::domain(
                        ErrorCode::IpcProtocolError,
                        "history export script response missing export.content",
                    )
                })?;
            Ok(script.as_bytes().to_vec())
        }
        other => Err(RubError::domain(
            ErrorCode::IpcProtocolError,
            format!("unknown history export format '{other}'"),
        )),
    }
}

fn replayable_pipe_step_json(entry: &Value) -> Result<Value, RubError> {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RubError::domain(
                ErrorCode::IpcProtocolError,
                "history export pipe entry missing command",
            )
        })?;
    let args = entry.get("args").cloned().unwrap_or(Value::Null);
    let mut step = serde_json::Map::from_iter([
        ("command".to_string(), Value::String(command.to_string())),
        ("args".to_string(), args),
    ]);
    if let Some(label) = entry.get("label").and_then(Value::as_str) {
        step.insert("label".to_string(), Value::String(label.to_string()));
    }
    Ok(Value::Object(step))
}
