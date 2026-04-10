use std::sync::Arc;

use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};

mod export;
mod projection;

use self::export::{export_pipe_history, export_script_history};
use self::projection::{
    command_history_projection_state_json, command_history_retention_window_json, history_subject,
};

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
            "projection_state": command_history_projection_state_json(&projection),
            "retention_window": command_history_retention_window_json(&projection),
            "dropped_before_projection": projection.dropped_before_projection,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::cmd_history;
    use super::export::{export_pipe_history, export_script_history};
    use crate::session::SessionState;
    use rub_ipc::protocol::IpcRequest;
    use std::sync::Arc;

    #[tokio::test]
    async fn command_history_marks_surface_as_bounded_post_commit_projection() {
        let home =
            std::env::temp_dir().join(format!("rub-history-export-{}-surface", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        let request = IpcRequest::new(
            "open",
            serde_json::json!({ "url": "https://example.com" }),
            1_000,
        );
        let response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));
        state.record_command_history(&request, &response).await;

        let exported = cmd_history(&serde_json::json!({ "last": 10 }), &state)
            .await
            .expect("history command succeeds");

        assert_eq!(
            exported["result"]["projection_state"]["projection_kind"],
            serde_json::json!("bounded_post_commit_projection")
        );
        assert_eq!(
            exported["result"]["projection_state"]["projection_authority"],
            serde_json::json!("session.history")
        );
        assert_eq!(
            exported["result"]["projection_state"]["upstream_commit_truth"],
            serde_json::json!("daemon_response_committed")
        );
        assert_eq!(
            exported["result"]["projection_state"]["lossy"],
            serde_json::json!(false)
        );
        assert_eq!(
            exported["result"]["retention_window"]["oldest_retained_sequence"],
            serde_json::json!(1)
        );
    }

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
            exported["result"]["projection_state"]["projection_authority"],
            serde_json::json!("session.workflow_capture")
        );
        assert_eq!(
            exported["result"]["projection_state"]["upstream_commit_truth"],
            serde_json::json!("daemon_response_committed")
        );
        assert_eq!(
            exported["result"]["entries"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            1
        );
        assert_eq!(exported["result"]["entries"][0]["command"], "open");
        assert_eq!(
            exported["result"]["steps"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or_default(),
            1
        );
        assert_eq!(exported["result"]["steps"][0]["command"], "open");
        assert!(exported["result"]["steps"][0].get("source").is_none());
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

        let extract = IpcRequest::new(
            "extract",
            serde_json::json!({
                "spec": "{}",
                "spec_source": {
                    "kind": "file",
                    "path": "/tmp/extract.json",
                    "path_state": {
                        "truth_level": "input_path_reference",
                        "path_authority": "cli.extract.spec_source.path",
                        "upstream_truth": "cli_extract_file_option",
                        "path_kind": "json_spec_file",
                        "control_role": "display_only"
                    }
                }
            }),
            1_000,
        );
        let extract_response =
            rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}));
        state
            .record_workflow_capture(&extract, &extract_response)
            .await;

        let exported = export_script_history(&state, 10, None, None, false)
            .await
            .expect("export succeeds");
        let script = exported["result"]["export"]["content"]
            .as_str()
            .expect("script string");

        assert_eq!(exported["result"]["format"], "script");
        assert!(script.contains("rub workflow - exported from history"));
        assert!(script.contains("\"$RUB\" pipe --file \"$RUB_WORKFLOW_FILE\""));
        assert!(script.contains("\"command\": \"extract\""));
        assert!(script.contains("\"path_authority\": \"cli.extract.spec_source.path\""));
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
        assert_eq!(
            exported["result"]["projection_state"]["lossy"],
            serde_json::json!(true)
        );
        assert_eq!(
            exported["result"]["projection_state"]["lossy_reasons"],
            serde_json::json!(["dropped_before_retention", "retention_truncated"])
        );
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
