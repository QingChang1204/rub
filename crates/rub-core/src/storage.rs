use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Browser storage area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageArea {
    Local,
    Session,
}

/// Session-scoped runtime status of the storage surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// Type of a recorded storage mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMutationKind {
    Set,
    Remove,
    Clear,
    Import,
}

/// Current browser-authoritative storage snapshot for one origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub origin: String,
    #[serde(default)]
    pub local_storage: BTreeMap<String, String>,
    #[serde(default)]
    pub session_storage: BTreeMap<String, String>,
}

/// Session-scoped mutation history for the storage runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageMutationRecord {
    pub sequence: u64,
    pub kind: StorageMutationKind,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<StorageArea>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Session-scoped storage runtime projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageRuntimeInfo {
    pub status: StorageRuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_origin: Option<String>,
    #[serde(default)]
    pub local_storage_keys: Vec<String>,
    #[serde(default)]
    pub session_storage_keys: Vec<String>,
    #[serde(default)]
    pub recent_mutations: Vec<StorageMutationRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for StorageRuntimeInfo {
    fn default() -> Self {
        Self {
            status: StorageRuntimeStatus::Inactive,
            current_origin: None,
            local_storage_keys: Vec::new(),
            session_storage_keys: Vec::new(),
            recent_mutations: Vec::new(),
            degraded_reason: None,
        }
    }
}
