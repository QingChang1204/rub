use rub_core::model::TriggerEvidenceInfo;
use serde::{Deserialize, Serialize};

mod local;
mod matching;
mod remote;

pub(crate) use local::evaluate_orchestration_probe_for_tab;
pub(crate) use remote::dispatch_remote_orchestration_probe;

/// Structured, bounded probe result used by orchestration workers and the
/// internal `_orchestration_probe` command surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrchestrationProbeResult {
    pub matched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TriggerEvidenceInfo>,
    pub next_network_cursor: u64,
    #[serde(default)]
    pub observed_drop_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

#[cfg(test)]
mod tests;
