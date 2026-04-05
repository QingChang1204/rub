use std::sync::Arc;

use crate::session::SessionState;
use crate::workflow_capture::{WorkflowCaptureClass, WorkflowCaptureEntry};
use rub_core::error::{ErrorCode, RubError};
use serde_json::Value;

#[derive(Debug, Clone)]
struct WorkflowExportStep {
    command: String,
    args: Value,
    source: Value,
}

#[derive(Debug)]
struct WorkflowExportProjection {
    steps: Vec<WorkflowExportStep>,
    source_count: usize,
    included_observation: bool,
    skipped_administrative: usize,
    skipped_observation: usize,
    skipped_ineligible: usize,
    complete: bool,
    capture_oldest_retained_sequence: Option<u64>,
    capture_newest_retained_sequence: Option<u64>,
    capture_dropped_before_retention: u64,
    capture_dropped_before_projection: u64,
}

pub(super) async fn cmd_history(
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let last = args
        .get("last")
        .and_then(|value| value.as_u64())
        .unwrap_or(10) as usize;
    let export_pipe = args
        .get("export_pipe")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let export_script = args
        .get("export_script")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let from = args.get("from").and_then(|value| value.as_u64());
    let to = args.get("to").and_then(|value| value.as_u64());
    let include_observation = args
        .get("include_observation")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    if export_pipe && export_script {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "history export requires exactly one format: --export-pipe or --export-script",
        ));
    }
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "--from cannot be greater than --to",
        ));
    }
    if export_pipe {
        return export_pipe_history(state, last, from, to, include_observation).await;
    }
    if export_script {
        return export_script_history(state, last, from, to, include_observation).await;
    }

    let projection = state.command_history(last).await;
    let items = serde_json::to_value(&projection.entries).map_err(RubError::from)?;
    Ok(serde_json::json!({
        "subject": history_subject(last, from, to),
        "result": {
            "items": items,
            "dropped_before_projection": projection.dropped_before_projection,
        }
    }))
}

async fn export_pipe_history(
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

async fn export_script_history(
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

fn history_subject(last: usize, from: Option<u64>, to: Option<u64>) -> serde_json::Value {
    serde_json::json!({
        "kind": "command_history",
        "selection": export_selection_json(last, from, to),
    })
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
    let complete = export_selection_is_complete(last, from, to, &projection);
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
        capture_oldest_retained_sequence: projection.oldest_retained_sequence,
        capture_newest_retained_sequence: projection.newest_retained_sequence,
        capture_dropped_before_retention: projection.dropped_before_retention,
        capture_dropped_before_projection: projection.dropped_before_projection,
    })
}

fn export_selection_is_complete(
    last: usize,
    from: Option<u64>,
    to: Option<u64>,
    projection: &crate::workflow_capture::WorkflowCaptureProjection,
) -> bool {
    if projection.dropped_before_retention == 0 {
        return true;
    }

    let Some(oldest_retained_sequence) = projection.oldest_retained_sequence else {
        return true;
    };

    if from.is_some() || to.is_some() {
        let requested_start = from.unwrap_or(1);
        requested_start >= oldest_retained_sequence
    } else {
        last <= projection.entries.len()
    }
}

