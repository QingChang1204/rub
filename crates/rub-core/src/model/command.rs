use serde::{Deserialize, Serialize};

use crate::command::allows_missing_request_command_id;
use crate::error::{ErrorCode, ErrorEnvelope};

use super::runtime::Timing;

const CLI_POST_COMMIT_FOLLOWUP_SURFACE: &str = "cli_post_commit_followup_failure";
const CLI_POST_COMMIT_FOLLOWUP_AUTHORITY: &str = "cli.post_commit_followup";
const DAEMON_RESPONSE_COMMITTED: &str = "daemon_response_committed";
const POST_COMMIT_LOCAL_FAILURE_STATE: &str = "daemon_committed_local_followup_failed";
const STDOUT_CONTRACT_FALLBACK_SURFACE: &str = "cli_stdout_contract_fallback";

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
        let request_id = request_id.into();
        Self {
            success: true,
            command: command.into(),
            stdout_schema_version: Self::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: request_id.clone(),
            command_id: Some(request_id),
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
        let request_id = request_id.into();
        Self {
            success: false,
            command: command.into(),
            stdout_schema_version: Self::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: request_id.clone(),
            command_id: Some(request_id),
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
        let allows_missing_command_id = allows_missing_request_command_id(&self.command);
        let stdout_contract_fallback = is_stdout_contract_fallback(self.error.as_ref());
        let has_non_empty_command_id = self
            .command_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let post_commit_followup_payload = self
            .data
            .as_ref()
            .map(classify_post_commit_followup_payload)
            .unwrap_or(PostCommitFollowupPayload::Absent);
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
            _ if self.command.trim().is_empty() => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout result command must be non-empty and non-whitespace",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "command",
                })),
            ),
            _ if self.session.trim().is_empty() => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout result session must be non-empty and non-whitespace",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "session",
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
            _ if allows_missing_command_id && self.command_id.is_some() && !has_non_empty_command_id => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout result command_id must be non-empty and non-whitespace when present",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "command_id",
                })),
            ),
            _ if !allows_missing_command_id && !stdout_contract_fallback && !has_non_empty_command_id => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    format!(
                        "stdout result command_id must be a non-empty string for non-compat command {}",
                        self.command
                    ),
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "command_id",
                    "command": self.command,
                    "compatibility_allows_missing_command_id": false,
                })),
            ),
            _ if matches!(post_commit_followup_payload, PostCommitFollowupPayload::Invalid) => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout result carried an invalid post-commit follow-up payload",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "data.post_commit_followup_*",
                })),
            ),
            _ if self.success && matches!(post_commit_followup_payload, PostCommitFollowupPayload::Valid) => Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "stdout success result carried a post-commit follow-up failure payload",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "field": "data.post_commit_followup_*",
                    "status": "success",
                    "expected_surface": "top_level_error",
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
            _ if !self.success && self.data.is_some() => Some(
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostCommitFollowupPayload {
    Absent,
    Valid,
    Invalid,
}

fn classify_post_commit_followup_payload(data: &serde_json::Value) -> PostCommitFollowupPayload {
    let Some(object) = data.as_object() else {
        return PostCommitFollowupPayload::Absent;
    };

    let has_state = object.contains_key("post_commit_followup_state");
    let has_error = object.contains_key("post_commit_followup_error");
    let has_legacy_commit_state = object.get("commit_state").and_then(|value| value.as_str())
        == Some(POST_COMMIT_LOCAL_FAILURE_STATE);

    if !has_state && !has_error && !has_legacy_commit_state {
        return PostCommitFollowupPayload::Absent;
    }

    if is_post_commit_followup_state(data) && has_post_commit_followup_error(data) {
        PostCommitFollowupPayload::Valid
    } else {
        PostCommitFollowupPayload::Invalid
    }
}

fn is_stdout_contract_fallback(error: Option<&ErrorEnvelope>) -> bool {
    error
        .and_then(|envelope| envelope.context.as_ref())
        .and_then(serde_json::Value::as_object)
        .is_some_and(|context| {
            context
                .get("stdout_contract_fallback")
                .and_then(|value| value.as_bool())
                == Some(true)
                && context
                    .get("projection_kind")
                    .and_then(|value| value.as_str())
                    == Some(STDOUT_CONTRACT_FALLBACK_SURFACE)
        })
}

fn is_post_commit_followup_state(data: &serde_json::Value) -> bool {
    let Some(state) = data
        .as_object()
        .and_then(|object| object.get("post_commit_followup_state"))
        .and_then(|value| value.as_object())
    else {
        return false;
    };

    state.get("surface").and_then(|value| value.as_str()) == Some(CLI_POST_COMMIT_FOLLOWUP_SURFACE)
        && state.get("truth_level").and_then(|value| value.as_str()) == Some("operator_projection")
        && state
            .get("projection_kind")
            .and_then(|value| value.as_str())
            == Some(CLI_POST_COMMIT_FOLLOWUP_SURFACE)
        && state
            .get("projection_authority")
            .and_then(|value| value.as_str())
            == Some(CLI_POST_COMMIT_FOLLOWUP_AUTHORITY)
        && state
            .get("upstream_commit_truth")
            .and_then(|value| value.as_str())
            == Some(DAEMON_RESPONSE_COMMITTED)
        && state.get("control_role").and_then(|value| value.as_str()) == Some("display_only")
        && state.get("durability").and_then(|value| value.as_str()) == Some("best_effort")
        && state
            .get("recovery_contract")
            .and_then(|value| value.as_str())
            == Some("no_public_recovery_contract")
}

fn has_post_commit_followup_error(data: &serde_json::Value) -> bool {
    data.as_object()
        .and_then(|object| object.get("post_commit_followup_error"))
        .is_some_and(|value| serde_json::from_value::<ErrorEnvelope>(value.clone()).is_ok())
}

#[cfg(test)]
mod tests {
    use super::CommandResult;
    use crate::error::{ErrorCode, ErrorEnvelope};

    #[test]
    fn stdout_contract_rejects_success_shaped_post_commit_followup_payload() {
        let result = CommandResult {
            success: true,
            command: "history".to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: "req-1".to_string(),
            command_id: Some("cmd-1".to_string()),
            session: "default".to_string(),
            timing: Default::default(),
            data: Some(serde_json::json!({
                "result": { "format": "pipe" },
                "post_commit_followup_state": {
                    "surface": "cli_post_commit_followup_failure",
                    "truth_level": "operator_projection",
                    "projection_kind": "cli_post_commit_followup_failure",
                    "projection_authority": "cli.post_commit_followup",
                    "upstream_commit_truth": "daemon_response_committed",
                    "control_role": "display_only",
                    "durability": "best_effort",
                    "recovery_contract": "no_public_recovery_contract",
                },
                "post_commit_followup_error": {
                    "code": "INVALID_INPUT",
                    "message": "local export failed after daemon success",
                    "suggestion": "fix it",
                    "context": {
                        "reason": "post_commit_history_export_failed",
                        "daemon_request_committed": true
                    }
                }
            })),
            error: None,
        };

        let envelope = result
            .validate_contract()
            .expect_err("post-commit local failures must fail closed via top-level error");
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("expected_surface")),
            Some(&serde_json::json!("top_level_error"))
        );
    }

    #[test]
    fn stdout_contract_rejects_untyped_post_commit_followup_payload() {
        let result = CommandResult {
            success: true,
            command: "history".to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: "req-1".to_string(),
            command_id: Some("cmd-1".to_string()),
            session: "default".to_string(),
            timing: Default::default(),
            data: Some(serde_json::json!({
                "post_commit_followup_state": {
                    "surface": "cli_post_commit_followup_failure",
                    "truth_level": "operator_projection",
                    "projection_kind": "cli_post_commit_followup_failure",
                    "projection_authority": "cli.post_commit_followup",
                    "upstream_commit_truth": "daemon_response_committed",
                    "control_role": "display_only",
                    "durability": "best_effort",
                    "recovery_contract": "no_public_recovery_contract",
                },
                "result": { "format": "pipe" }
            })),
            error: None,
        };

        let envelope = result
            .validate_contract()
            .expect_err("magic-string-only contract must fail closed");
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
    }

    #[test]
    fn local_success_constructor_copies_request_id_into_command_id() {
        let result = CommandResult::success(
            "close",
            "default",
            "req-local-success",
            serde_json::json!({"result": {"closed": false}}),
        );

        assert_eq!(result.request_id, "req-local-success");
        assert_eq!(result.command_id.as_deref(), Some("req-local-success"));
        assert!(result.validate_contract().is_ok());
    }

    #[test]
    fn local_error_constructor_copies_request_id_into_command_id() {
        let result = CommandResult::error(
            "close",
            "default",
            "req-local-error",
            ErrorEnvelope::new(ErrorCode::InvalidInput, "close failed"),
        );

        assert_eq!(result.request_id, "req-local-error");
        assert_eq!(result.command_id.as_deref(), Some("req-local-error"));
        assert!(result.validate_contract().is_ok());
    }

    #[test]
    fn stdout_contract_rejects_missing_command_id_for_non_compat_command() {
        let result = CommandResult {
            success: true,
            command: "open".to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: "req-1".to_string(),
            command_id: None,
            session: "default".to_string(),
            timing: Default::default(),
            data: Some(serde_json::json!({"result": {"ok": true}})),
            error: None,
        };

        let envelope = result
            .validate_contract()
            .expect_err("non-compat stdout result must require command_id");
        assert_eq!(envelope.code, ErrorCode::IpcProtocolError);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("field")),
            Some(&serde_json::json!("command_id"))
        );
    }

    #[test]
    fn stdout_contract_rejects_empty_command_and_session_identity() {
        let mut result = CommandResult {
            success: true,
            command: " ".to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: "req-1".to_string(),
            command_id: Some("cmd-1".to_string()),
            session: "default".to_string(),
            timing: Default::default(),
            data: Some(serde_json::json!({"result": {"ok": true}})),
            error: None,
        };

        let envelope = result
            .validate_contract()
            .expect_err("stdout command identity must be explicit");
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("field")),
            Some(&serde_json::json!("command"))
        );

        result.command = "open".to_string();
        result.session = "\t".to_string();
        let envelope = result
            .validate_contract()
            .expect_err("stdout session identity must be explicit");
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("field")),
            Some(&serde_json::json!("session"))
        );
    }

    #[test]
    fn stdout_contract_allows_missing_command_id_for_compat_command() {
        let result = CommandResult {
            success: true,
            command: "_handshake".to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: "req-1".to_string(),
            command_id: None,
            session: "default".to_string(),
            timing: Default::default(),
            data: Some(serde_json::json!({"result": {"ok": true}})),
            error: None,
        };

        assert!(result.validate_contract().is_ok());
    }

    #[test]
    fn stdout_contract_allows_missing_command_id_for_stdout_contract_fallback() {
        let result = CommandResult {
            success: false,
            command: "hover".to_string(),
            stdout_schema_version: CommandResult::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: "req-1".to_string(),
            command_id: None,
            session: "default".to_string(),
            timing: Default::default(),
            data: None,
            error: Some(
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "IPC response contract error: missing command_id",
                )
                .with_context(serde_json::json!({
                    "reason": "invalid_stdout_result_contract",
                    "stdout_contract_fallback": true,
                    "projection_kind": "cli_stdout_contract_fallback",
                })),
            ),
        };

        assert!(result.validate_contract().is_ok());
    }
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
