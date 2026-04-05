use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use rub_core::error::ErrorEnvelope;
use rub_core::model::Timing;

/// IPC protocol version constant.
pub const IPC_PROTOCOL_VERSION: &str = "1.0";

/// Response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStatus {
    Success,
    Error,
}

/// IPC request from CLI to Daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcRequest {
    pub ipc_protocol_version: String,
    pub command: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_command_id"
    )]
    pub command_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_daemon_session_id"
    )]
    pub daemon_session_id: Option<String>,
    pub args: serde_json::Value,
    pub timeout_ms: u64,
}

impl IpcRequest {
    pub fn new(command: impl Into<String>, args: serde_json::Value, timeout_ms: u64) -> Self {
        Self {
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            command: command.into(),
            command_id: None,
            daemon_session_id: None,
            args,
            timeout_ms,
        }
    }

    pub fn with_command_id(mut self, id: impl Into<String>) -> Result<Self, String> {
        self.command_id = validate_optional_command_id(Some(id.into()))?;
        Ok(self)
    }

    pub fn with_daemon_session_id(mut self, id: impl Into<String>) -> Result<Self, String> {
        self.daemon_session_id = validate_optional_daemon_session_id(Some(id.into()))?;
        Ok(self)
    }
}

/// IPC response from Daemon to CLI.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpcResponse {
    pub ipc_protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub request_id: String,
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
    pub timing: Timing,
}

impl IpcResponse {
    pub fn success(request_id: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            command_id: None,
            request_id: request_id.into(),
            status: ResponseStatus::Success,
            data: Some(data),
            error: None,
            timing: Timing::default(),
        }
    }

    pub fn error(request_id: impl Into<String>, envelope: ErrorEnvelope) -> Self {
        Self {
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            command_id: None,
            request_id: request_id.into(),
            status: ResponseStatus::Error,
            data: None,
            error: Some(envelope),
            timing: Timing::default(),
        }
    }

    pub fn with_command_id(mut self, id: impl Into<String>) -> Result<Self, String> {
        self.command_id = validate_optional_command_id(Some(id.into()))?;
        Ok(self)
    }

    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    pub fn contract_error_envelope(&self) -> Option<ErrorEnvelope> {
        match self.status {
            ResponseStatus::Success if self.error.is_some() => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    "IPC success response carried an error envelope",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_contract",
                    "status": "success",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            ResponseStatus::Success if self.data.is_none() => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    "IPC success response omitted success data",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_contract",
                    "status": "success",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            ResponseStatus::Error if self.error.is_none() => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    "IPC error response omitted the error envelope",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_contract",
                    "status": "error",
                    "has_data": self.data.is_some(),
                    "has_error": self.error.is_some(),
                })),
            ),
            ResponseStatus::Error if self.data.is_some() => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    "IPC error response carried success data",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_contract",
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIpcResponse {
    pub ipc_protocol_version: String,
    #[serde(default, deserialize_with = "deserialize_optional_command_id")]
    pub command_id: Option<String>,
    pub request_id: String,
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
    pub timing: Timing,
}

impl<'de> Deserialize<'de> for IpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawIpcResponse::deserialize(deserializer)?;
        let response = IpcResponse {
            ipc_protocol_version: raw.ipc_protocol_version,
            command_id: raw.command_id,
            request_id: raw.request_id,
            status: raw.status,
            data: raw.data,
            error: raw.error,
            timing: raw.timing,
        };
        response
            .validate_contract()
            .map_err(|envelope| de::Error::custom(envelope.message))?;
        Ok(response)
    }
}

fn deserialize_optional_command_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let command_id = Option::<String>::deserialize(deserializer)?;
    validate_optional_command_id(command_id).map_err(de::Error::custom)
}

fn deserialize_optional_daemon_session_id<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let daemon_session_id = Option::<String>::deserialize(deserializer)?;
    validate_optional_daemon_session_id(daemon_session_id).map_err(de::Error::custom)
}

fn validate_optional_command_id(command_id: Option<String>) -> Result<Option<String>, String> {
    match command_id {
        Some(value) if value.trim().is_empty() => {
            Err("IPC command_id must be non-empty and non-whitespace".to_string())
        }
        other => Ok(other),
    }
}

fn validate_optional_daemon_session_id(
    daemon_session_id: Option<String>,
) -> Result<Option<String>, String> {
    match daemon_session_id {
        Some(value) if value.trim().is_empty() => {
            Err("IPC daemon_session_id must be non-empty and non-whitespace".to_string())
        }
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{IpcRequest, IpcResponse};

    #[test]
    fn request_builder_rejects_blank_command_id() {
        let error = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_command_id("   ")
            .expect_err("blank command_id must be rejected");
        assert!(error.contains("non-empty"));
    }

    #[test]
    fn response_builder_rejects_blank_command_id() {
        let error = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(" ")
            .expect_err("blank command_id must be rejected");
        assert!(error.contains("non-empty"));
    }

    #[test]
    fn request_builder_rejects_blank_daemon_session_id() {
        let error = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_daemon_session_id("   ")
            .expect_err("blank daemon_session_id must be rejected");
        assert!(error.contains("non-empty"));
    }
}
