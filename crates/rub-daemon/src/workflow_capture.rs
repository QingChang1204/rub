use std::collections::VecDeque;

pub use crate::workflow_policy::WorkflowCaptureClass;
use crate::workflow_policy::{workflow_request_allowed, workflow_request_capture_class};
use rub_ipc::codec::MAX_FRAME_BYTES;
use rub_ipc::protocol::{IpcRequest, IpcResponse, ResponseStatus};

const WORKFLOW_CAPTURE_LIMIT: usize = 128;
const WORKFLOW_CAPTURE_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 4;

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn workflow_capture_delivery_delivered() -> WorkflowCaptureDeliveryState {
    WorkflowCaptureDeliveryState::Delivered
}

fn workflow_capture_delivery_is_delivered(value: &WorkflowCaptureDeliveryState) -> bool {
    matches!(value, WorkflowCaptureDeliveryState::Delivered)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCaptureDeliveryState {
    Delivered,
    DeliveryFailedAfterCommit,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowCaptureEntry {
    pub sequence: u64,
    pub command: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub args: serde_json::Value,
    pub capture_class: WorkflowCaptureClass,
    #[serde(default)]
    pub workflow_allowed: bool,
    #[serde(
        default = "workflow_capture_delivery_delivered",
        skip_serializing_if = "workflow_capture_delivery_is_delivered"
    )]
    pub delivery_state: WorkflowCaptureDeliveryState,
    pub timing: rub_core::model::Timing,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowCaptureProjection {
    pub entries: Vec<WorkflowCaptureEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_retained_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_retained_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub dropped_before_retention: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub dropped_before_projection: u64,
}

#[derive(Debug, Default)]
pub struct WorkflowCaptureState {
    next_sequence: u64,
    total_bytes: usize,
    entries: VecDeque<WorkflowCaptureStoredEntry>,
}

#[derive(Debug, Clone)]
struct WorkflowCaptureStoredEntry {
    entry: WorkflowCaptureEntry,
    approx_bytes: usize,
}

fn workflow_capture_entry_bytes(entry: &WorkflowCaptureEntry) -> usize {
    serde_json::to_vec(entry)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_FRAME_BYTES)
}

fn trim_workflow_capture_with_limits(
    total_bytes: &mut usize,
    entries: &mut VecDeque<WorkflowCaptureStoredEntry>,
    max_entries: usize,
    max_bytes: usize,
) {
    while entries.len() > max_entries || (*total_bytes > max_bytes && entries.len() > 1) {
        let Some(evicted) = entries.pop_front() else {
            break;
        };
        *total_bytes = total_bytes.saturating_sub(evicted.approx_bytes);
    }
}

impl WorkflowCaptureState {
    pub fn record(
        &mut self,
        request: &IpcRequest,
        response: &IpcResponse,
        delivery_state: WorkflowCaptureDeliveryState,
    ) {
        if !matches!(response.status, ResponseStatus::Success) {
            return;
        }

        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;

        let entry = WorkflowCaptureEntry {
            sequence,
            command: request.command.clone(),
            request_id: response.request_id.clone(),
            command_id: response.command_id.clone(),
            args: request.args.clone(),
            capture_class: workflow_request_capture_class(&request.command, &request.args),
            workflow_allowed: workflow_request_allowed(&request.command, &request.args),
            delivery_state,
            timing: response.timing,
        };
        let approx_bytes = workflow_capture_entry_bytes(&entry);
        self.total_bytes = self.total_bytes.saturating_add(approx_bytes);
        self.entries.push_back(WorkflowCaptureStoredEntry {
            entry,
            approx_bytes,
        });

        trim_workflow_capture_with_limits(
            &mut self.total_bytes,
            &mut self.entries,
            WORKFLOW_CAPTURE_LIMIT,
            WORKFLOW_CAPTURE_LIMIT_BYTES,
        );
    }

