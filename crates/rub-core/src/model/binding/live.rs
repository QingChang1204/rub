use serde::{Deserialize, Serialize};

use super::{BindingPersistencePolicy, BindingRecord};

/// Projected durability scope for a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingDurabilityScope {
    RubHomeLocalDurable,
    RubHomeLocalEphemeral,
    ExternalAttachment,
}

/// Projected reattachment mode for a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingReattachmentMode {
    ManagedReacquirable,
    ExternalReattachRequired,
    TempHomeEphemeral,
}

/// High-level projected status for a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    LiveSessionPresent,
    VerificationRequired,
    ExternalReattachmentRequired,
    EphemeralBinding,
    LiveStatusUnavailable,
}

/// Projected live status for a binding record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingLiveStatus {
    pub status: BindingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    pub live_session_present: bool,
    pub runtime_refresh_required: bool,
    pub human_refresh_available: bool,
    pub verification_required: bool,
    pub durability_scope: BindingDurabilityScope,
    pub reattachment_mode: BindingReattachmentMode,
}

/// One live-session match used to explain how a binding resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingResolutionMatch {
    pub matched_by: String,
    pub session_id: String,
    pub session_name: String,
}

/// Typed resolution summary for a binding lookup or capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingResolution {
    LiveStatusUnavailable,
    NoLiveMatch,
    LiveMatch {
        matched_by: String,
        session_id: String,
        session_name: String,
    },
    AmbiguousLiveMatch {
        matches: Vec<BindingResolutionMatch>,
    },
}

/// Typed target resolution for a remembered alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RememberedBindingAliasTarget {
    Resolved {
        binding_alias: String,
        binding: Box<BindingRecord>,
        live_status: BindingLiveStatus,
        resolution: BindingResolution,
    },
    MissingBinding {
        binding_alias: String,
    },
}

impl BindingPersistencePolicy {
    pub fn durability_and_reattachment(self) -> (BindingDurabilityScope, BindingReattachmentMode) {
        match self {
            BindingPersistencePolicy::RubHomeLocalDurable => (
                BindingDurabilityScope::RubHomeLocalDurable,
                BindingReattachmentMode::ManagedReacquirable,
            ),
            BindingPersistencePolicy::RubHomeLocalEphemeral => (
                BindingDurabilityScope::RubHomeLocalEphemeral,
                BindingReattachmentMode::TempHomeEphemeral,
            ),
            BindingPersistencePolicy::ExternalReattachmentRequired => (
                BindingDurabilityScope::ExternalAttachment,
                BindingReattachmentMode::ExternalReattachRequired,
            ),
        }
    }
}
