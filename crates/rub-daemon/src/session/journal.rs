use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use rub_core::fs::sync_parent_dir;
use rub_ipc::protocol::{IpcRequest, IpcResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::SessionState;
use crate::workflow_capture::WorkflowCaptureDeliveryState;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PostCommitJournalEntry {
    journal_state: serde_json::Value,
    session_id: String,
    session_name: String,
    command: String,
    command_id: Option<String>,
    request_id: String,
    request: serde_json::Value,
    response: serde_json::Value,
    #[serde(
        default = "workflow_capture_delivery_delivered",
        skip_serializing_if = "workflow_capture_delivery_is_delivered"
    )]
    delivery_state: WorkflowCaptureDeliveryState,
    request_redaction_lossy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_redaction_reason: Option<String>,
    response_redaction_lossy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_redaction_reason: Option<String>,
}

#[derive(Debug)]
struct RedactedJournalPayload {
    value: Value,
    lossy: bool,
    reason: Option<String>,
}

impl PostCommitJournalEntry {
    fn from_request_response(
        state: &SessionState,
        request: &IpcRequest,
        response: &IpcResponse,
        delivery_state: WorkflowCaptureDeliveryState,
    ) -> io::Result<Self> {
        let redacted_request = redacted_post_commit_request_with_status(request, &state.rub_home);
        let request_json = serde_json::to_value(&redacted_request.request)
            .map_err(|error| io::Error::other(format!("serialize journal request: {error}")))?;
        let response_json = redacted_post_commit_response(response, &state.rub_home)?;
        Ok(Self {
            journal_state: post_commit_journal_state_json(),
            session_id: state.session_id.clone(),
            session_name: state.session_name.clone(),
            command: redacted_request.request.command,
            command_id: response
                .command_id
                .clone()
                .or_else(|| redacted_request.request.command_id.clone()),
            request_id: response.request_id.clone(),
            request: request_json,
            response: response_json.value,
            delivery_state,
            request_redaction_lossy: redacted_request.lossy,
            request_redaction_reason: redacted_request.reason,
            response_redaction_lossy: response_json.lossy,
            response_redaction_reason: response_json.reason,
        })
    }
}

fn workflow_capture_delivery_delivered() -> WorkflowCaptureDeliveryState {
    WorkflowCaptureDeliveryState::Delivered
}

fn workflow_capture_delivery_is_delivered(value: &WorkflowCaptureDeliveryState) -> bool {
    matches!(value, WorkflowCaptureDeliveryState::Delivered)
}

fn post_commit_journal_state_json() -> Value {
    json!({
        "surface": "post_commit_journal",
        "visibility": "internal_only",
        "recovery_role": "daemon_recovery_writer",
        "upstream_commit_truth": "daemon_response_committed",
        "delivery_state_contract": "sibling_post_commit_delivery_state",
        "commit_relation": "downstream_of_daemon_commit_fence",
        "durability": "durable",
        "retention_scope": "session_runtime_cleanup",
        "reader_contract": "no_public_api",
    })
}

fn append_durable_journal_entry(path: &Path, entry: &PostCommitJournalEntry) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, entry)
        .map_err(|error| io::Error::other(format!("serialize journal line: {error}")))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    sync_parent_dir(path)?;
    Ok(())
}

fn redacted_journal_payload(
    mut value: Value,
    rub_home: &Path,
    surface: &'static str,
) -> RedactedJournalPayload {
    match crate::router::secret_resolution::redact_json_value_from_secret_sources(
        &mut value, rub_home,
    ) {
        Ok(metadata) => {
            crate::router::secret_resolution::redact_json_value(&mut value, &metadata);
            RedactedJournalPayload {
                value,
                lossy: false,
                reason: None,
            }
        }
        Err(error) => RedactedJournalPayload {
            value: json!({
                "surface": surface,
                "reason": "secret_redaction_unavailable",
                "redacted": true,
            }),
            lossy: true,
            reason: Some(error.to_string()),
        },
    }
}

struct RedactedPostCommitRequest {
    request: IpcRequest,
    lossy: bool,
    reason: Option<String>,
}

fn redacted_post_commit_request_with_status(
    request: &IpcRequest,
    rub_home: &Path,
) -> RedactedPostCommitRequest {
    let mut captured_request = request.clone();
    let payload = redacted_journal_payload(captured_request.args.clone(), rub_home, "request");
    captured_request.args = payload.value;
    RedactedPostCommitRequest {
        request: captured_request,
        lossy: payload.lossy,
        reason: payload.reason,
    }
}

