use serde::{Deserialize, Serialize};

use super::{BindingLiveStatus, BindingResolution, RememberedBindingAliasKind};

/// Source object explicitly chosen for one command-time binding reuse decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingExecutionSourceKind {
    RememberedAlias,
}

/// Concrete reuse mode chosen after explicit binding/alias resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingExecutionMode {
    ReuseLiveSession,
    LaunchBoundProfile,
    LaunchBoundRuntime,
}

/// Refresh path that remains available for one resolved binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingRefreshPath {
    Human,
    Cli,
    Mixed,
}

/// CLI-side projection explaining how one explicit alias resolved for command execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingExecutionResolutionInfo {
    pub source_kind: BindingExecutionSourceKind,
    pub requested_alias: String,
    pub remembered_alias_kind: RememberedBindingAliasKind,
    pub binding_alias: String,
    pub mode: BindingExecutionMode,
    pub effective_session_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_profile_dir_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_user_data_dir: Option<String>,
    pub live_status: BindingLiveStatus,
    pub resolution: BindingResolution,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_refresh_paths: Vec<BindingRefreshPath>,
}
