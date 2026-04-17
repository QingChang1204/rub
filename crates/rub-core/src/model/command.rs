use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ErrorEnvelope};

use super::runtime::Timing;

/// Unified command result envelope for stdout JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub command: String,
    pub stdout_schema_version: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub session: String,
    pub timing: Timing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
}

impl CommandResult {
    /// Protocol version constant.
    pub const STDOUT_SCHEMA_VERSION: &'static str = "3.0";

    /// Create a success result.
    pub fn success(
        command: impl Into<String>,
        session: impl Into<String>,
        request_id: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            success: true,
            command: command.into(),
            stdout_schema_version: Self::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: request_id.into(),
            command_id: None,
            session: session.into(),
            timing: Timing::default(),
            data: Some(data),
            error: None,
        }
    }

    /// Create an error result.
    pub fn error(
        command: impl Into<String>,
        session: impl Into<String>,
        request_id: impl Into<String>,
        envelope: ErrorEnvelope,
    ) -> Self {
        Self {
            success: false,
            command: command.into(),
            stdout_schema_version: Self::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: request_id.into(),
            command_id: None,
            session: session.into(),
            timing: Timing::default(),
            data: None,
            error: Some(envelope),
        }
    }

    /// Set the command_id.
    pub fn with_command_id(mut self, id: impl Into<String>) -> Self {
        self.command_id = Some(id.into());
        self
    }

    /// Set the timing.
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    pub fn contract_error_envelope(&self) -> Option<ErrorEnvelope> {
        match self.stdout_schema_version.as_str() {
            version if version != Self::STDOUT_SCHEMA_VERSION => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!(
                        "stdout result schema mismatch: expected {}, got {}",
                        Self::STDOUT_SCHEMA_VERSION,
                        self.stdout_schema_version
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "stdout_schema_version",
                    "expected_stdout_schema_version": Self::STDOUT_SCHEMA_VERSION,
                    "actual_stdout_schema_version": self.stdout_schema_version,
                })),
            ),
            _ if self.request_id.trim().is_empty() => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout result request_id must be non-empty and non-whitespace",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "request_id",
                })),
            ),
            _ if self
                .command_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty()) => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout result command_id must be non-empty and non-whitespace when present",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "command_id",
                })),
            ),
            _ if self.success && self.error.is_some() => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout success result carried an error envelope",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "status": "success",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            _ if self.success && self.data.is_none() => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout success result omitted success data",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "status": "success",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            _ if !self.success && self.error.is_none() => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout error result omitted the error envelope",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "status": "error",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            _ if !self.success
                && self
                    .data
                    .as_ref()
                    .is_some_and(|data| !is_post_commit_local_failure_data(data)) =>
            Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout error result carried success data",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "status": "error",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            _ => None,
        }
    }

    pub fn validate_contract(&self) -> Result<(), ErrorEnvelope> {
        self.contract_error_envelope().map_or(Ok(()), Err)
    }
}

fn is_post_commit_local_failure_data(data: &serde_json::Value) -> bool {
    data.as_object()
        .and_then(|object| object.get("commit_state"))
        .and_then(|value| value.as_str())
        == Some("daemon_committed_local_followup_failed")
}

/// Load strategy for page navigation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoadStrategy {
    #[default]
    Load,
    #[serde(rename = "domcontentloaded")]
    DomContentLoaded,
    #[serde(rename = "networkidle")]
    NetworkIdle,
}

/// Scroll direction for the scroll command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    Up,
    Down,
}
