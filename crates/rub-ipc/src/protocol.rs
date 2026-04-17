use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::fmt;

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
#[derive(Debug, Clone, Serialize)]
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

    pub fn contract_error_envelope(&self) -> Option<ErrorEnvelope> {
        match () {
            _ if self.ipc_protocol_version != IPC_PROTOCOL_VERSION => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC request protocol mismatch: expected {}, got {}",
                        IPC_PROTOCOL_VERSION, self.ipc_protocol_version
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_request_contract",
                    "field": "ipc_protocol_version",
                    "expected_protocol_version": IPC_PROTOCOL_VERSION,
                    "actual_protocol_version": self.ipc_protocol_version,
                })),
            ),
            _ if self.command.trim().is_empty() => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    "IPC request command must be non-empty and non-whitespace",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_request_contract",
                    "field": "command",
                })),
            ),
            _ if self
                .command_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty()) =>
            {
                Some(
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        "IPC request command_id must be non-empty and non-whitespace when present",
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_request_contract",
                        "field": "command_id",
                    })),
                )
            }
            _ if self
                .daemon_session_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty()) =>
            {
                Some(
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        "IPC request daemon_session_id must be non-empty and non-whitespace when present",
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_request_contract",
                        "field": "daemon_session_id",
                    })),
                )
            }
            _ => None,
        }
    }

    pub fn validate_contract(&self) -> Result<(), ErrorEnvelope> {
        self.contract_error_envelope().map_or(Ok(()), Err)
    }

    pub fn from_value_strict(value: serde_json::Value) -> Result<Self, ErrorEnvelope> {
        let raw: RawIpcRequest = serde_json::from_value(value).map_err(|error| {
            ErrorEnvelope::new(
                rub_core::error::ErrorCode::IpcProtocolError,
                format!("IPC request schema error: {error}"),
            )
            .with_context(serde_json::json!({
                "reason": "invalid_ipc_request_schema",
            }))
        })?;
        let request = IpcRequest {
            ipc_protocol_version: raw.ipc_protocol_version,
            command: raw.command,
            command_id: raw.command_id,
            daemon_session_id: raw.daemon_session_id,
            args: raw.args,
            timeout_ms: raw.timeout_ms,
        };
        request.validate_contract()?;
        Ok(request)
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
            request_id: validated_request_id(request_id),
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
            request_id: validated_request_id(request_id),
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
            _ if self.ipc_protocol_version != IPC_PROTOCOL_VERSION => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcVersionMismatch,
                    format!(
                        "IPC response protocol mismatch: expected {}, got {}",
                        IPC_PROTOCOL_VERSION, self.ipc_protocol_version
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_protocol_version_mismatch",
                    "expected_protocol_version": IPC_PROTOCOL_VERSION,
                    "actual_protocol_version": self.ipc_protocol_version,
                })),
            ),
            _ if self.request_id.trim().is_empty() => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    "IPC response request_id must be non-empty and non-whitespace",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_contract",
                    "field": "request_id",
                })),
            ),
            _ if self
                .command_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty()) =>
            {
                Some(
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        "IPC response command_id must be non-empty and non-whitespace when present",
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_response_contract",
                        "field": "command_id",
                    })),
                )
            }
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

    pub fn from_value_strict(value: serde_json::Value) -> Result<Self, ErrorEnvelope> {
        let raw: RawIpcResponse = serde_json::from_value(value).map_err(|error| {
            ErrorEnvelope::new(
                rub_core::error::ErrorCode::IpcProtocolError,
                format!("IPC response schema error: {error}"),
            )
            .with_context(serde_json::json!({
                "reason": "invalid_ipc_response_schema",
            }))
        })?;
        let response = IpcResponse {
            ipc_protocol_version: raw.ipc_protocol_version,
            command_id: raw.command_id,
            request_id: raw.request_id,
            status: raw.status,
            data: raw.data,
            error: raw.error,
            timing: raw.timing,
        };
        response.validate_contract()?;
        Ok(response)
    }
}

#[derive(Debug, Clone)]
pub struct IpcProtocolDecodeError {
    envelope: ErrorEnvelope,
}

impl IpcProtocolDecodeError {
    pub fn new(envelope: ErrorEnvelope) -> Self {
        Self { envelope }
    }

    pub fn envelope(&self) -> &ErrorEnvelope {
        &self.envelope
    }

    pub fn into_envelope(self) -> ErrorEnvelope {
        self.envelope
    }
}

impl fmt::Display for IpcProtocolDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.envelope.message)
    }
}

