use std::sync::Arc;

use crate::session::SessionState;
use crate::workflow_capture::{
    WorkflowCaptureClass, WorkflowCaptureEntry, WorkflowCaptureProjection,
};
use rub_core::error::RubError;
use serde_json::Value;

use super::projection::{
    export_selection_json, history_capture_window_json, history_subject,
    workflow_export_projection_state_json,
};

#[derive(Debug, Clone)]
pub(super) struct WorkflowExportStep {
    pub(super) command: String,
    pub(super) args: Value,
    pub(super) source: Value,
}

#[derive(Debug)]
pub(super) struct WorkflowExportProjection {
    pub(super) steps: Vec<WorkflowExportStep>,
    pub(super) source_count: usize,
    pub(super) included_observation: bool,
    pub(super) skipped_administrative: usize,
    pub(super) skipped_observation: usize,
    pub(super) skipped_ineligible: usize,
    pub(super) complete: bool,
    pub(super) selection_dropped_before_projection: bool,
    pub(super) capture_oldest_retained_sequence: Option<u64>,
    pub(super) capture_newest_retained_sequence: Option<u64>,
    pub(super) capture_dropped_before_retention: u64,
    pub(super) capture_dropped_before_projection: u64,
    pub(super) selection_truncated_by_retention: bool,
}

pub(super) async fn export_pipe_history(
    state: &Arc<SessionState>,
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
    include_observation: bool,
) -> Result<serde_json::Value, RubError> {
    let projection = build_export_projection(state, last, from, to, include_observation).await?;
    Ok(serde_json::json!({
        "subject": history_subject(last, from, to),
        "result": {
            "format": "pipe",
            "entries": projection.steps.iter().map(export_step_json).collect::<Vec<_>>(),
            "steps": projection
                .steps
                .iter()
                .map(replayable_export_step_json)
                .collect::<Vec<_>>(),
            "projection_state": workflow_export_projection_state_json(&projection),
            "selection": export_selection_json(last, from, to),
            "complete": projection.complete,
            "capture_window": history_capture_window_json(&projection),
            "included_observation": projection.included_observation,
            "source_count": projection.source_count,
            "skipped": {
                "administrative": projection.skipped_administrative,
                "observation": projection.skipped_observation,
                "ineligible": projection.skipped_ineligible,
            }
        }
    }))
}

pub(super) async fn export_script_history(
    state: &Arc<SessionState>,
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
    include_observation: bool,
) -> Result<serde_json::Value, RubError> {
    let projection = build_export_projection(state, last, from, to, include_observation).await?;
    let script = render_export_script(&projection)?;
    Ok(serde_json::json!({
        "subject": history_subject(last, from, to),
        "result": {
            "format": "script",
            "export": {
                "kind": "shell_script",
                "content": script,
            },
            "projection_state": workflow_export_projection_state_json(&projection),
            "selection": export_selection_json(last, from, to),
            "complete": projection.complete,
            "capture_window": history_capture_window_json(&projection),
            "included_observation": projection.included_observation,
            "source_count": projection.source_count,
            "skipped": {
                "administrative": projection.skipped_administrative,
                "observation": projection.skipped_observation,
                "ineligible": projection.skipped_ineligible,
            }
        }
    }))
}

async fn build_export_projection(
    state: &Arc<SessionState>,
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
    include_observation: bool,
) -> Result<WorkflowExportProjection, RubError> {
    let projection = if from.is_some() || to.is_some() {
        state.workflow_capture(usize::MAX).await
    } else {
        state.workflow_capture(last).await
    };
    let selection_truncated_by_retention =
        export_selection_is_truncated_by_retention(last, from, to, &projection);
    let selection_dropped_before_projection =
        export_selection_is_affected_by_projection_loss(last, from, to, &projection);
    let complete = !selection_truncated_by_retention && !selection_dropped_before_projection;
    let source_count = projection.entries.len();
    let mut steps = Vec::new();
    let mut skipped_administrative = 0usize;
    let mut skipped_observation = 0usize;
    let mut skipped_ineligible = 0usize;

    for entry in projection.entries {
        if !matches_export_range(entry.sequence, from, to) {
            continue;
        }
        if matches!(entry.capture_class, WorkflowCaptureClass::Administrative) {
            skipped_administrative += 1;
            continue;
        }
        if matches!(entry.capture_class, WorkflowCaptureClass::Observation) && !include_observation
        {
            skipped_observation += 1;
            continue;
        }
        if !entry.workflow_allowed {
            skipped_ineligible += 1;
            continue;
        }
        steps.push(export_step(entry));
    }

    Ok(WorkflowExportProjection {
        steps,
        source_count,
        included_observation: include_observation,
        skipped_administrative,
        skipped_observation,
        skipped_ineligible,
        complete,
        selection_dropped_before_projection,
        capture_oldest_retained_sequence: projection.oldest_retained_sequence,
        capture_newest_retained_sequence: projection.newest_retained_sequence,
        capture_dropped_before_retention: projection.dropped_before_retention,
        capture_dropped_before_projection: projection.dropped_before_projection,
        selection_truncated_by_retention,
    })
}

