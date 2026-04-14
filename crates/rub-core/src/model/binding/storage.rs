use serde::{Deserialize, Serialize};

/// Scope for a named runtime binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingScope {
    RubHomeLocal,
}

/// Session-reference kind stored in a binding record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSessionReferenceKind {
    LiveSessionHint,
}

/// Live session correlation hint stored alongside a durable binding record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSessionReference {
    pub kind: BindingSessionReferenceKind,
    pub session_id: String,
    pub session_name: String,
}

/// Provenance for how a binding was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingCreatedVia {
    HandoffCompleted,
    TakeoverResumed,
    CliAuthCompleted,
    MixedAuthCompleted,
    BoundExistingRuntime,
    Unknown,
}

/// High-level auth input mode used to arrive at a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingAuthInputMode {
    Human,
    Cli,
    Mixed,
    Unknown,
}

/// Conservative auth provenance attached to a binding record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingAuthProvenance {
    pub created_via: BindingCreatedVia,
    pub auth_input_mode: BindingAuthInputMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_fence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_from_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_from_attachment_identity: Option<String>,
}

/// Durable persistence policy for a v1 binding record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingPersistencePolicy {
    RubHomeLocalDurable,
    RubHomeLocalEphemeral,
    ExternalReattachmentRequired,
}

/// One durable binding record stored under `RUB_HOME`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingRecord {
    pub alias: String,
    pub scope: BindingScope,
    pub rub_home_reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_reference: Option<BindingSessionReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_directory_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir_reference: Option<String>,
    pub auth_provenance: BindingAuthProvenance,
    pub persistence_policy: BindingPersistencePolicy,
    pub created_at: String,
    pub last_captured_at: String,
}

/// Durable bindings file envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingRegistryData {
    pub schema_version: u32,
    pub bindings: Vec<BindingRecord>,
}

impl Default for BindingRegistryData {
    fn default() -> Self {
        Self {
            schema_version: 1,
            bindings: Vec::new(),
        }
    }
}

/// Kind of remembered alias layered on top of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberedBindingAliasKind {
    Binding,
    Account,
    Workspace,
}

/// One inspectable remembered alias that resolves to a concrete binding alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedBindingAliasRecord {
    pub alias: String,
    pub kind: RememberedBindingAliasKind,
    pub binding_alias: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable remembered-alias registry stored under `RUB_HOME`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedBindingAliasRegistryData {
    pub schema_version: u32,
    pub aliases: Vec<RememberedBindingAliasRecord>,
}

impl Default for RememberedBindingAliasRegistryData {
    fn default() -> Self {
        Self {
            schema_version: 1,
            aliases: Vec::new(),
        }
    }
}