fn history_capture_window_json(projection: &WorkflowExportProjection) -> serde_json::Value {
    serde_json::json!({
        "oldest_retained_sequence": projection.capture_oldest_retained_sequence,
        "newest_retained_sequence": projection.capture_newest_retained_sequence,
        "dropped_before_retention": projection.capture_dropped_before_retention,
        "dropped_before_projection": projection.capture_dropped_before_projection,
        "truncated": !projection.complete,
    })
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

fn export_selection_json(last: usize, from: Option<u64>, to: Option<u64>) -> serde_json::Value {
    if from.is_some() || to.is_some() {
        serde_json::json!({
            "from": from,
            "to": to,
        })
    } else {
        serde_json::json!({
            "last": last,
        })
    }
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

#[cfg(test)]
mod tests {
    use super::{export_pipe_history, export_script_history};
    use crate::session::SessionState;
    use rub_ipc::protocol::IpcRequest;
    use std::sync::Arc;

    #[tokio::test]
    async fn export_pipe_history_filters_admin_and_observation_by_default() {
        let home =
            std::env::temp_dir().join(format!("rub-history-export-{}-default", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        let open = IpcRequest::new(
            "open",
            serde_json::json!({ "url": "https://example.com" }),
            1_000,
        );
        let open_response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));
        state.record_workflow_capture(&open, &open_response).await;

        let observe = IpcRequest::new("observe", serde_json::json!({ "limit": 5 }), 1_000);
        let observe_response =
            rub_ipc::protocol::IpcResponse::success("req-2", serde_json::json!({}));
        state
            .record_workflow_capture(&observe, &observe_response)
            .await;

        let close = IpcRequest::new("close", serde_json::json!({}), 1_000);
        let close_response =
            rub_ipc::protocol::IpcResponse::success("req-3", serde_json::json!({}));
        state.record_workflow_capture(&close, &close_response).await;

        let exported = export_pipe_history(&state, 10, None, None, false)
            .await
            .expect("export succeeds");
        assert_eq!(
            exported["result"]["entries"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            1
        );
        assert_eq!(exported["result"]["entries"][0]["command"], "open");
        assert_eq!(exported["result"]["skipped"]["observation"], 1);
        assert_eq!(exported["result"]["skipped"]["administrative"], 1);
    }

    #[tokio::test]
    async fn export_pipe_history_redacts_args_with_current_secret_sources() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let home = std::env::temp_dir()
                .join(format!("rub-history-export-{}-redact", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).expect("create home");
            let secrets = home.join("secrets.env");
            std::fs::write(&secrets, "RUB_TOKEN=token-123\n").expect("write secrets");
            std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600))
                .expect("set permissions");

            let state = Arc::new(SessionState::new("default", home, None));
            let request = IpcRequest::new(
                "type",
                serde_json::json!({ "selector": "#password", "text": "token-123", "clear": true }),
                1_000,
            );
            let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));
            state.record_workflow_capture(&request, &response).await;

            let exported = export_pipe_history(&state, 10, None, None, false)
                .await
                .expect("export succeeds");
            assert_eq!(
                exported["result"]["entries"][0]["args"]["text"],
                serde_json::json!("$RUB_TOKEN")
            );
        }
    }

    #[tokio::test]
    async fn export_script_history_wraps_replayable_pipe_file() {
        let home =
            std::env::temp_dir().join(format!("rub-history-export-{}-script", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        let open = IpcRequest::new(
            "open",
            serde_json::json!({ "url": "https://example.com" }),
            1_000,
        );
        let open_response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));
        state.record_workflow_capture(&open, &open_response).await;

        let exported = export_script_history(&state, 10, None, None, false)
            .await
            .expect("export succeeds");
        let script = exported["result"]["export"]["content"]
            .as_str()
            .expect("script string");

        assert_eq!(exported["result"]["format"], "script");
        assert!(script.contains("rub workflow - exported from history"));
        assert!(script.contains("\"$RUB\" pipe --file \"$RUB_WORKFLOW_FILE\""));
        assert!(script.contains("\"command\": \"open\""));
        assert!(script.contains("mktemp \"${TMPDIR:-/tmp}/rub-workflow.XXXXXX\""));
        assert!(!script.contains("XXXXXX.json"));
    }

    #[tokio::test]
    async fn export_pipe_history_supports_sequence_range_filters() {
        let home =
            std::env::temp_dir().join(format!("rub-history-export-{}-range", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        for (command, request_id) in [("open", "req-1"), ("type", "req-2"), ("click", "req-3")] {
            let request = IpcRequest::new(command, serde_json::json!({}), 1_000);
            let response =
                rub_ipc::protocol::IpcResponse::success(request_id, serde_json::json!({}));
            state.record_workflow_capture(&request, &response).await;
        }

        let exported = export_pipe_history(&state, 10, Some(2), Some(3), false)
            .await
            .expect("export succeeds");
        assert_eq!(exported["subject"]["selection"]["from"], 2);
        assert_eq!(exported["subject"]["selection"]["to"], 3);
        assert_eq!(
            exported["result"]["entries"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            2
        );
        assert_eq!(exported["result"]["entries"][0]["command"], "type");
        assert_eq!(exported["result"]["entries"][1]["command"], "click");
    }

    #[tokio::test]
    async fn export_history_reports_truncation_when_requested_range_exceeds_retained_capture() {
        let home = std::env::temp_dir().join(format!(
            "rub-history-export-{}-truncated",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        for index in 0..130 {
            let request = IpcRequest::new("click", serde_json::json!({ "index": index }), 1_000);
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            state.record_workflow_capture(&request, &response).await;
        }

        let exported = export_pipe_history(&state, 200, None, None, false)
            .await
            .expect("export succeeds");
        assert_eq!(exported["result"]["complete"], serde_json::json!(false));
        assert_eq!(
            exported["result"]["capture_window"]["oldest_retained_sequence"],
            serde_json::json!(3)
        );
        assert_eq!(
            exported["result"]["capture_window"]["dropped_before_retention"],
            serde_json::json!(2)
        );
        assert_eq!(
            exported["result"]["capture_window"]["truncated"],
            serde_json::json!(true)
        );
    }
}