fn export_selection_is_truncated_by_retention(
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
    projection: &WorkflowCaptureProjection,
) -> bool {
    if projection.dropped_before_retention == 0 {
        return false;
    }

    let Some(oldest_retained_sequence) = projection.oldest_retained_sequence else {
        return false;
    };

    if from.is_some() || to.is_some() {
        let requested_start = from.unwrap_or(1);
        requested_start < oldest_retained_sequence
    } else {
        last > projection.entries.len()
    }
}

fn export_selection_is_affected_by_projection_loss(
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
    projection: &WorkflowCaptureProjection,
) -> bool {
    if projection.dropped_before_projection == 0 {
        return false;
    }

    if from.is_some() || to.is_some() {
        // Range selections address recorded workflow-capture sequence space; dropped
        // pre-projection commands never entered that sequence space and therefore
        // remain global metadata rather than selection-relative loss.
        return false;
    }

    last > projection.entries.len()
}

fn matches_export_range(sequence: u64, from: Option<u64>, to: Option<u64>) -> bool {
    if let Some(from) = from
        && sequence < from
    {
        return false;
    }
    if let Some(to) = to
        && sequence > to
    {
        return false;
    }
    true
}

fn export_step(entry: WorkflowCaptureEntry) -> WorkflowExportStep {
    WorkflowExportStep {
        command: entry.command,
        args: entry.args,
        source: serde_json::json!({
            "sequence": entry.sequence,
            "request_id": entry.request_id,
            "command_id": entry.command_id,
            "capture_class": entry.capture_class,
            "delivery_state": entry.delivery_state,
        }),
    }
}

fn export_step_json(step: &WorkflowExportStep) -> serde_json::Value {
    serde_json::json!({
        "command": step.command,
        "args": step.args,
        "source": step.source,
    })
}

fn replayable_export_step_json(step: &WorkflowExportStep) -> serde_json::Value {
    serde_json::json!({
        "command": step.command,
        "args": step.args,
    })
}

fn render_export_script(projection: &WorkflowExportProjection) -> Result<String, RubError> {
    let pipeline = projection
        .steps
        .iter()
        .map(|step| {
            serde_json::json!({
                "command": step.command,
                "args": step.args,
            })
        })
        .collect::<Vec<_>>();
    let pipeline_json = serde_json::to_string_pretty(&pipeline).map_err(RubError::from)?;
    let skipped = projection.skipped_administrative
        + projection.skipped_observation
        + projection.skipped_ineligible;

    let mut script = String::new();
    push_script_line(&mut script, "#!/usr/bin/env bash");
    push_script_line(&mut script, "# rub workflow - exported from history");
    push_script_line(
        &mut script,
        &format!(
            "# Source commands: {} ({} exported, {} skipped)",
            projection.source_count,
            projection.steps.len(),
            skipped
        ),
    );
    push_script_line(&mut script, "set -euo pipefail");
    script.push('\n');
    push_script_line(&mut script, "RUB=\"${RUB:-rub}\"");
    push_script_line(
        &mut script,
        "RUB_WORKFLOW_FILE=\"$(mktemp \"${TMPDIR:-/tmp}/rub-workflow.XXXXXX\")\"",
    );
    push_script_line(&mut script, "cleanup() { rm -f \"$RUB_WORKFLOW_FILE\"; }");
    push_script_line(&mut script, "trap cleanup EXIT");
    script.push('\n');
    push_script_line(&mut script, "cat >\"$RUB_WORKFLOW_FILE\" <<'RUB_PIPE_JSON'");
    push_script_line(&mut script, &pipeline_json);
    push_script_line(&mut script, "RUB_PIPE_JSON");
    push_script_line(&mut script, "\"$RUB\" pipe --file \"$RUB_WORKFLOW_FILE\"");

    Ok(script)
}

fn push_script_line(script: &mut String, line: &str) {
    script.push_str(line);
    script.push('\n');
}
