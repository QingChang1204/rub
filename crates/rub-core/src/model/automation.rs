use serde::{Deserialize, Serialize};

use crate::locator::CanonicalLocator;
use crate::storage::StorageArea;

/// Runtime status of the trigger surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// Lifecycle status of one trigger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerStatus {
    Armed,
    Paused,
    Fired,
    Blocked,
    Degraded,
    Expired,
}

/// Execution mode for one trigger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMode {
    Once,
}

fn default_trigger_mode() -> TriggerMode {
    TriggerMode::Once
}

fn default_trigger_lifecycle_generation() -> u64 {
    1
}

/// Canonical kind of trigger condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerConditionKind {
    TextPresent,
    LocatorPresent,
    UrlMatch,
    Readiness,
    NetworkRequest,
    StorageValue,
}

/// Extensible action envelope for trigger targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerActionKind {
    BrowserCommand,
    Workflow,
    Provider,
    Script,
    Webhook,
}

/// Stable tab binding captured for trigger source/target authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerTabBindingInfo {
    pub index: u32,
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    pub url: String,
    pub title: String,
}

/// Trigger source condition specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerConditionSpec {
    pub kind: TriggerConditionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<CanonicalLocator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::storage::deserialize_optional_storage_area"
    )]
    pub storage_area: Option<StorageArea>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// User-supplied registration spec for a new trigger rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerRegistrationSpec {
    pub source_tab: u32,
    pub target_tab: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_frame_id: Option<String>,
    #[serde(default = "default_trigger_mode")]
    pub mode: TriggerMode,
    pub condition: TriggerConditionSpec,
    pub action: TriggerActionSpec,
}

/// Extensible target action specification for trigger firing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerActionSpec {
    pub kind: TriggerActionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// Most recent evidence observed for a trigger condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerEvidenceInfo {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// Structured description of the automation action authority that actually ran (or was attempted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationActionInfo {
    pub kind: TriggerActionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_path_state: Option<PathReferenceState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_step_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_vars: Vec<String>,
}

pub type TriggerActionExecutionInfo = AutomationActionInfo;

/// Structured result emitted for the most recent trigger outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerResultInfo {
    pub trigger_id: u32,
    pub status: TriggerStatus,
    pub next_status: TriggerStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<TriggerActionExecutionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<crate::error::ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_context: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_evidence_fingerprint: Option<String>,
}

/// Canonical lifecycle event kinds published by the long-lived trigger trace surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEventKind {
    Registered,
    Paused,
    Resumed,
    Removed,
    Fired,
    Blocked,
    Degraded,
    Unavailable,
    Recovered,
}

/// One recent event published by the session-scoped trigger trace surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerEventInfo {
    pub sequence: u64,
    pub kind: TriggerEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<u32>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TriggerEvidenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TriggerResultInfo>,
}

/// Dedicated bounded trigger trace/history stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerTraceProjection {
    #[serde(default)]
    pub events: Vec<TriggerEventInfo>,
}

/// One configured trigger rule inside the session-scoped trigger runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerInfo {
    pub id: u32,
    pub status: TriggerStatus,
    #[serde(default = "default_trigger_lifecycle_generation", skip_serializing)]
    pub lifecycle_generation: u64,
    pub mode: TriggerMode,
    pub source_tab: TriggerTabBindingInfo,
    pub target_tab: TriggerTabBindingInfo,
    pub condition: TriggerConditionSpec,
    pub action: TriggerActionSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_condition_evidence: Option<TriggerEvidenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_evidence_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_action_result: Option<TriggerResultInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// Session-scoped trigger registry projection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerRuntimeInfo {
    pub status: TriggerRuntimeStatus,
    #[serde(default)]
    pub triggers: Vec<TriggerInfo>,
    pub active_count: usize,
    pub degraded_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_trigger_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_trigger_result: Option<TriggerResultInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// Runtime status of the cross-session orchestration foundation surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// Canonical orchestration rule status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationRuleStatus {
    Armed,
    Paused,
    Fired,
    Blocked,
    Degraded,
    Expired,
}

/// Session-scoped orchestration execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationMode {
    Once,
    Repeat,
}

fn default_orchestration_mode() -> OrchestrationMode {
    OrchestrationMode::Once
}

fn default_orchestration_rule_generation() -> u64 {
    1
}

/// User-supplied execution policy for orchestration rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationExecutionPolicySpec {
    #[serde(default)]
    pub cooldown_ms: u64,
    #[serde(default)]
    pub max_retries: u32,
}

/// Canonical execution policy currently applied to an orchestration rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OrchestrationExecutionPolicyInfo {
    #[serde(default)]
    pub cooldown_ms: u64,
    #[serde(default)]
    pub max_retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_until_ms: Option<u64>,
}

/// Rule-side address spec for future orchestration routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationAddressSpec {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
}

/// Canonical address info currently resolved for an orchestration rule endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationAddressInfo {
    pub session_id: String,
    pub session_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
}

