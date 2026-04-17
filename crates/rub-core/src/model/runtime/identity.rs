use serde::{Deserialize, Serialize};

/// Timing breakdown for command execution.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Timing {
    pub queue_ms: u64,
    pub exec_ms: u64,
    pub total_ms: u64,
}

/// Runtime stealth coverage projected for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityProbeStatus {
    Passed,
    Failed,
    Unknown,
}

/// Measured self-probe results for the current identity runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdentitySelfProbeInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_main_world: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iframe_context: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_context: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ua_consistency: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webgl_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touch_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_metrics_surface: Option<IdentityProbeStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub unsupported_surfaces: Vec<String>,
}

/// Runtime stealth coverage projected for diagnostics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StealthCoverageInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_hook_installations: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_hook_failures: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iframe_targets_detected: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_targets_detected: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_worker_targets_detected: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_worker_targets_detected: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent_override: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent_metadata_override: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub observed_target_types: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_probe: Option<IdentitySelfProbeInfo>,
}
