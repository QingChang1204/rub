use serde::{Deserialize, Serialize};

/// Runtime status of the browser-side state inspection surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateInspectorStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// High-level auth state inferred by the state inspector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    #[default]
    Unknown,
    Anonymous,
    Authenticated,
}

/// Session-scoped auth/storage observability projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateInspectorInfo {
    pub status: StateInspectorStatus,
    pub auth_state: AuthState,
    pub cookie_count: u32,
    pub local_storage_keys: Vec<String>,
    pub session_storage_keys: Vec<String>,
    #[serde(default)]
    pub auth_signals: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for StateInspectorInfo {
    fn default() -> Self {
        Self {
            status: StateInspectorStatus::Inactive,
            auth_state: AuthState::Unknown,
            cookie_count: 0,
            local_storage_keys: Vec::new(),
            session_storage_keys: Vec::new(),
            auth_signals: Vec::new(),
            degraded_reason: None,
        }
    }
}

/// Runtime status of the readiness heuristics surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// Route-stability projection for SPA/navigation-aware readiness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStability {
    #[default]
    Unknown,
    Stable,
    Transitioning,
}

/// Overlay state projected by the readiness subsystem.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayState {
    #[default]
    None,
    Development,
    Error,
    UserBlocking,
}

/// Session-scoped readiness projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessInfo {
    pub status: ReadinessStatus,
    pub route_stability: RouteStability,
    pub loading_present: bool,
    pub skeleton_present: bool,
    pub overlay_state: OverlayState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_ready_state: Option<String>,
    #[serde(default)]
    pub blocking_signals: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for ReadinessInfo {
    fn default() -> Self {
        Self {
            status: ReadinessStatus::Inactive,
            route_stability: RouteStability::Unknown,
            loading_present: false,
            skeleton_present: false,
            overlay_state: OverlayState::None,
            document_ready_state: None,
            blocking_signals: Vec::new(),
            degraded_reason: None,
        }
    }
}

/// A live browser-side runtime-state snapshot captured at a specific point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStateSnapshot {
    pub state_inspector: StateInspectorInfo,
    pub readiness_state: ReadinessInfo,
}

/// Runtime status of the current frame context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameContextStatus {
    #[default]
    Unknown,
    Top,
    Child,
    Stale,
    Degraded,
}

/// Canonical metadata for a resolved frame context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameContextInfo {
    pub frame_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_origin_accessible: Option<bool>,
}

/// One entry in the live frame inventory of the current page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameInventoryEntry {
    pub index: u32,
    pub frame: FrameContextInfo,
    pub is_current: bool,
    pub is_primary: bool,
}

/// Session-scoped frame context runtime projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRuntimeInfo {
    pub status: FrameContextStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_frame: Option<FrameContextInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_frame: Option<FrameContextInfo>,
    #[serde(default)]
    pub frame_lineage: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// Structured delta between two live runtime-state probes taken around a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStateDelta {
    pub before: RuntimeStateSnapshot,
    pub after: RuntimeStateSnapshot,
    pub changed: Vec<String>,
}
