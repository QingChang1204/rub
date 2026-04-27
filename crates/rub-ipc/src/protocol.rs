use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::fmt;

use rub_core::command::{
    allows_missing_request_command_id, allows_transport_protocol_compat_exemption, command_metadata,
};
use rub_core::error::ErrorEnvelope;
use rub_core::model::Timing;
use uuid::Uuid;

/// IPC protocol version constant.
pub const IPC_PROTOCOL_VERSION: &str = "1.1";
pub const UPGRADE_CHECK_PROBE_COMMAND_ID: &str = "upgrade-check-probe";
pub const MAX_IPC_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;

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
        let command = command.into();
        Self {
            ipc_protocol_version: IPC_PROTOCOL_VERSION.to_string(),
            command_id: default_request_command_id(&command),
            command,
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

    fn contract_error_envelope_with_transport_policy(
        &self,
        allow_transport_internal_protocol_mismatch: bool,
    ) -> Option<ErrorEnvelope> {
        let protocol_mismatch_allowed = allow_transport_internal_protocol_mismatch
            && allows_transport_protocol_compat_exemption(&self.command);
        match () {
            _ if self.ipc_protocol_version != IPC_PROTOCOL_VERSION && !protocol_mismatch_allowed => Some(
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
            _ if allow_transport_internal_protocol_mismatch
                && command_metadata(&self.command).in_process_only =>
            {
                Some(
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        format!(
                            "IPC request command '{}' is in-process only and cannot be sent over transport",
                            self.command
                        ),
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_request_contract",
                        "field": "command",
                        "command": self.command,
                        "in_process_only": true,
                    })),
                )
            }
            _ if self.command_id.is_none() && !allows_missing_request_command_id(&self.command) => {
                Some(
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        format!(
                            "IPC request '{}' requires a command_id on the wire",
                            self.command
                        ),
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_request_contract",
                        "field": "command_id",
                        "command": self.command,
                    })),
                )
            }
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
            _ if self.timeout_ms == 0 || self.timeout_ms > MAX_IPC_TIMEOUT_MS => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC request timeout_ms must be between 1 and {MAX_IPC_TIMEOUT_MS}"
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_request_contract",
                    "field": "timeout_ms",
                    "max_timeout_ms": MAX_IPC_TIMEOUT_MS,
                    "actual_timeout_ms": self.timeout_ms,
                })),
            ),
            _ => None,
        }
    }

    pub fn contract_error_envelope(&self) -> Option<ErrorEnvelope> {
        self.contract_error_envelope_with_transport_policy(false)
    }

    pub fn validate_contract(&self) -> Result<(), ErrorEnvelope> {
        self.contract_error_envelope().map_or(Ok(()), Err)
    }

    pub fn validate_transport_contract(&self) -> Result<(), ErrorEnvelope> {
        self.contract_error_envelope_with_transport_policy(true)
            .map_or(Ok(()), Err)
    }

    pub fn from_value_strict(value: serde_json::Value) -> Result<Self, ErrorEnvelope> {
        Self::decode_with_contract_policy(value, false)
    }

    pub fn from_value_transport(value: serde_json::Value) -> Result<Self, ErrorEnvelope> {
        Self::decode_with_contract_policy(value, true)
    }

    fn decode_with_contract_policy(
        value: serde_json::Value,
        allow_transport_internal_protocol_mismatch: bool,
    ) -> Result<Self, ErrorEnvelope> {
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
        if allow_transport_internal_protocol_mismatch {
            request.validate_transport_contract()?;
        } else {
            request.validate_contract()?;
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_session_id: Option<String>,
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
            daemon_session_id: None,
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
            daemon_session_id: None,
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

    pub fn with_daemon_session_id(mut self, id: impl Into<String>) -> Result<Self, String> {
        self.daemon_session_id = validate_optional_daemon_session_id(Some(id.into()))?;
        Ok(self)
    }

    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    fn contract_error_envelope_with_transport_policy(
        &self,
        request: Option<&IpcRequest>,
        allow_transport_internal_protocol_mismatch: bool,
    ) -> Option<ErrorEnvelope> {
        let protocol_mismatch_allowed = request.is_some_and(|request| {
            allow_transport_internal_protocol_mismatch
                && allows_transport_protocol_compat_exemption(&request.command)
        });
        match self.status {
            _ if self.ipc_protocol_version != IPC_PROTOCOL_VERSION && !protocol_mismatch_allowed => Some(
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
            _ if self
                .daemon_session_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty()) =>
            {
                Some(
                    ErrorEnvelope::new(
                        rub_core::error::ErrorCode::IpcProtocolError,
                        "IPC response daemon_session_id must be non-empty and non-whitespace when present",
                    )
                    .with_context(serde_json::json!({
                        "reason": "invalid_ipc_response_contract",
                        "field": "daemon_session_id",
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

    pub fn contract_error_envelope(&self) -> Option<ErrorEnvelope> {
        self.contract_error_envelope_with_transport_policy(None, false)
    }

    pub fn validate_contract(&self) -> Result<(), ErrorEnvelope> {
        self.contract_error_envelope().map_or(Ok(()), Err)
    }

    pub fn validate_transport_contract(&self, request: &IpcRequest) -> Result<(), ErrorEnvelope> {
        self.contract_error_envelope_with_transport_policy(Some(request), true)
            .map_or(Ok(()), Err)
    }

    pub fn correlation_error_envelope(&self, request: &IpcRequest) -> Option<ErrorEnvelope> {
        match (
            request.command_id.as_deref(),
            self.command_id.as_deref(),
            request.daemon_session_id.as_deref(),
            self.daemon_session_id.as_deref(),
        ) {
            (Some(expected), Some(actual), _, _) if actual != expected => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC response command_id mismatch: expected {:?}, got {:?}",
                        request.command_id, self.command_id
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_command_id_mismatch",
                    "expected_command_id": request.command_id,
                    "actual_command_id": self.command_id,
                })),
            ),
            (Some(_), None, _, _) => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC response for command '{}' omitted the required command_id echo",
                        request.command
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_missing_command_id",
                    "command": request.command,
                    "expected_command_id": request.command_id,
                })),
            ),
            (None, Some(_), _, _) => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC response for command '{}' carried an unsolicited command_id",
                        request.command
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_unsolicited_command_id",
                    "command": request.command,
                    "actual_command_id": self.command_id,
                })),
            ),
            (_, _, Some(expected), Some(actual)) if actual != expected => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC response daemon_session_id mismatch: expected {:?}, got {:?}",
                        request.daemon_session_id, self.daemon_session_id,
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_daemon_session_id_mismatch",
                    "expected_daemon_session_id": request.daemon_session_id,
                    "actual_daemon_session_id": self.daemon_session_id,
                })),
            ),
            (_, _, Some(_), None) => Some(
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!(
                        "IPC response for command '{}' omitted the required daemon_session_id echo",
                        request.command
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "ipc_response_missing_daemon_session_id",
                    "command": request.command,
                    "expected_daemon_session_id": request.daemon_session_id,
                })),
            ),
            _ => None,
        }
    }

    pub fn validate_correlated_contract(&self, request: &IpcRequest) -> Result<(), ErrorEnvelope> {
        self.correlation_error_envelope(request).map_or(Ok(()), Err)
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
        let response = response_from_raw(raw);
        response.validate_contract()?;
        Ok(response)
    }

    pub fn from_value_transport(
        value: serde_json::Value,
        request: &IpcRequest,
    ) -> Result<Self, ErrorEnvelope> {
        let response = if allows_transport_protocol_compat_exemption(&request.command) {
            let raw: RawTransportIpcResponse = serde_json::from_value(value).map_err(|error| {
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!("IPC response schema error: {error}"),
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_schema",
                }))
            })?;
            response_from_transport_raw(raw)
        } else {
            let raw: RawIpcResponse = serde_json::from_value(value).map_err(|error| {
                ErrorEnvelope::new(
                    rub_core::error::ErrorCode::IpcProtocolError,
                    format!("IPC response schema error: {error}"),
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_ipc_response_schema",
                }))
            })?;
            response_from_raw(raw)
        };
        response.validate_transport_contract(request)?;
        response.validate_correlated_contract(request)?;
        Ok(response)
    }
}

