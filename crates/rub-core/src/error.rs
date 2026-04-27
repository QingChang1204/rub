use serde::{Deserialize, Serialize};
use std::fmt;

/// All error codes as typed enum (exhaustive).
/// Machine-readable identifier for structured JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Navigation
    NavigationFailed,
    PageLoadTimeout,
    CertError,

    // Interaction
    ElementNotFound,
    ElementNotInteractable,
    InteractionNotConfirmed,
    StaleSnapshot,
    StaleIndex,

    // v1.1: Wait
    WaitTimeout,

    // v1.1: Tabs
    TabNotFound,

    // v1.1: Keyboard
    InvalidKeyName,

    // v1.1: Input validation
    InvalidInput,
    NoMatchingOption,
    FileNotFound,

    // JavaScript
    JsEvalError,
    JsTimeout,

    // Session & Daemon
    DaemonStartFailed,
    DaemonNotRunning,
    SessionBusy,
    IpcTimeout,
    IpcProtocolError,
    IpcVersionMismatch,
    IoError,
    JsonError,
    InternalError,

    // Browser
    BrowserNotFound,
    BrowserCrashed,
    BrowserLaunchFailed,
    ProfileInUse,

    // v1.3: External CDP connection
    CdpConnectionFailed,
    CdpConnectionAmbiguous,
    CdpConnectionLost,
    ProfileNotFound,
    ConflictingConnectOptions,

    // v1.4: Stealth & Anti-Detection
    StealthPatchFailed,
    HumanizeTargetNotFound,

    // v1.5: Human verification handoff
    AutomationPaused,
}

impl ErrorCode {
    /// Returns the default recovery suggestion for this error code.
    pub fn suggestion(&self) -> &'static str {
        match self {
            Self::NavigationFailed => "Check URL spelling and network connectivity",
            Self::PageLoadTimeout => {
                "Increase timeout with --timeout or use --load-strategy domcontentloaded"
            }
            Self::CertError => "Use --ignore-cert-errors for self-signed certificates",
            Self::ElementNotFound => "Run 'rub state' to refresh element indices (range: 0-N)",
            Self::ElementNotInteractable => {
                "Element may be behind a modal or not visible. Try scrolling or closing overlays"
            }
            Self::InteractionNotConfirmed => {
                "The browser-side effect was not confirmed. Inspect confirmation details before retrying, and add an explicit wait if the effect is expected to land asynchronously"
            }
            Self::StaleSnapshot => "Run 'rub state' to get a fresh snapshot",
            Self::StaleIndex => "Run 'rub state' to get fresh element indices",
            Self::WaitTimeout => "Increase timeout or verify the wait condition exists on the page",
            Self::TabNotFound => "Run 'rub tabs' to list available tab indices",
            Self::InvalidKeyName => {
                "Check key name spelling. For plain text, use 'rub type' instead"
            }
            Self::InvalidInput => "Check the command arguments and locator shape",
            Self::NoMatchingOption => {
                "Check the option text or value. Run 'rub get html --selector select' to see available options"
            }
            Self::FileNotFound => "Check the file path and ensure it exists",
            Self::JsEvalError => "Check your JavaScript syntax",
            Self::JsTimeout => "Script took too long. Simplify or increase timeout",
            Self::DaemonStartFailed => "Check permissions on ~/.rub/ and available ports",
            Self::DaemonNotRunning => "Try 'rub close' then retry, or check 'rub doctor'",
            Self::SessionBusy => "Session is restarting. Wait a moment and retry.",
            Self::IpcTimeout => {
                "This session runs one command at a time. Wait for the earlier command to finish, use a separate RUB_HOME for parallel work, or increase --timeout"
            }
            Self::IpcProtocolError => "Internal error. Please report with 'rub doctor' output",
            Self::IpcVersionMismatch => {
                "CLI will auto-upgrade daemon. If this persists, reinstall rub."
            }
            Self::IoError => "Check filesystem paths, permissions, and local socket availability",
            Self::JsonError => "Check JSON structure and field types",
            Self::InternalError => "Internal error. Please report with 'rub doctor' output",
            Self::BrowserNotFound => {
                "Install Chrome, Chromium, or Edge. Run 'rub doctor' to verify"
            }
            Self::BrowserCrashed => "Browser crashed. Next command will auto-restart it",
            Self::BrowserLaunchFailed => {
                "Check 'rub doctor'. May need --no-sandbox on Linux, or check GPU drivers"
            }
            Self::ProfileInUse => {
                "Another session is using this profile. Use '--session <name>' to target it, or choose a different '--user-data-dir'"
            }
            Self::CdpConnectionFailed => {
                "Check the URL and ensure Chrome is running with --remote-debugging-port"
            }
            Self::CdpConnectionAmbiguous => {
                "Multiple CDP endpoints found. Use --cdp-url with a specific URL instead of --connect"
            }
            Self::CdpConnectionLost => {
                "External browser disconnected. Reconnect with --cdp-url or --connect"
            }
            Self::ProfileNotFound => {
                "Profile not found. Run 'rub doctor' to see available profiles, or use --user-data-dir"
            }
            Self::ConflictingConnectOptions => {
                "Use only one of --cdp-url, --connect, or --profile per command"
            }
            Self::StealthPatchFailed => {
                "Stealth patch injection failed. Try --no-stealth to diagnose"
            }
            Self::HumanizeTargetNotFound => {
                "Target element not found for humanized interaction. Falling back to direct action"
            }
            Self::AutomationPaused => {
                "Automation is paused for human verification. Complete the handoff or resume the session first"
            }
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display the SCREAMING_SNAKE_CASE form
        let s = serde_json::to_string(self).unwrap_or_default();
        // Strip quotes from JSON string
        write!(f, "{}", s.trim_matches('"'))
    }
}