/// User-supplied registration spec for a new orchestration rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationRegistrationSpec {
    pub source: OrchestrationAddressSpec,
    pub target: OrchestrationAddressSpec,
    #[serde(default = "default_orchestration_mode")]
    pub mode: OrchestrationMode,
    #[serde(default)]
    pub execution_policy: OrchestrationExecutionPolicySpec,
    pub condition: TriggerConditionSpec,
    #[serde(default)]
    pub actions: Vec<TriggerActionSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// One configured orchestration rule inside the session-scoped orchestration runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationRuleInfo {
    pub id: u32,
    pub status: OrchestrationRuleStatus,
    #[serde(default = "default_orchestration_rule_generation", skip_serializing)]
    pub lifecycle_generation: u64,
    pub source: OrchestrationAddressInfo,
    pub target: OrchestrationAddressInfo,
    pub mode: OrchestrationMode,
    #[serde(default)]
    pub execution_policy: OrchestrationExecutionPolicyInfo,
    pub condition: TriggerConditionSpec,
    #[serde(default)]
    pub actions: Vec<TriggerActionSpec>,
    pub correlation_key: String,
    pub idempotency_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_condition_evidence: Option<TriggerEvidenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<OrchestrationResultInfo>,
}

/// Canonical group projection for rules sharing the same correlation key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationGroupInfo {
    pub correlation_key: String,
    #[serde(default)]
    pub rule_ids: Vec<u32>,
    pub active_rule_count: usize,
    pub cooldown_rule_count: usize,
    pub paused_rule_count: usize,
    pub unavailable_rule_count: usize,
}

/// Canonical lifecycle event kinds published by the orchestration trace surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationEventKind {
    Registered,
    Paused,
    Resumed,
    Removed,
    Fired,
    Blocked,
    Degraded,
    Unavailable,
    Recovered,
}

/// Step-wise status for one committed orchestration action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationStepStatus {
    Committed,
    Blocked,
    Degraded,
    Skipped,
}

pub type OrchestrationActionExecutionInfo = AutomationActionInfo;

/// Structured step result emitted for one orchestration action inside a pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationStepResultInfo {
    pub step_index: u32,
    pub status: OrchestrationStepStatus,
    pub summary: String,
    #[serde(default = "default_attempt_count")]
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<OrchestrationActionExecutionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<crate::error::ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_context: Option<serde_json::Value>,
}

/// Structured result emitted for the most recent orchestration execution attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationResultInfo {
    pub rule_id: u32,
    pub status: OrchestrationRuleStatus,
    pub next_status: OrchestrationRuleStatus,
    pub summary: String,
    pub committed_steps: u32,
    pub total_steps: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<OrchestrationStepResultInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_until_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<crate::error::ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_context: Option<serde_json::Value>,
}

/// One recent event published by the session-scoped orchestration trace surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationEventInfo {
    pub sequence: u64,
    pub kind: OrchestrationEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<u32>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TriggerEvidenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<crate::error::ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_context: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
}

/// Dedicated bounded orchestration trace/history stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationTraceProjection {
    #[serde(default)]
    pub events: Vec<OrchestrationEventInfo>,
}

/// Truth label for a display-only path reference projected on an operator surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathReferenceState {
    pub truth_level: String,
    pub path_authority: String,
    pub upstream_truth: String,
    pub path_kind: String,
    pub control_role: String,
}

/// One registry-backed session addressable by future orchestration rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationSessionAvailability {
    Addressable,
    BusyOrUnknown,
    ProtocolIncompatible,
    HardCutReleasePending,
    PendingStartup,
    CurrentFallback,
}

/// One registry-backed session addressable by future orchestration rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationSessionInfo {
    pub session_id: String,
    pub session_name: String,
    pub pid: u32,
    pub socket_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_path_state: Option<PathReferenceState>,
    pub current: bool,
    pub ipc_protocol_version: String,
    pub availability: OrchestrationSessionAvailability,
    pub addressing_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir_state: Option<PathReferenceState>,
}

/// Read-only orchestration runtime foundation projected from the canonical RUB_HOME registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationRuntimeInfo {
    pub status: OrchestrationRuntimeStatus,
    #[serde(default)]
    pub known_sessions: Vec<OrchestrationSessionInfo>,
    #[serde(default)]
    pub rules: Vec<OrchestrationRuleInfo>,
    #[serde(default)]
    pub groups: Vec<OrchestrationGroupInfo>,
    pub session_count: usize,
    pub group_count: usize,
    pub active_rule_count: usize,
    pub cooldown_rule_count: usize,
    pub paused_rule_count: usize,
    pub unavailable_rule_count: usize,
    pub addressing_supported: bool,
    pub execution_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rule_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rule_result: Option<OrchestrationResultInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for OrchestrationRuntimeInfo {
    fn default() -> Self {
        Self {
            status: OrchestrationRuntimeStatus::Inactive,
            known_sessions: Vec::new(),
            rules: Vec::new(),
            groups: Vec::new(),
            session_count: 0,
            group_count: 0,
            active_rule_count: 0,
            cooldown_rule_count: 0,
            paused_rule_count: 0,
            unavailable_rule_count: 0,
            addressing_supported: false,
            execution_supported: false,
            current_session_id: None,
            current_session_name: None,
            last_rule_id: None,
            last_rule_result: None,
            degraded_reason: None,
        }
    }
}

fn default_attempt_count() -> u32 {
    1
}