#[derive(Debug, Clone)]
pub struct IpcProtocolDecodeError {
    envelope: ErrorEnvelope,
    command_id: Option<String>,
    daemon_session_id: Option<String>,
}

impl IpcProtocolDecodeError {
    pub fn new(envelope: ErrorEnvelope) -> Self {
        Self {
            envelope,
            command_id: None,
            daemon_session_id: None,
        }
    }

    pub fn with_request_correlation(
        envelope: ErrorEnvelope,
        command_id: Option<String>,
        daemon_session_id: Option<String>,
    ) -> Self {
        Self {
            envelope,
            command_id,
            daemon_session_id,
        }
    }

    pub fn envelope(&self) -> &ErrorEnvelope {
        &self.envelope
    }

    pub fn command_id(&self) -> Option<&str> {
        self.command_id.as_deref()
    }

    pub fn daemon_session_id(&self) -> Option<&str> {
        self.daemon_session_id.as_deref()
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
    #[serde(default, deserialize_with = "deserialize_optional_daemon_session_id")]
    pub daemon_session_id: Option<String>,
    #[serde(deserialize_with = "deserialize_request_id")]
    pub request_id: String,
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
    pub timing: Timing,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTransportIpcResponse {
    pub ipc_protocol_version: String,
    #[serde(default, deserialize_with = "deserialize_optional_command_id")]
    pub command_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_daemon_session_id")]
    pub daemon_session_id: Option<String>,
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

fn response_from_raw(raw: RawIpcResponse) -> IpcResponse {
    IpcResponse {
        ipc_protocol_version: raw.ipc_protocol_version,
        command_id: raw.command_id,
        daemon_session_id: raw.daemon_session_id,
        request_id: raw.request_id,
        status: raw.status,
        data: raw.data,
        error: raw.error,
        timing: raw.timing,
    }
}

fn response_from_transport_raw(raw: RawTransportIpcResponse) -> IpcResponse {
    IpcResponse {
        ipc_protocol_version: raw.ipc_protocol_version,
        command_id: raw.command_id,
        daemon_session_id: raw.daemon_session_id,
        request_id: raw.request_id,
        status: raw.status,
        data: raw.data,
        error: raw.error,
        timing: raw.timing,
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

fn default_request_command_id(command: &str) -> Option<String> {
    if allows_missing_request_command_id(command) {
        None
    } else {
        Some(Uuid::now_v7().to_string())
    }
}

fn validate_request_id(request_id: String) -> Result<String, String> {
    if request_id.trim().is_empty() {
        Err("IPC request_id must be non-empty and non-whitespace".to_string())
    } else {
        Ok(request_id)
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
    use rub_core::model::Timing;

    #[test]
    fn request_builder_rejects_blank_command_id() {
        let error = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_command_id("   ")
            .expect_err("blank command_id must be rejected");
        assert!(error.contains("non-empty"));
    }

    #[test]
    fn request_builder_assigns_default_command_id_for_non_compatibility_requests() {
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        assert!(request.command_id.is_some());
    }

    #[test]
    fn request_builder_preserves_missing_command_id_for_control_plane_compat_requests() {
        let request = IpcRequest::new("_handshake", serde_json::json!({}), 1_000);
        assert!(request.command_id.is_none());
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
    fn response_builder_preserves_blank_request_id_for_typed_contract_validation() {
        let error = IpcResponse::success("   ", serde_json::json!({"ok": true}))
            .validate_contract()
            .expect_err("blank request_id must fail contract validation");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(error.context.expect("context")["field"], "request_id");
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
        let error = serde_json::from_value::<IpcRequest>(serde_json::json!({
            "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
            "command": "   ",
            "args": {},
            "timeout_ms": 1000,
        }))
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
            "command_id": "cmd-1",
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
    fn transport_request_decode_allows_protocol_version_mismatch_for_control_plane_internal_commands()
     {
        let request = IpcRequest::from_value_transport(serde_json::json!({
            "ipc_protocol_version": "0.9",
            "command": "_handshake",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect("transport-exposed internal command should reach router compatibility surface");

        assert_eq!(request.command, "_handshake");
        assert_eq!(request.ipc_protocol_version, "0.9");
    }

    #[test]
    fn strict_request_decode_rejects_missing_command_id_for_non_compatibility_requests() {
        let error = IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
            "command": "doctor",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect_err("non-compatibility requests must carry command_id");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("command_id")
        );
    }

    #[test]
    fn strict_request_decode_allows_missing_command_id_for_control_plane_compat_requests() {
        let request = IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
            "command": "_handshake",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect("compatibility control-plane request may omit command_id");
        assert_eq!(request.command_id, None);
    }

    #[test]
    fn strict_request_decode_rejects_timeout_outside_protocol_budget() {
        let error = IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
            "command": "doctor",
            "command_id": "cmd-1",
            "args": {},
            "timeout_ms": super::MAX_IPC_TIMEOUT_MS + 1
        }))
        .expect_err("timeout outside protocol budget must fail contract validation");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        let context = error.context.expect("context");
        assert_eq!(context["field"], serde_json::json!("timeout_ms"));
        assert_eq!(
            context["reason"],
            serde_json::json!("invalid_ipc_request_contract")
        );
    }

    #[test]
    fn transport_request_decode_rejects_protocol_version_mismatch_for_semantic_internal_commands() {
        let error = IpcRequest::from_value_transport(serde_json::json!({
            "ipc_protocol_version": "0.9",
            "command": "_fill_validate",
            "command_id": "cmd-1",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect_err(
            "semantic internal transport commands must not inherit protocol mismatch exemption",
        );

        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("ipc_protocol_version")
        );
    }

    #[test]
    fn transport_request_decode_rejects_protocol_version_mismatch_for_non_internal_commands() {
        let error = IpcRequest::from_value_transport(serde_json::json!({
            "ipc_protocol_version": "0.9",
            "command": "doctor",
            "command_id": "cmd-1",
            "args": {},
            "timeout_ms": 1000
        }))
        .expect_err("non-internal transport commands must still fail closed on protocol mismatch");

        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("ipc_protocol_version")
        );
    }

    #[test]
    fn transport_request_decode_rejects_protocol_version_mismatch_for_in_process_only_internal_commands()
     {
        let error = IpcRequest::from_value_transport(serde_json::json!({
            "ipc_protocol_version": "0.9",
            "command": "_trigger_pipe",
            "command_id": "cmd-1",
            "args": {"spec": []},
            "timeout_ms": 1000
        }))
        .expect_err("in-process-only internal commands must not gain transport compatibility");

        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error.context.expect("context")["field"],
            serde_json::json!("ipc_protocol_version")
        );
    }

    #[test]
    fn transport_request_decode_rejects_current_version_in_process_only_internal_commands() {
        for command in ["_trigger_fill", "_trigger_pipe"] {
            let error = IpcRequest::from_value_transport(serde_json::json!({
                "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
                "command": command,
                "command_id": format!("{command}-cmd"),
                "args": {},
                "timeout_ms": 1000
            }))
            .expect_err("in-process-only commands must not be transport accepted");

            assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
            let context = error.context.expect("context");
            assert_eq!(context["field"], serde_json::json!("command"));
            assert_eq!(context["command"], serde_json::json!(command));
            assert_eq!(context["in_process_only"], serde_json::json!(true));
        }
    }

    #[test]
    fn strict_request_decode_rejects_blank_command() {
        let error = IpcRequest::from_value_strict(serde_json::json!({
            "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
            "command": " ",
            "command_id": "cmd-1",
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
            ipc_protocol_version: super::IPC_PROTOCOL_VERSION.to_string(),
            command_id: None,
            daemon_session_id: None,
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
            daemon_session_id: None,
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
            ipc_protocol_version: super::IPC_PROTOCOL_VERSION.to_string(),
            command_id: Some(" ".to_string()),
            daemon_session_id: None,
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

    #[test]
    fn correlated_response_requires_command_id_echo_when_request_had_one() {
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}));

        let error = response
            .validate_correlated_contract(&request)
            .expect_err("missing command_id echo must fail closed");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(serde_json::Value::as_str),
            Some("ipc_response_missing_command_id")
        );
    }

    #[test]
    fn correlated_response_requires_daemon_session_id_echo_when_request_targeted_one() {
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000)
            .with_daemon_session_id("sess-live")
            .expect("daemon_session_id must be valid");
        let response = IpcResponse::success("req-1", serde_json::json!({"ok": true}))
            .with_command_id(
                request
                    .command_id
                    .clone()
                    .expect("doctor request should carry command_id"),
            )
            .expect("command_id must remain valid");

        let error = response
            .validate_correlated_contract(&request)
            .expect_err("missing daemon_session_id echo must fail closed");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(serde_json::Value::as_str),
            Some("ipc_response_missing_daemon_session_id")
        );
    }

    #[test]
    fn transport_response_decode_allows_protocol_mismatch_and_unknown_fields_for_control_plane_compat_requests()
     {
        let request = IpcRequest::new("_handshake", serde_json::json!({}), 1_000)
            .with_command_id("handshake-probe")
            .expect("probe command_id must be valid");
        let response = IpcResponse::from_value_transport(
            serde_json::json!({
                "ipc_protocol_version": "0.9",
                "command_id": "handshake-probe",
                "request_id": "req-1",
                "status": "success",
                "daemon_session_id": "sess-live",
                "data": {
                    "daemon_session_id": "sess-live",
                    "launch_policy": {
                        "headless": true,
                        "ignore_cert_errors": false,
                        "hide_infobars": false
                    }
                },
                "timing": {"queue_ms":0,"exec_ms":0,"total_ms":0},
                "future_metadata": "allowed"
            }),
            &request,
        )
        .expect(
            "compat response transport decode should stay open enough for the control-plane lane",
        );

        assert_eq!(response.ipc_protocol_version, "0.9");
        assert_eq!(response.command_id.as_deref(), Some("handshake-probe"));
    }

    #[test]
    fn transport_response_decode_rejects_unknown_fields_for_non_compat_requests() {
        let request = IpcRequest::new("doctor", serde_json::json!({}), 1_000);
        let error = IpcResponse::from_value_transport(
            serde_json::json!({
                "ipc_protocol_version": super::IPC_PROTOCOL_VERSION,
                "command_id": request.command_id.clone(),
                "request_id": "req-1",
                "status": "success",
                "daemon_session_id": "sess-live",
                "data": { "ok": true },
                "timing": {},
                "future_metadata": "rejected"
            }),
            &request,
        )
        .expect_err("non-compat response transport decode must remain schema-closed");
        assert_eq!(error.code, rub_core::error::ErrorCode::IpcProtocolError);
        assert_eq!(
            error
                .context
                .as_ref()
                .and_then(|context| context.get("reason"))
                .and_then(serde_json::Value::as_str),
            Some("invalid_ipc_response_schema")
        );
    }
}
