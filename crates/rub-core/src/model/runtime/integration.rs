use serde::{Deserialize, Serialize};

use crate::model::session::NetworkRule;

/// Session-scoped developer integration mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMode {
    Normal,
    Developer,
}

/// High-level runtime status of the developer integration surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationRuntimeStatus {
    Inactive,
    Active,
    Degraded,
    Unsupported,
}

/// Canonical session-scoped integration runtime surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationSurface {
    RequestRules,
    RuntimeObservatory,
    StateInspector,
    Readiness,
    HumanVerificationHandoff,
}

/// Session-scoped developer integration runtime projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationRuntimeInfo {
    pub mode: IntegrationMode,
    pub status: IntegrationRuntimeStatus,
    pub request_rule_count: u32,
    pub request_rules: Vec<NetworkRule>,
    #[serde(default)]
    pub active_surfaces: Vec<IntegrationSurface>,
    #[serde(default)]
    pub degraded_surfaces: Vec<IntegrationSurface>,
    pub observatory_ready: bool,
    pub readiness_ready: bool,
    pub state_inspector_ready: bool,
    pub handoff_ready: bool,
}

impl Default for IntegrationRuntimeInfo {
    fn default() -> Self {
        Self {
            mode: IntegrationMode::Normal,
            status: IntegrationRuntimeStatus::Inactive,
            request_rule_count: 0,
            request_rules: Vec::new(),
            active_surfaces: Vec::new(),
            degraded_surfaces: Vec::new(),
            observatory_ready: false,
            readiness_ready: false,
            state_inspector_ready: false,
            handoff_ready: false,
        }
    }
}

impl IntegrationRuntimeInfo {
    pub fn sync_request_rule_count(&mut self) {
        self.request_rule_count = self.request_rules.len() as u32;
    }
}
