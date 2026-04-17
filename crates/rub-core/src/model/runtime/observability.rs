use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Runtime status of the session-scoped observability surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeObservatoryStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// A browser console error collected by the runtime observatory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleErrorEvent {
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A page-level error collected by the runtime observatory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageErrorEvent {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// A failed request collected by the runtime observatory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkFailureEvent {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub error_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_rule_effects: Vec<NetworkRuleEffect>,
}

/// Action kind applied by a session-scoped network rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRuleEffectKind {
    Rewrite,
    Block,
    Allow,
    HeaderOverride,
}

/// A concrete network rule effect correlated with an observed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRuleEffect {
    pub rule_id: u32,
    pub kind: NetworkRuleEffectKind,
}

/// A summarized request observation collected by the runtime observatory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSummaryEvent {
    pub request_id: String,
    pub url: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_rule_effects: Vec<NetworkRuleEffect>,
}

/// Bounded body projection captured for request/response inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkBodyPreview {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted_reason: Option<String>,
}

/// Lifecycle state of a recorded network request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRequestLifecycle {
    Pending,
    Responded,
    Completed,
    Failed,
}

/// Pre-authority request lifecycle observation emitted before daemon sequencing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedNetworkRequestRecord {
    pub request_id: String,
    pub lifecycle: NetworkRequestLifecycle,
    pub url: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub response_headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<NetworkBodyPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<NetworkBodyPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_rule_effects: Vec<NetworkRuleEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Detailed request lifecycle record captured by the network inspection runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRequestRecord {
    pub request_id: String,
    pub sequence: u64,
    pub lifecycle: NetworkRequestLifecycle,
    pub url: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub response_headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<NetworkBodyPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<NetworkBodyPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_rule_effects: Vec<NetworkRuleEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// A sequenced runtime observability event correlated with a command window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeObservatoryEvent {
    pub sequence: u64,
    #[serde(flatten)]
    pub payload: RuntimeObservatoryEventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "event", rename_all = "snake_case")]
pub enum RuntimeObservatoryEventPayload {
    ConsoleError(ConsoleErrorEvent),
    PageError(PageErrorEvent),
    NetworkFailure(NetworkFailureEvent),
    RequestSummary(RequestSummaryEvent),
}

/// Session-scoped runtime observability projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeObservatoryInfo {
    pub status: RuntimeObservatoryStatus,
    pub recent_console_errors: Vec<ConsoleErrorEvent>,
    pub recent_page_errors: Vec<PageErrorEvent>,
    pub recent_network_failures: Vec<NetworkFailureEvent>,
    pub recent_requests: Vec<RequestSummaryEvent>,
    #[serde(default)]
    pub dropped_event_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for RuntimeObservatoryInfo {
    fn default() -> Self {
        Self {
            status: RuntimeObservatoryStatus::Inactive,
            recent_console_errors: Vec::new(),
            recent_page_errors: Vec::new(),
            recent_network_failures: Vec::new(),
            recent_requests: Vec::new(),
            dropped_event_count: 0,
            degraded_reason: None,
        }
    }
}
