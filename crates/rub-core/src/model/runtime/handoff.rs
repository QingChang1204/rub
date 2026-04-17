use serde::{Deserialize, Serialize};

/// Runtime status of the human verification handoff surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanVerificationHandoffStatus {
    Available,
    Active,
    Completed,
    #[default]
    Unavailable,
}

/// Session-scoped human verification handoff projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanVerificationHandoffInfo {
    pub status: HumanVerificationHandoffStatus,
    pub automation_paused: bool,
    pub resume_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl Default for HumanVerificationHandoffInfo {
    fn default() -> Self {
        Self {
            status: HumanVerificationHandoffStatus::Unavailable,
            automation_paused: false,
            resume_supported: false,
            unavailable_reason: Some("not_configured".to_string()),
        }
    }
}

/// Runtime status of the session takeover surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverRuntimeStatus {
    Available,
    Active,
    Degraded,
    #[default]
    Unavailable,
}

/// Whether the current session is directly accessible to a human operator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAccessibility {
    #[default]
    AutomationOnly,
    UserAccessible,
}

/// How the current browser session is exposed to the operator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverVisibilityMode {
    Headed,
    #[default]
    Headless,
    External,
}

/// Canonical transition kinds for the session takeover runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverTransitionKind {
    Start,
    Resume,
    Elevate,
}

/// Structured result of one takeover-runtime transition attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverTransitionResult {
    Succeeded,
    Rejected,
}

/// Most recent takeover-runtime transition published for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverTransitionInfo {
    pub kind: TakeoverTransitionKind,
    pub result: TakeoverTransitionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Session-scoped accessibility/takeover runtime projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverRuntimeInfo {
    pub status: TakeoverRuntimeStatus,
    pub session_accessibility: SessionAccessibility,
    pub visibility_mode: TakeoverVisibilityMode,
    pub elevate_supported: bool,
    pub resume_supported: bool,
    pub automation_paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_transition: Option<TakeoverTransitionInfo>,
}
