use std::collections::VecDeque;

use rub_core::model::Timing;
use rub_ipc::protocol::{IpcRequest, IpcResponse, ResponseStatus};

const HISTORY_LIMIT: usize = 64;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandHistoryEntry {
    pub sequence: u64,
    pub command: String,
    pub success: bool,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub timing: Timing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_kind: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandHistoryProjection {
    pub entries: Vec<CommandHistoryEntry>,
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
pub struct CommandHistoryState {
    next_sequence: u64,
    entries: VecDeque<CommandHistoryEntry>,
}

impl CommandHistoryState {
    pub fn record(&mut self, request: &IpcRequest, response: &IpcResponse) {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;

        let (confirmation_status, confirmation_kind) = interaction_confirmation(response);
        let error_code = response.error.as_ref().map(|error| error.code.to_string());
        let summary = summarize_entry(
            response,
            confirmation_status.as_deref(),
            confirmation_kind.as_deref(),
        );

        self.entries.push_back(CommandHistoryEntry {
            sequence,
            command: request.command.clone(),
            success: matches!(response.status, ResponseStatus::Success),
            request_id: response.request_id.clone(),
            command_id: response.command_id.clone(),
            timing: response.timing,
            summary,
            error_code,
            confirmation_status,
            confirmation_kind,
        });

        while self.entries.len() > HISTORY_LIMIT {
            self.entries.pop_front();
        }
    }

    pub fn projection(
        &self,
        last: usize,
        dropped_before_projection: u64,
    ) -> CommandHistoryProjection {
        let count = self.entries.len();
        let take = last.min(count);
        let mut entries = self
            .entries
            .iter()
            .rev()
            .take(take)
            .cloned()
            .collect::<Vec<_>>();
        entries.reverse();
        let oldest_retained_sequence = self.entries.front().map(|entry| entry.sequence);
        let newest_retained_sequence = self.entries.back().map(|entry| entry.sequence);
        let dropped_before_retention = oldest_retained_sequence
            .map(|sequence| sequence.saturating_sub(1))
            .unwrap_or(0);
        CommandHistoryProjection {
            entries,
            oldest_retained_sequence,
            newest_retained_sequence,
            dropped_before_retention,
            dropped_before_projection,
        }
    }
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn interaction_confirmation(response: &IpcResponse) -> (Option<String>, Option<String>) {
    let interaction = response
        .data
        .as_ref()
        .and_then(|data| data.get("interaction"))
        .and_then(|value| value.as_object());
    let status = interaction
        .and_then(|value| value.get("confirmation_status"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let kind = interaction
        .and_then(|value| value.get("confirmation_kind"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    (status, kind)
}

fn summarize_entry(
    response: &IpcResponse,
    confirmation_status: Option<&str>,
    confirmation_kind: Option<&str>,
) -> Option<String> {
    if let Some(code) = response.error.as_ref().map(|error| error.code.to_string()) {
        return Some(code);
    }

    if let Some(status) = confirmation_status {
        if let Some(kind) = confirmation_kind {
            return Some(format!("{status}/{kind}"));
        }
        return Some(status.to_string());
    }

    matches!(response.status, ResponseStatus::Success).then_some("success".to_string())
}

#[cfg(test)]
mod tests {
    use super::CommandHistoryState;
    use rub_core::error::{ErrorCode, ErrorEnvelope};
    use rub_ipc::protocol::IpcRequest;

    #[test]
    fn history_records_success_and_error_summaries() {
        let mut history = CommandHistoryState::default();
        let request = IpcRequest::new("click", serde_json::json!({}), 1_000);
        let response = rub_ipc::protocol::IpcResponse::success(
            "req-1",
            serde_json::json!({
                "interaction": {
                    "confirmation_status": "confirmed",
                    "confirmation_kind": "page_mutation"
                }
            }),
        );
        history.record(&request, &response);

        let error_request = IpcRequest::new("wait", serde_json::json!({}), 1_000);
        let error_response = rub_ipc::protocol::IpcResponse::error(
            "req-2",
            ErrorEnvelope::new(ErrorCode::WaitTimeout, "timed out"),
        );
        history.record(&error_request, &error_response);

        let projection = history.projection(10, 0);
        assert_eq!(projection.entries.len(), 2);
        assert_eq!(projection.oldest_retained_sequence, Some(1));
        assert_eq!(projection.newest_retained_sequence, Some(2));
        assert_eq!(projection.dropped_before_retention, 0);
        assert_eq!(projection.dropped_before_projection, 0);
        assert_eq!(
            projection.entries[0].summary.as_deref(),
            Some("confirmed/page_mutation")
        );
        assert_eq!(
            projection.entries[1].error_code.as_deref(),
            Some("WAIT_TIMEOUT")
        );
    }

    #[test]
    fn history_projection_reports_retention_truncation() {
        let mut history = CommandHistoryState::default();

        for index in 0..70 {
            let request = IpcRequest::new("open", serde_json::json!({ "index": index }), 1_000);
            let response = rub_ipc::protocol::IpcResponse::success(
                format!("req-{index}"),
                serde_json::json!({}),
            );
            history.record(&request, &response);
        }

        let projection = history.projection(usize::MAX, 0);
        assert_eq!(projection.entries.len(), 64);
        assert_eq!(projection.oldest_retained_sequence, Some(7));
        assert_eq!(projection.newest_retained_sequence, Some(70));
        assert_eq!(projection.dropped_before_retention, 6);
        assert_eq!(projection.dropped_before_projection, 0);
    }
}