impl std::error::Error for IpcProtocolDecodeError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIpcRequest {
    pub ipc_protocol_version: String,
    pub command: String,
    #[serde(default, deserialize_with = "deserialize_optional_command_id")]
    pub command_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_daemon_session_id")]
    pub daemon_session_id: Option<String>,
    pub args: serde_json::Value,
    pub timeout_ms: u64,
}

impl<'de> Deserialize<'de> for IpcRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        IpcRequest::from_value_strict(value).map_err(|envelope| de::Error::custom(envelope.message))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIpcResponse {
    pub ipc_protocol_version: String,
    #[serde(default, deserialize_with = "deserialize_optional_command_id")]
    pub command_id: Option<String>,
    #[serde(deserialize_with = "deserialize_request_id")]
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
        let value = serde_json::Value::deserialize(deserializer)?;
        IpcResponse::from_value_strict(value)
            .map_err(|envelope| de::Error::custom(envelope.message))
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

fn deserialize_request_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let request_id = String::deserialize(deserializer)?;
    validate_request_id(request_id).map_err(de::Error::custom)
}

fn validate_optional_command_id(command_id: Option<String>) -> Result<Option<String>, String> {
    match command_id {
        Some(value) if value.trim().is_empty() => {
            Err("IPC command_id must be non-empty and non-whitespace".to_string())
        }
        other => Ok(other),
    }
}

fn validate_request_id(request_id: String) -> Result<String, String> {
    if request_id.trim().is_empty() {
        Err("IPC request_id must be non-empty and non-whitespace".to_string())
    } else {
        Ok(request_id)
    }
}

fn validated_request_id(request_id: impl Into<String>) -> String {
    validate_request_id(request_id.into())
        .expect("IPC request_id must be non-empty and non-whitespace")
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
    use rub_core::model::Timing;

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

    #[test]
    #[should_panic(expected = "IPC request_id must be non-empty and non-whitespace")]
    fn response_builder_rejects_blank_request_id() {
        let _ = IpcResponse::success("   ", serde_json::json!({"ok": true}));
    }

    #[test]
    fn strict_decode_rejects_blank_request_id() {
        let error = IpcResponse::from_value_strict(serde_json::json!({
            "ipc_protocol_version": "1.0",
            "request_id": "   ",
            "status": "success",
            "data": {"ok": true},
            "timing": {}
        }))
        .expect_err("blank request_id must be rejected");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["reason"],
            serde_json::json!("invalid_ipc_response_schema")
        );
    }

    #[test]
    fn ipc_request_rejects_blank_command() {
        let error = serde_json::from_str::<IpcRequest>(
            r#"{
                "ipc_protocol_version":"1.0",
                "command":"   ",
                "args":{},
                "timeout_ms":1000
            }"#,
        )
        .expect_err("blank command should be rejected");
        assert!(
            error
                .to_string()
                .contains("command must be non-empty and non-whitespace"),
            "{error}"
        );
    }

    #[test]
    fn strict_request_decode_rejects_protocol_version_mismatch() {
        let error = IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": "0.9",
            "command": "doctor",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect_err("protocol mismatch should be rejected");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("ipc_protocol_version")
        );
    }

    #[test]
    fn strict_request_decode_rejects_blank_command() {
        let error = IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": "1.0",
            "command": " ",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect_err("blank command should be rejected");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("command")
        );
    }

    #[test]
    fn contract_error_flags_blank_request_id() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: None,
            request_id: " ".to_string(),
            status: super::ResponseStatus::Success,
            data: Some(serde_json::json!({"ok": true})),
            error: None,
            timing: Timing::default(),
        };
        let error = response
            .contract_error_envelope()
            .expect("blank request_id should violate contract");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("request_id")
        );
    }

    #[test]
    fn contract_error_flags_protocol_version_mismatch() {
        let response = IpcResponse {
            ipc_protocol_version: "0.9".to_string(),
            command_id: None,
            request_id: "req-1".to_string(),
            status: super::ResponseStatus::Success,
            data: Some(serde_json::json!({"ok": true})),
            error: None,
            timing: Timing::default(),
        };
        let error = response
            .contract_error_envelope()
            .expect("protocol mismatch should violate contract");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcVersionMismatch);
        assert_eq!(
            error.context.expect("context")["reason"],
            serde_json::json!("ipc_response_protocol_version_mismatch")
        );
    }

    #[test]
    fn contract_error_flags_blank_command_id() {
        let response = IpcResponse {
            ipc_protocol_version: "1.0".to_string(),
            command_id: Some(" ".to_string()),
            request_id: "req-1".to_string(),
            status: super::ResponseStatus::Success,
            data: Some(serde_json::json!({"ok": true})),
            error: None,
            timing: Timing::default(),
        };
        let error = response
            .contract_error_envelope()
            .expect("blank command_id should violate contract");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("command_id")
        );
    }
}