pub(crate) fn redacted_post_commit_request(request: &IpcRequest, rub_home: &Path) -> IpcRequest {
    redacted_post_commit_request_with_status(request, rub_home).request
}

fn redacted_post_commit_response(
    response: &IpcResponse,
    rub_home: &Path,
) -> io::Result<RedactedJournalPayload> {
    let response_json = serde_json::to_value(response)
        .map_err(|error| io::Error::other(format!("serialize journal response: {error}")))?;
    Ok(redacted_journal_payload(
        response_json,
        rub_home,
        "response",
    ))
}

#[cfg(test)]
fn read_durable_journal_entries(path: &Path) -> io::Result<Vec<serde_json::Value>> {
    let bytes = std::fs::read(path)?;
    let contents = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let last_index = lines.len().saturating_sub(1);
    let mut entries = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) => entries.push(value),
            Err(_error) if index == last_index => break,
            Err(error) => {
                return Err(io::Error::other(format!(
                    "decode post-commit journal line: {error}"
                )));
            }
        }
    }
    Ok(entries)
}

impl SessionState {
    fn post_commit_journal_path(&self) -> PathBuf {
        crate::rub_paths::RubPaths::new(&self.rub_home)
            .session_runtime(&self.session_name, &self.session_id)
            .post_commit_journal_path()
    }

    pub(crate) async fn record_post_commit_journal(
        &self,
        request: &IpcRequest,
        response: &IpcResponse,
        delivery_state: WorkflowCaptureDeliveryState,
    ) -> io::Result<()> {
        // This journal is an internal recovery writer downstream of the daemon
        // response commit fence. It must never redefine public commit truth.
        let _append_guard = self.post_commit_journal_append.lock().await;
        #[cfg(test)]
        while self.post_commit_journal_blocked.load(Ordering::SeqCst) {
            self.post_commit_journal_block_notify.notified().await;
        }
        #[cfg(test)]
        if self
            .post_commit_journal_force_failure_once
            .swap(false, Ordering::SeqCst)
        {
            self.post_commit_journal_failures
                .fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::other("forced post-commit journal failure"));
        }

        let entry =
            PostCommitJournalEntry::from_request_response(self, request, response, delivery_state)?;
        let path = self.post_commit_journal_path();
        match tokio::task::spawn_blocking(move || append_durable_journal_entry(&path, &entry)).await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.post_commit_journal_failures
                    .fetch_add(1, Ordering::SeqCst);
                Err(error)
            }
            Err(error) => {
                self.post_commit_journal_failures
                    .fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other(format!(
                    "post-commit journal task failed: {error}"
                )))
            }
        }
    }

    pub(crate) fn post_commit_journal_failure_count(&self) -> u64 {
        self.post_commit_journal_failures.load(Ordering::SeqCst)
    }

    pub(crate) fn post_commit_journal_projection(&self) -> serde_json::Value {
        let failure_count = self.post_commit_journal_failure_count();
        serde_json::json!({
            "surface": "post_commit_journal",
            "authority": "session.post_commit_journal",
            "status": if failure_count == 0 { "active" } else { "degraded" },
            "failure_count": failure_count,
            "recovery_contract": {
                "kind": "post_commit_journal_recovery",
                "daemon_commit_truth_preserved": true,
                "journal_append_authoritative": failure_count == 0,
                "operator_action": if failure_count == 0 {
                    "none"
                } else {
                    "inspect daemon logs and runtime post_commit_journal surface before relying on local recovery journal completeness"
                },
            },
        })
    }

    pub(crate) fn pending_post_commit_followup_count(&self) -> u32 {
        self.post_commit_followup_count.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn force_post_commit_journal_failure_once(&self) {
        self.post_commit_journal_force_failure_once
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn block_post_commit_journal_for_tests(&self) {
        self.post_commit_journal_blocked
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn unblock_post_commit_journal_for_tests(&self) {
        self.post_commit_journal_blocked
            .store(false, Ordering::SeqCst);
        self.post_commit_journal_block_notify.notify_waiters();
    }

    #[cfg(test)]
    pub(crate) fn read_post_commit_journal_entries_for_tests(
        &self,
    ) -> io::Result<Vec<serde_json::Value>> {
        let path = self.post_commit_journal_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        read_durable_journal_entries(&path)
    }
}
