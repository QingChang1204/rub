use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::session::NetworkRule;

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

/// Runtime status of the session-scoped download surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
    Unsupported,
}

/// Session-scoped download behavior mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadMode {
    #[default]
    ObserveOnly,
    Managed,
    Deny,
}

/// Lifecycle state of a browser download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Started,
    InProgress,
    Completed,
    Failed,
    Canceled,
}

/// One session-scoped browser download entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadEntry {
    pub guid: String,
    pub state: DownloadState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_hint: Option<String>,
    pub received_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_command_id: Option<String>,
}

/// Sequenced download event mirrored into diagnostics and interaction traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadEventKind {
    Started,
    Progress,
    Completed,
    Failed,
    Canceled,
}

/// One sequenced download runtime event.
///
/// Compatibility contract:
/// - `sequence` remains the stable lifecycle/event order fence for one session.
/// - The timeline is a historical projection, not a byte-for-byte immutable append log.
/// - When the browser emits terminal/progress state before the late `Started`
///   metadata, older events for the same `guid` may be backfilled with missing
///   `url` / `suggested_filename` / `frame_id` without changing their `sequence`
///   or `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadEvent {
    pub sequence: u64,
    pub kind: DownloadEventKind,
    pub download: DownloadEntry,
}

/// Session-scoped browser download runtime projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRuntimeInfo {
    pub status: DownloadRuntimeStatus,
    pub mode: DownloadMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_dir: Option<String>,
    #[serde(default)]
    pub active_downloads: Vec<DownloadEntry>,
    #[serde(default)]
    pub completed_downloads: Vec<DownloadEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_download: Option<DownloadEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for DownloadRuntimeInfo {
    fn default() -> Self {
        Self {
            status: DownloadRuntimeStatus::Inactive,
            mode: DownloadMode::ObserveOnly,
            download_dir: None,
            active_downloads: Vec::new(),
            completed_downloads: Vec::new(),
            last_download: None,
            degraded_reason: None,
        }
    }
}

/// Terminal status for one saved asset in a bulk save transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedAssetStatus {
    Saved,
    SkippedExisting,
    Failed,
    TimedOut,
}

/// One saved or attempted asset from a bulk save transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAssetEntry {
    pub index: u32,
    pub url: String,
    pub status: SavedAssetStatus,
    pub output_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path_state: Option<SavedAssetOutputPathState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_written: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Truth and durability label for the output path surfaced by one saved asset entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAssetOutputPathState {
    pub path_kind: String,
    pub path_authority: String,
    pub upstream_truth: String,
    pub control_role: String,
    pub durability: String,
}

/// Summary projection for one bulk asset save transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkAssetSaveSummary {
    pub complete: bool,
    pub source_count: u32,
    pub attempted_count: u32,
    pub saved_count: u32,
    pub skipped_existing_count: u32,
    pub failed_count: u32,
    pub timed_out_count: u32,
    pub output_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir_state: Option<BulkAssetSaveOutputDirState>,
}

/// Truth label for the bulk-save output directory reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkAssetSaveOutputDirState {
    pub path_kind: String,
    pub path_authority: String,
    pub upstream_truth: String,
    pub control_role: String,
}

/// Runtime status of the session-scoped JavaScript dialog surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// JavaScript dialog type surfaced by the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    Beforeunload,
}

/// One currently pending JavaScript dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDialogInfo {
    pub kind: DialogKind,
    pub message: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
    pub has_browser_handler: bool,
    pub opened_at: String,
}

/// Most recent dialog resolution observed by the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogResolutionInfo {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_input: Option<String>,
    pub closed_at: String,
}

/// Session-scoped JavaScript dialog runtime projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogRuntimeInfo {
    pub status: DialogRuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_dialog: Option<PendingDialogInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_dialog: Option<PendingDialogInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<DialogResolutionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for DialogRuntimeInfo {
    fn default() -> Self {
        Self {
            status: DialogRuntimeStatus::Inactive,
            pending_dialog: None,
            last_dialog: None,
            last_result: None,
            degraded_reason: None,
        }
    }
}

/// A pre-registered one-shot dialog handling intent.
///
/// When set, the CDP `EventJavascriptDialogOpening` listener consumes this
/// policy and immediately calls `Page.handleJavaScriptDialog` — before Chrome's
/// built-in handler can auto-dismiss the dialog.
///
/// This is the correct fix for `has_browser_handler: true` race conditions in
/// headless Chrome, where the browser may auto-dismiss dialogs before an
/// IPC-routed `dialog accept/dismiss` command can arrive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogInterceptPolicy {
    /// Whether to accept (`true`) or dismiss (`false`) the intercepted dialog.
    pub accept: bool,
    /// Optional text to provide if the dialog is a `prompt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
    /// Restrict this intercept to a specific tab (CDP target ID).
    ///
    /// When `Some`, the listener only consumes this policy if its own
    /// `tab_target_id` matches. When `None`, any tab may consume it
    /// (single-tab sessions only — not safe in multi-tab contexts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_tab_id: Option<String>,
}

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

/// Runtime status of the human verification handoff surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanVerificationHandoffStatus {
    Available,
    Active,
    Completed,
    #[default]
    Unavailable,
}

/// Session-scoped human verification handoff projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanVerificationHandoffInfo {
    pub status: HumanVerificationHandoffStatus,
    pub automation_paused: bool,
    pub resume_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl Default for HumanVerificationHandoffInfo {
    fn default() -> Self {
        Self {
            status: HumanVerificationHandoffStatus::Unavailable,
            automation_paused: false,
            resume_supported: false,
            unavailable_reason: Some("not_configured".to_string()),
        }
    }
}

/// Runtime status of the session takeover surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverRuntimeStatus {
    Available,
    Active,
    Degraded,
    #[default]
    Unavailable,
}

/// Whether the current session is directly accessible to a human operator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAccessibility {
    #[default]
    AutomationOnly,
    UserAccessible,
}

/// How the current browser session is exposed to the operator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverVisibilityMode {
    Headed,
    #[default]
    Headless,
    External,
}

/// Canonical transition kinds for the session takeover runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverTransitionKind {
    Start,
    Resume,
    Elevate,
}

/// Structured result of one takeover-runtime transition attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverTransitionResult {
    Succeeded,
    Rejected,
}

/// Most recent takeover-runtime transition published for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverTransitionInfo {
    pub kind: TakeoverTransitionKind,
    pub result: TakeoverTransitionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Session-scoped accessibility/takeover runtime projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverRuntimeInfo {
    pub status: TakeoverRuntimeStatus,
    pub session_accessibility: SessionAccessibility,
    pub visibility_mode: TakeoverVisibilityMode,
    pub elevate_supported: bool,
    pub resume_supported: bool,
    pub automation_paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_transition: Option<TakeoverTransitionInfo>,
}