/// Structured error envelope for JSON output.
/// Every error carries code, message, suggestion, and optional context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub code: ErrorCode,
    pub message: String,
    pub suggestion: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

impl fmt::Display for ErrorEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl ErrorEnvelope {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let suggestion = code.suggestion().to_string();
        Self {
            code,
            message: message.into(),
            suggestion,
            context: None,
        }
    }

    pub fn with_context(mut self, ctx: serde_json::Value) -> Self {
        self.context = Some(ctx);
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = suggestion.into();
        self
    }
}

/// The unified error type for rub domain logic.
#[derive(Debug, thiserror::Error)]
pub enum RubError {
    #[error("{0}")]
    Domain(ErrorEnvelope),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl RubError {
    /// Create a domain error with the given code and message.
    pub fn domain(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Domain(ErrorEnvelope::new(code, message))
    }

    /// Create a domain error with context.
    pub fn domain_with_context(
        code: ErrorCode,
        message: impl Into<String>,
        context: serde_json::Value,
    ) -> Self {
        Self::Domain(ErrorEnvelope::new(code, message).with_context(context))
    }

    /// Create a domain error with context and an explicit suggestion.
    pub fn domain_with_context_and_suggestion(
        code: ErrorCode,
        message: impl Into<String>,
        context: serde_json::Value,
        suggestion: impl Into<String>,
    ) -> Self {
        Self::Domain(
            ErrorEnvelope::new(code, message)
                .with_context(context)
                .with_suggestion(suggestion),
        )
    }

    /// Extract the error envelope, creating one if needed.
    pub fn into_envelope(self) -> ErrorEnvelope {
        match self {
            Self::Domain(e) => e,
            Self::Io(e) => ErrorEnvelope::new(ErrorCode::IoError, e.to_string()),
            Self::Json(e) => ErrorEnvelope::new(ErrorCode::JsonError, e.to_string()),
            Self::Internal(msg) => ErrorEnvelope::new(ErrorCode::InternalError, msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_serializes_to_screaming_snake_case() {
        let code = ErrorCode::StaleSnapshot;
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "\"STALE_SNAPSHOT\"");
    }

    #[test]
    fn error_code_deserializes_from_screaming_snake_case() {
        let code: ErrorCode = serde_json::from_str("\"ELEMENT_NOT_FOUND\"").unwrap();
        assert_eq!(code, ErrorCode::ElementNotFound);
    }

    #[test]
    fn error_code_roundtrip() {
        let codes = [
            ErrorCode::NavigationFailed,
            ErrorCode::PageLoadTimeout,
            ErrorCode::CertError,
            ErrorCode::ElementNotFound,
            ErrorCode::ElementNotInteractable,
            ErrorCode::StaleSnapshot,
            ErrorCode::StaleIndex,
            ErrorCode::WaitTimeout,
            ErrorCode::TabNotFound,
            ErrorCode::InvalidKeyName,
            ErrorCode::InvalidInput,
            ErrorCode::NoMatchingOption,
            ErrorCode::FileNotFound,
            ErrorCode::JsEvalError,
            ErrorCode::JsTimeout,
            ErrorCode::DaemonStartFailed,
            ErrorCode::DaemonNotRunning,
            ErrorCode::SessionBusy,
            ErrorCode::IpcTimeout,
            ErrorCode::IpcProtocolError,
            ErrorCode::IpcVersionMismatch,
            ErrorCode::IoError,
            ErrorCode::JsonError,
            ErrorCode::InternalError,
            ErrorCode::BrowserNotFound,
            ErrorCode::BrowserCrashed,
            ErrorCode::BrowserLaunchFailed,
            ErrorCode::ProfileInUse,
            ErrorCode::CdpConnectionFailed,
            ErrorCode::CdpConnectionAmbiguous,
            ErrorCode::CdpConnectionLost,
            ErrorCode::ProfileNotFound,
            ErrorCode::ConflictingConnectOptions,
            ErrorCode::StealthPatchFailed,
            ErrorCode::HumanizeTargetNotFound,
            ErrorCode::AutomationPaused,
        ];

        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            let back: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, back, "Roundtrip failed for {json}");
        }
    }

    #[test]
    fn all_error_codes_have_suggestions() {
        let codes = [
            ErrorCode::NavigationFailed,
            ErrorCode::PageLoadTimeout,
            ErrorCode::CertError,
            ErrorCode::ElementNotFound,
            ErrorCode::ElementNotInteractable,
            ErrorCode::StaleSnapshot,
            ErrorCode::StaleIndex,
            ErrorCode::WaitTimeout,
            ErrorCode::TabNotFound,
            ErrorCode::InvalidKeyName,
            ErrorCode::InvalidInput,
            ErrorCode::NoMatchingOption,
            ErrorCode::FileNotFound,
            ErrorCode::JsEvalError,
            ErrorCode::JsTimeout,
            ErrorCode::DaemonStartFailed,
            ErrorCode::DaemonNotRunning,
            ErrorCode::SessionBusy,
            ErrorCode::IpcTimeout,
            ErrorCode::IpcProtocolError,
            ErrorCode::IpcVersionMismatch,
            ErrorCode::IoError,
            ErrorCode::JsonError,
            ErrorCode::InternalError,
            ErrorCode::BrowserNotFound,
            ErrorCode::BrowserCrashed,
            ErrorCode::BrowserLaunchFailed,
            ErrorCode::ProfileInUse,
            ErrorCode::CdpConnectionFailed,
            ErrorCode::CdpConnectionAmbiguous,
            ErrorCode::CdpConnectionLost,
            ErrorCode::ProfileNotFound,
            ErrorCode::ConflictingConnectOptions,
            ErrorCode::StealthPatchFailed,
            ErrorCode::HumanizeTargetNotFound,
            ErrorCode::AutomationPaused,
        ];

        for code in codes {
            assert!(
                !code.suggestion().is_empty(),
                "Missing suggestion for {code:?}"
            );
        }
    }

    #[test]
    fn error_envelope_serializes() {
        let env = ErrorEnvelope::new(ErrorCode::StaleSnapshot, "Snapshot is stale").with_context(
            serde_json::json!({
                "snapshot_epoch": 3,
                "current_epoch": 5
            }),
        );
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["code"], "STALE_SNAPSHOT");
        assert_eq!(json["message"], "Snapshot is stale");
        assert_eq!(json["context"]["snapshot_epoch"], 3);
    }
}