    pub fn projection(
        &self,
        last: usize,
        dropped_before_projection: u64,
    ) -> WorkflowCaptureProjection {
        let count = self.entries.len();
        let take = last.min(count);
        let mut entries = self
            .entries
            .iter()
            .rev()
            .take(take)
            .map(|entry| entry.entry.clone())
            .collect::<Vec<_>>();
        entries.reverse();
        let oldest_retained_sequence = self.entries.front().map(|entry| entry.entry.sequence);
        let newest_retained_sequence = self.entries.back().map(|entry| entry.entry.sequence);
        let dropped_before_retention = oldest_retained_sequence
            .map(|sequence| sequence.saturating_sub(1))
            .unwrap_or(0);
        WorkflowCaptureProjection {
            entries,
            oldest_retained_sequence,
            newest_retained_sequence,
            dropped_before_retention,
            dropped_before_projection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WORKFLOW_CAPTURE_LIMIT_BYTES, WorkflowCaptureClass, WorkflowCaptureDeliveryState,
        WorkflowCaptureEntry, WorkflowCaptureState, WorkflowCaptureStoredEntry,
        trim_workflow_capture_with_limits,
    };
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_core::model::Timing;
    use rub_ipc::protocol::IpcRequest;
    use std::collections::VecDeque;

    #[test]
    fn workflow_capture_records_successful_requests_with_classification() {
        let mut capture = WorkflowCaptureState::default();

        let pipe = IpcRequest::new(
            "pipe",
            serde_json::json!({
                "spec": "[]",
                "spec_source": {
                    "kind": "file",
                    "path": "/tmp/test.json",
                    "path_state": {
                        "truth_level": "input_path_reference",
                        "path_authority": "cli.pipe.spec_source.path",
                        "upstream_truth": "cli_pipe_file_option",
                        "path_kind": "workflow_spec_file",
                        "control_role": "display_only"
                    }
                }
            }),
            30_000,
        )
        .with_command_id("cmd-1")
        .expect("static command_id must be valid");
        let pipe_response = rub_ipc::protocol::IpcResponse::success("req-1", serde_json::json!({}))
            .with_command_id("cmd-1")
            .expect("static command_id must be valid");
        capture.record(
            &pipe,
            &pipe_response,
            WorkflowCaptureDeliveryState::Delivered,
        );

        let observe = IpcRequest::new("observe", serde_json::json!({ "limit": 5 }), 30_000);
        let observe_response =
            rub_ipc::protocol::IpcResponse::success("req-2", serde_json::json!({}));
        capture.record(
            &observe,
            &observe_response,
            WorkflowCaptureDeliveryState::Delivered,
        );

        let failed = IpcRequest::new("click", serde_json::json!({ "selector": "#go" }), 30_000);
        let failed_response = rub_ipc::protocol::IpcResponse::error(
            "req-3",
            ErrorEnvelope::new(ErrorCode::ElementNotFound, "missing"),
        );
        capture.record(
            &failed,
            &failed_response,
            WorkflowCaptureDeliveryState::Delivered,
        );

        let orchestration = IpcRequest::new(
            "orchestration",
            serde_json::json!({ "sub": "add", "spec": "{}" }),
            30_000,
        );
        let orchestration_response = rub_ipc::protocol::IpcResponse::success(
            "req-4",
            serde_json::json!({ "rule": { "id": 1 } }),
        );
        capture.record(
            &orchestration,
            &orchestration_response,
            WorkflowCaptureDeliveryState::Delivered,
        );

        let orchestration_trace = IpcRequest::new(
            "orchestration",
            serde_json::json!({ "sub": "trace", "last": 5 }),
            30_000,
        );
        let orchestration_trace_response = rub_ipc::protocol::IpcResponse::success(
            "req-5",
            serde_json::json!({ "trace": { "events": [] } }),
        );
        capture.record(
            &orchestration_trace,
            &orchestration_trace_response,
            WorkflowCaptureDeliveryState::Delivered,
        );

        let projection = capture.projection(10, 0);
        assert_eq!(projection.entries.len(), 4);
        assert_eq!(projection.entries[0].command, "pipe");
        assert_eq!(
            projection.entries[0].capture_class,
            WorkflowCaptureClass::Administrative
        );
        assert_eq!(
            projection.entries[0].delivery_state,
            WorkflowCaptureDeliveryState::Delivered
        );
        assert_eq!(
            projection.entries[0].args["spec_source"]["kind"],
            serde_json::json!("file")
        );
        assert_eq!(
            projection.entries[0].args["spec_source"]["path_state"]["path_authority"],
            serde_json::json!("cli.pipe.spec_source.path")
        );
        assert_eq!(
            projection.entries[1].capture_class,
            WorkflowCaptureClass::Observation
        );
        assert_eq!(projection.entries[2].command, "orchestration");
        assert_eq!(
            projection.entries[2].capture_class,
            WorkflowCaptureClass::Workflow
        );
        assert!(projection.entries[2].workflow_allowed);
        assert_eq!(
            projection.entries[3].capture_class,
            WorkflowCaptureClass::Observation
        );
        assert!(!projection.entries[3].workflow_allowed);
        assert_eq!(projection.oldest_retained_sequence, Some(1));
        assert_eq!(projection.newest_retained_sequence, Some(4));
        assert_eq!(projection.dropped_before_retention, 0);
        assert_eq!(projection.dropped_before_projection, 0);
    }

    #[test]
    fn workflow_capture_projection_reports_retention_truncation() {
        let mut capture = WorkflowCaptureState::default();

        for index in 0..130 {
            let request = IpcRequest::new("open", serde_json::json!({ "index": index }), 30_000);
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            capture.record(&request, &response, WorkflowCaptureDeliveryState::Delivered);
        }

        let projection = capture.projection(usize::MAX, 0);
        assert_eq!(projection.entries.len(), 128);
        assert_eq!(projection.oldest_retained_sequence, Some(3));
        assert_eq!(projection.newest_retained_sequence, Some(130));
        assert_eq!(projection.dropped_before_retention, 2);
        assert_eq!(projection.dropped_before_projection, 0);
    }

    #[test]
    fn workflow_capture_projection_enforces_byte_budget() {
        let mut total_bytes = 0usize;
        let mut entries = VecDeque::new();
        entries.push_back(WorkflowCaptureStoredEntry {
            entry: WorkflowCaptureEntry {
                sequence: 1,
                command: "pipe".to_string(),
                request_id: "req-a".to_string(),
                command_id: None,
                args: serde_json::json!({}),
                capture_class: WorkflowCaptureClass::Workflow,
                workflow_allowed: true,
                delivery_state: WorkflowCaptureDeliveryState::Delivered,
                timing: Timing::default(),
            },
            approx_bytes: (WORKFLOW_CAPTURE_LIMIT_BYTES / 2).max(1),
        });
        total_bytes += (WORKFLOW_CAPTURE_LIMIT_BYTES / 2).max(1);
        entries.push_back(WorkflowCaptureStoredEntry {
            entry: WorkflowCaptureEntry {
                sequence: 2,
                command: "pipe".to_string(),
                request_id: "req-b".to_string(),
                command_id: None,
                args: serde_json::json!({}),
                capture_class: WorkflowCaptureClass::Workflow,
                workflow_allowed: true,
                delivery_state: WorkflowCaptureDeliveryState::Delivered,
                timing: Timing::default(),
            },
            approx_bytes: (WORKFLOW_CAPTURE_LIMIT_BYTES / 2).max(1),
        });
        total_bytes += (WORKFLOW_CAPTURE_LIMIT_BYTES / 2).max(1);

        trim_workflow_capture_with_limits(&mut total_bytes, &mut entries, 128, 8);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.request_id, "req-b");
    }

    #[test]
    fn workflow_capture_records_daemon_committed_delivery_failure_with_honest_metadata() {
        let mut capture = WorkflowCaptureState::default();
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

        capture.record(
            &request,
            &response,
            WorkflowCaptureDeliveryState::DeliveryFailedAfterCommit,
        );

        let projection = capture.projection(10, 0);
        assert_eq!(projection.entries.len(), 1);
        assert_eq!(
            projection.entries[0].delivery_state,
            WorkflowCaptureDeliveryState::DeliveryFailedAfterCommit
        );
        assert_eq!(projection.entries[0].command, "open");
    }
}
