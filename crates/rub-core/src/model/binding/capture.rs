use serde::{Deserialize, Serialize};

use crate::model::{AuthState, ConnectionTarget, StateInspectorStatus};

use super::{
    BindingAuthProvenance, BindingDurabilityScope, BindingPersistencePolicy,
    BindingReattachmentMode, BindingSessionReference,
};

/// Session-scoped identity projected for one binding capture candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingCaptureSessionInfo {
    pub session_id: String,
    pub session_name: String,
    pub rub_home_reference: String,
    pub rub_home_temp_owned: bool,
}

/// Attachment/runtime identity projected for one binding capture candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingCaptureAttachmentInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_target: Option<ConnectionTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_directory_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir_reference: Option<String>,
}

/// Capture-fence status for a binding candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingCaptureFenceStatus {
    CaptureReady,
    BindCurrentOnly,
    CaptureUnavailable,
}

/// Projected capture-fence semantics for a binding candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingCaptureFenceInfo {
    pub status: BindingCaptureFenceStatus,
    pub capture_eligible: bool,
    pub bind_current_eligible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_fence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
}

/// Auth-related evidence projected for one binding capture candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingCaptureAuthEvidence {
    pub status: StateInspectorStatus,
    pub auth_state: AuthState,
    pub cookie_count: u32,
    #[serde(default)]
    pub auth_signals: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// Durability semantics projected for one binding capture candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingCaptureDurabilityInfo {
    pub persistence_policy: BindingPersistencePolicy,
    pub durability_scope: BindingDurabilityScope,
    pub reattachment_mode: BindingReattachmentMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
}

/// Live correlation hints projected for one binding capture candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingCaptureLiveCorrelation {
    pub session_reference: BindingSessionReference,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_identity: Option<String>,
}

/// Additive diagnostics for a composite binding-capture projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BindingCaptureDiagnostics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consistency_warnings: Vec<String>,
}

impl BindingCaptureDiagnostics {
    pub fn is_empty(&self) -> bool {
        self.consistency_warnings.is_empty()
    }
}

/// Live runtime projection used to create or validate one binding capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingCaptureCandidateInfo {
    pub session: BindingCaptureSessionInfo,
    pub attachment: BindingCaptureAttachmentInfo,
    pub capture_fence: BindingCaptureFenceInfo,
    pub auth_evidence: BindingCaptureAuthEvidence,
    pub durability: BindingCaptureDurabilityInfo,
    pub live_correlation: BindingCaptureLiveCorrelation,
    pub auth_provenance_hint: BindingAuthProvenance,
    #[serde(default, skip_serializing_if = "BindingCaptureDiagnostics::is_empty")]
    pub diagnostics: BindingCaptureDiagnostics,
}
