use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::runtime::StealthCoverageInfo;

/// Session-scoped mode for public-web interference handling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceMode {
    #[default]
    Normal,
    PublicWebStable,
    Strict,
}

/// Runtime status of the public-web interference surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// Classified kind of public-web interference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceKind {
    InterstitialNavigation,
    PopupHijack,
    OverlayInterference,
    ThirdPartyNoise,
    HumanVerificationRequired,
    UnknownNavigationDrift,
}

/// Explicit safe recovery action chosen by the interference runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRecoveryAction {
    BackNavigate,
    CloseUnexpectedTab,
    RestorePrimaryContext,
    DismissOverlay,
    EscalateToHandoff,
}

/// Outcome of the most recent interference recovery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRecoveryResult {
    Succeeded,
    Failed,
    Abandoned,
    Escalated,
}

/// Structured report for an explicit interference recovery attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceRecoveryReport {
    pub attempted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<InterferenceRecoveryAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<InterferenceRecoveryResult>,
    pub fence_satisfied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Structured observation describing a classified interference event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceObservation {
    pub kind: InterferenceKind,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_url: Option<String>,
}

/// Session-scoped public-web interference runtime projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceRuntimeInfo {
    pub mode: InterferenceMode,
    pub status: InterferenceRuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_interference: Option<InterferenceObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_interference: Option<InterferenceObservation>,
    #[serde(default)]
    pub active_policies: Vec<String>,
    pub recovery_in_progress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recovery_action: Option<InterferenceRecoveryAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recovery_result: Option<InterferenceRecoveryResult>,
    pub handoff_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for InterferenceRuntimeInfo {
    fn default() -> Self {
        Self {
            mode: InterferenceMode::Normal,
            status: InterferenceRuntimeStatus::Inactive,
            current_interference: None,
            last_interference: None,
            active_policies: Vec::new(),
            recovery_in_progress: false,
            last_recovery_action: None,
            last_recovery_result: None,
            handoff_required: false,
            degraded_reason: None,
        }
    }
}

/// Delta between two interference runtime snapshots captured around a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceStateDelta {
    pub before: InterferenceRuntimeInfo,
    pub after: InterferenceRuntimeInfo,
    #[serde(default)]
    pub changed: Vec<String>,
}

/// Attachment status of a session-scoped network rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRuleStatus {
    Configured,
    Active,
    Degraded,
}

/// Session-scoped network rule specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkRuleSpec {
    Rewrite {
        url_pattern: String,
        target_base: String,
    },
    Block {
        url_pattern: String,
    },
    Allow {
        url_pattern: String,
    },
    HeaderOverride {
        url_pattern: String,
        headers: BTreeMap<String, String>,
    },
}

/// Session-scoped request rule projected by the developer integration runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRule {
    pub id: u32,
    pub status: NetworkRuleStatus,
    #[serde(flatten)]
    pub spec: NetworkRuleSpec,
}

/// Browser launch policy projected for diagnostics and agent policy checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPolicyInfo {
    pub headless: bool,
    pub ignore_cert_errors: bool,
    pub hide_infobars: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
    /// How the browser was attached (v1.3: external CDP, profile, auto-discovered, or managed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_target: Option<ConnectionTarget>,
    // v1.4: Stealth diagnostics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_patches: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_default_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humanize_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humanize_speed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_coverage: Option<StealthCoverageInfo>,
}

// ── v1.3 Model Types ──────────────────────────────────────────────────

/// How the browser was connected/attached.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ConnectionTarget {
    /// `--cdp-url <ws://... or http://...>`
    CdpUrl { url: String },
    /// `--connect` (auto-discovered CDP endpoint)
    AutoDiscovered { url: String, port: u16 },
    /// `--profile <name>` (resolved to a user-data-dir path)
    Profile { name: String, resolved_path: String },
    /// Default: daemon launches and owns its own browser.
    Managed,
}
