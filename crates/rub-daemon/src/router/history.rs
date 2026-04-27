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

    let projection = if from.is_some() || to.is_some() {
        state.command_history_range(from, to).await
    } else {
        state.command_history(last).await
    };
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
    use super::export::{WorkflowExportProjection, export_pipe_history, export_script_history};
    use super::projection::workflow_export_projection_state_json;
    use crate::session::SessionState;
    use crate::workflow_capture::WorkflowCaptureDeliveryState;
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
            exported["result"]["projection_state"]["surface"],
            serde_json::json!("command_history")
        );
        assert_eq!(
            exported["result"]["projection_state"]["projection_kind"],
            serde_json::json!("bounded_post_commit_projection")
        );
        assert_eq!(
            exported["result"]["projection_state"]["truth_level"],
            serde_json::json!("operator_projection")
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
            exported["result"]["projection_state"]["control_role"],
            serde_json::json!("display_only")
        );
        assert_eq!(
            exported["result"]["projection_state"]["durability"],
            serde_json::json!("best_effort")
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
            exported["result"]["projection_state"]["surface"],
            serde_json::json!("workflow_capture_export")
        );
        assert_eq!(
            exported["result"]["projection_state"]["projection_authority"],
            serde_json::json!("session.workflow_capture")
        );
        assert_eq!(
            exported["result"]["projection_state"]["truth_level"],
            serde_json::json!("operator_projection")
        );
        assert_eq!(
            exported["result"]["projection_state"]["upstream_commit_truth"],
            serde_json::json!("daemon_response_committed")
        );
        assert_eq!(
            exported["result"]["projection_state"]["control_role"],
            serde_json::json!("display_only")
        );
        assert_eq!(
            exported["result"]["projection_state"]["durability"],
            serde_json::json!("best_effort")
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
    async fn command_history_non_export_mode_honors_from_to_range() {
        let home =
            std::env::temp_dir().join(format!("rub-history-export-{}-range", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        for index in 0..4 {
            let request = IpcRequest::new(
                "open",
                serde_json::json!({ "url": format!("https://example.com/{index}") }),
                1_000,
            );
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            state.record_command_history(&request, &response).await;
        }

        let history = cmd_history(&serde_json::json!({ "from": 2, "to": 3 }), &state)
            .await
            .expect("history command succeeds");
        let items = history["result"]["items"]
            .as_array()
            .expect("history items should be an array");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["sequence"], serde_json::json!(2));
        assert_eq!(items[1]["sequence"], serde_json::json!(3));
    }

    #[tokio::test]
    async fn command_history_projection_state_ignores_global_projection_loss_when_last_window_is_complete()
     {
        let home = std::env::temp_dir().join(format!(
            "rub-history-export-{}-non-export-last-projection-loss",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        for index in 0..300 {
            let request = IpcRequest::new(
                "open",
                serde_json::json!({
                    "url": format!("https://example.test/{index}")
                }),
                30_000,
            );
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            state.submit_post_commit_projection(&request, &response);
        }

        let history = cmd_history(&serde_json::json!({ "last": 5 }), &state)
            .await
            .expect("history command succeeds");

        assert!(
            history["result"]["dropped_before_projection"]
                .as_u64()
                .expect("projection drop count must be present")
                > 0
        );
        assert_eq!(
            history["result"]["projection_state"]["lossy"],
            serde_json::json!(false)
        );
        assert_eq!(
            history["result"]["projection_state"]["lossy_reasons"],
            serde_json::json!([])
        );
    }

    #[tokio::test]
    async fn command_history_projection_state_ignores_global_projection_loss_when_range_window_is_complete()
     {
        let home = std::env::temp_dir().join(format!(
            "rub-history-export-{}-non-export-range-projection-loss",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        for index in 0..300 {
            let request = IpcRequest::new(
                "open",
                serde_json::json!({
                    "url": format!("https://example.test/{index}")
                }),
                30_000,
            );
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            state.submit_post_commit_projection(&request, &response);
        }

        let retained = state.command_history(5).await;
        let from = retained
            .entries
            .first()
            .map(|entry| entry.sequence)
            .expect("retained history should not be empty");
        let to = retained
            .entries
            .last()
            .map(|entry| entry.sequence)
            .expect("retained history should not be empty");

        let history = cmd_history(&serde_json::json!({ "from": from, "to": to }), &state)
            .await
            .expect("history command succeeds");

        assert!(
            history["result"]["dropped_before_projection"]
                .as_u64()
                .expect("projection drop count must be present")
                > 0
        );
        assert_eq!(
            history["result"]["projection_state"]["lossy"],
            serde_json::json!(false)
        );
        assert_eq!(
            history["result"]["projection_state"]["lossy_reasons"],
            serde_json::json!([])
        );
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
    async fn export_pipe_history_surfaces_delivery_failed_after_commit_source_metadata() {
        let home = std::env::temp_dir().join(format!(
            "rub-history-export-{}-delivery-failure",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        let request = IpcRequest::new(
            "open",
            serde_json::json!({ "url": "https://example.com" }),
            1_000,
        )
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
        let response =
            rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({ "ok": true }))
                .with_command_id("cmd-1")
                .expect("static command_id must be valid");
        state
            .record_workflow_capture_with_state(
                &request,
                &response,
                WorkflowCaptureDeliveryState::DeliveryFailedAfterCommit,
            )
            .await;

        let exported = export_pipe_history(&state, 10, None, None, false)
            .await
            .expect("export succeeds");
        assert_eq!(
            exported["result"]["entries"][0]["source"]["delivery_state"],
            serde_json::json!("delivery_failed_after_commit")
        );
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
            serde_json::json!(["retention_truncated"])
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

    #[tokio::test]
    async fn export_history_marks_projection_loss_as_incomplete() {
        let home = std::env::temp_dir().join(format!(
            "rub-history-export-{}-projection-loss",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create home");
        let state = Arc::new(SessionState::new("default", home, None));

        for index in 0..300 {
            let request = IpcRequest::new(
                "open",
                serde_json::json!({
                    "url": format!("https://example.test/{index}")
                }),
                30_000,
            );
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            state.submit_post_commit_projection(&request, &response);
        }

        let exported = export_pipe_history(&state, 5, None, None, false)
            .await
            .expect("export succeeds");

        assert_eq!(exported["result"]["complete"], serde_json::json!(true));
        assert!(
            exported["result"]["capture_window"]["dropped_before_projection"]
                .as_u64()
                .expect("projection drop count must be present")
                > 0
        );
        assert_eq!(
            exported["result"]["capture_window"]["truncated"],
            serde_json::json!(false)
        );
        assert_eq!(
            exported["result"]["projection_state"]["lossy_reasons"],
            serde_json::json!([])
        );
    }

    #[test]
    fn workflow_export_projection_state_distinguishes_projection_loss_from_retention_truncation() {
        let projection = WorkflowExportProjection {
            steps: Vec::new(),
            source_count: 0,
            included_observation: false,
            skipped_administrative: 0,
            skipped_observation: 0,
            skipped_ineligible: 0,
            complete: false,
            selection_dropped_before_projection: false,
            capture_oldest_retained_sequence: Some(42),
            capture_newest_retained_sequence: Some(99),
            capture_dropped_before_retention: 0,
            capture_dropped_before_projection: 3,
            selection_truncated_by_retention: false,
        };

        assert_eq!(
            workflow_export_projection_state_json(&projection)["lossy_reasons"],
            serde_json::json!([])
        );
        assert_eq!(
            workflow_export_projection_state_json(&projection)["global_projection_drop_count"],
            serde_json::json!(3)
        );
    }

    #[test]
    fn workflow_export_projection_state_marks_retention_truncation_only_when_retained_window_is_short()
     {
        let projection = WorkflowExportProjection {
            steps: Vec::new(),
            source_count: 0,
            included_observation: false,
            skipped_administrative: 0,
            skipped_observation: 0,
            skipped_ineligible: 0,
            complete: false,
            selection_dropped_before_projection: false,
            capture_oldest_retained_sequence: Some(6),
            capture_newest_retained_sequence: Some(12),
            capture_dropped_before_retention: 5,
            capture_dropped_before_projection: 0,
            selection_truncated_by_retention: true,
        };

        assert_eq!(
            workflow_export_projection_state_json(&projection)["lossy_reasons"],
            serde_json::json!(["retention_truncated"])
        );
    }

    #[test]
    fn workflow_export_projection_state_ignores_global_retention_loss_when_selection_is_complete() {
        let projection = WorkflowExportProjection {
            steps: Vec::new(),
            source_count: 0,
            included_observation: false,
            skipped_administrative: 0,
            skipped_observation: 0,
            skipped_ineligible: 0,
            complete: true,
            selection_dropped_before_projection: false,
            capture_oldest_retained_sequence: Some(6),
            capture_newest_retained_sequence: Some(12),
            capture_dropped_before_retention: 5,
            capture_dropped_before_projection: 0,
            selection_truncated_by_retention: false,
        };

        assert_eq!(
            workflow_export_projection_state_json(&projection)["lossy_reasons"],
            serde_json::json!([])
        );
    }

    #[test]
    fn workflow_export_projection_state_marks_selection_relative_projection_loss_only_when_selected_window_is_short()
     {
        let projection = WorkflowExportProjection {
            steps: Vec::new(),
            source_count: 0,
            included_observation: false,
            skipped_administrative: 0,
            skipped_observation: 0,
            skipped_ineligible: 0,
            complete: false,
            selection_dropped_before_projection: true,
            capture_oldest_retained_sequence: Some(42),
            capture_newest_retained_sequence: Some(99),
            capture_dropped_before_retention: 0,
            capture_dropped_before_projection: 3,
            selection_truncated_by_retention: false,
        };

        assert_eq!(
            workflow_export_projection_state_json(&projection)["lossy_reasons"],
            serde_json::json!(["dropped_before_projection"])
        );
        assert_eq!(
            workflow_export_projection_state_json(&projection)["global_projection_drop_count"],
            serde_json::json!(3)
        );
    }
}
