use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::error::ErrorEnvelope;
use crate::locator::CanonicalLocator;

/// Interactive element tag types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementTag {
    Button,
    Link,
    Input,
    TextArea,
    Select,
    Checkbox,
    Radio,
    Option,
    /// Element with role="button", role="link", or onclick handler
    Other,
}

/// Bounding box for element visibility checks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Live DOM content-anchor match returned by the explicit content-find surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentFindMatch {
    pub tag_name: String,
    pub text: String,
    pub role: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub testid: Option<String>,
}

/// Scroll position of the viewport.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScrollPosition {
    pub x: f64,
    pub y: f64,
    pub at_bottom: bool,
}

/// How the browser-side interaction was actuated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionActuation {
    Pointer,
    Keyboard,
    Semantic,
    Programmatic,
}

/// Intent-level semantic class for an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionSemanticClass {
    Activate,
    Hover,
    SetValue,
    ToggleState,
    SelectChoice,
    NavigateContext,
    InvokeWorkflow,
}

/// Confirmation status for an interaction effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionConfirmationStatus {
    Confirmed,
    Unconfirmed,
    Contradicted,
    Degraded,
}

/// What kind of effect was observed for an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionConfirmationKind {
    ToggleState,
    HoverState,
    FocusChange,
    ElementStateChange,
    FilesAttached,
    ValueApplied,
    SelectionApplied,
    ContextChange,
    PageMutation,
    DialogOpened,
}

/// Structured confirmation payload published for an interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionConfirmation {
    pub status: InteractionConfirmationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<InteractionConfirmationKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Metadata produced by a browser-side interaction attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionOutcome {
    /// Semantic interaction class the runtime executed.
    pub semantic_class: InteractionSemanticClass,
    /// True when the adapter acted on a verified CDP node reference.
    pub element_verified: bool,
    /// Execution strategy used to actuate the interaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actuation: Option<InteractionActuation>,
    /// Whether the intended effect was confirmed after actuation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<InteractionConfirmation>,
}

/// Metadata returned after selecting an option in a dropdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOutcome {
    pub semantic_class: InteractionSemanticClass,
    pub element_verified: bool,
    pub selected_value: String,
    pub selected_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actuation: Option<InteractionActuation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<InteractionConfirmation>,
}

/// Interactive DOM element from a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Element {
    /// Sequential index (0-based), valid only within its snapshot.
    pub index: u32,
    /// Element type.
    pub tag: ElementTag,
    /// Visible text content (truncated to 200 chars).
    pub text: String,
    /// Relevant attributes: href, placeholder, aria-label, type, name, value, role.
    pub attributes: HashMap<String, String>,
    /// Stable CDP-derived identifier: "frame_id:backend_node_id".
    /// Survives re-renders if the underlying DOM node persists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_ref: Option<String>,
    /// For visibility checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounding_box: Option<BoundingBox>,
    /// Accessibility info (only populated when `--a11y` flag is set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ax_info: Option<AXInfo>,
    /// JS event listeners detected via `getEventListeners()` (only with `--listeners`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listeners: Option<Vec<String>>,
    /// Projection-relative DOM depth. Unscoped snapshots use document-root depth;
    /// scoped observation snapshots rebase depth to the selected scope root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

// ── v1.1 Model Types ──────────────────────────────────────────────────

/// Accessibility information for element augmentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AXInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessible_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessible_description: Option<String>,
}

/// Tab metadata visible to CLI consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    /// Positional index in creation order (0-based).
    pub index: u32,
    /// CDP target identifier (opaque, stable within session).
    pub target_id: String,
    /// Current URL of the tab.
    pub url: String,
    /// Current page title.
    pub title: String,
    /// Whether this is the currently active tab.
    pub active: bool,
}

/// Keyboard modifier keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Control,
    Shift,
    Alt,
    Meta,
}

/// Parsed key combination (e.g., "Control+Shift+a").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCombo {
    /// W3C UIEvents key name (e.g., "Enter", "a").
    pub key: String,
    /// Active modifier keys.
    pub modifiers: Vec<Modifier>,
}

impl KeyCombo {
    /// Parse a key combination string like "Control+Shift+a" or "Enter".
    ///
    /// Rules:
    /// - Modifiers are separated by `+`
    /// - The last segment is the key name
    /// - Known modifiers: Control, Shift, Alt, Meta (case-insensitive)
    /// - Returns error for unrecognized modifier names
    ///
    /// Note: The final key name is NOT validated here — that's done by the
    /// CDP layer which owns the key mapping table. This method only validates
    /// modifier names, since those are part of the domain model.
    pub fn parse(input: &str) -> Result<Self, crate::error::RubError> {
        if input.is_empty() {
            return Err(crate::error::RubError::domain(
                crate::error::ErrorCode::InvalidKeyName,
                "Key name cannot be empty",
            ));
        }

        let parts: Vec<&str> = input.split('+').collect();
        if parts.is_empty() {
            return Err(crate::error::RubError::domain(
                crate::error::ErrorCode::InvalidKeyName,
                "Key name cannot be empty",
            ));
        }

        let mut modifiers = Vec::new();
        // All parts except the last are modifiers
        for part in &parts[..parts.len() - 1] {
            let modifier = Self::parse_modifier(part.trim()).ok_or_else(|| {
                crate::error::RubError::domain(
                    crate::error::ErrorCode::InvalidKeyName,
                    format!(
                        "Unknown modifier: '{}'. Known modifiers: Control, Shift, Alt, Meta",
                        part
                    ),
                )
            })?;
            modifiers.push(modifier);
        }

        let key = parts
            .last()
            .map(|part| part.trim().to_string())
            .ok_or_else(|| {
                crate::error::RubError::domain(
                    crate::error::ErrorCode::InvalidKeyName,
                    "Key name cannot be empty",
                )
            })?;
        if key.is_empty() {
            return Err(crate::error::RubError::domain(
                crate::error::ErrorCode::InvalidKeyName,
                "Key name cannot be empty (trailing +?)",
            ));
        }

        Ok(Self { key, modifiers })
    }

    fn parse_modifier(s: &str) -> Option<Modifier> {
        match s.to_lowercase().as_str() {
            "control" | "ctrl" => Some(Modifier::Control),
            "shift" => Some(Modifier::Shift),
            "alt" => Some(Modifier::Alt),
            "meta" | "cmd" | "command" => Some(Modifier::Meta),
            _ => None,
        }
    }
}

/// Wait condition type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitKind {
    Locator {
        locator: CanonicalLocator,
        state: WaitState,
    },
    Text {
        text: String,
    },
}

/// Element state for wait conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitState {
    Visible,
    Hidden,
    Attached,
    Detached,
}

/// A condition to wait for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitCondition {
    pub kind: WaitKind,
    pub timeout_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
}

/// Browser cookie (mirrors CDP CookieParam).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
}

/// Immutable snapshot of the page DOM state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unique identifier (UUID v7).
    pub snapshot_id: String,
    /// Epoch at time of capture.
    pub dom_epoch: u64,
    /// Canonical frame context the snapshot was captured from.
    pub frame_context: FrameContextInfo,
    /// Canonical frame lineage for the captured context (current -> root).
    #[serde(default)]
    pub frame_lineage: Vec<String>,
    /// Current page URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Interactive elements in DOM preorder.
    pub elements: Vec<Element>,
    /// Total element count before truncation.
    pub total_count: u32,
    /// True if elements.len() < total_count.
    pub truncated: bool,
    /// Current scroll position.
    pub scroll: ScrollPosition,
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Projection integrity diagnostics for DOM-to-backend mapping.
    pub projection: SnapshotProjection,
    /// True when viewport filtering was applied (v1.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_filtered: Option<bool>,
    /// Number of elements visible in the viewport (v1.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_count: Option<u32>,
}

/// Integrity diagnostics for DOM projection mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotProjection {
    pub verified: bool,
    pub js_traversal_count: u32,
    pub backend_traversal_count: u32,
    pub resolved_ref_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Page metadata returned from navigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub url: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub final_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_warning: Option<String>,
}

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
    pub source_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_written: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_area: Option<String>,
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

/// One registry-backed session addressable by future orchestration rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationSessionInfo {
    pub session_id: String,
    pub session_name: String,
    pub pid: u32,
    pub socket_path: String,
    pub current: bool,
    pub ipc_protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
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

/// Session-scoped mode for public-web interference handling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceMode {
    #[default]
    Normal,
    PublicWebStable,
    Strict,
}

/// Runtime status of the public-web interference surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// Classified kind of public-web interference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceKind {
    InterstitialNavigation,
    PopupHijack,
    OverlayInterference,
    ThirdPartyNoise,
    HumanVerificationRequired,
    UnknownNavigationDrift,
}

/// Explicit safe recovery action chosen by the interference runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRecoveryAction {
    BackNavigate,
    CloseUnexpectedTab,
    RestorePrimaryContext,
    DismissOverlay,
    EscalateToHandoff,
}

/// Outcome of the most recent interference recovery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterferenceRecoveryResult {
    Succeeded,
    Failed,
    Abandoned,
    Escalated,
}

/// Structured report for an explicit interference recovery attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceRecoveryReport {
    pub attempted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<InterferenceRecoveryAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<InterferenceRecoveryResult>,
    pub fence_satisfied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Structured observation describing a classified interference event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceObservation {
    pub kind: InterferenceKind,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_url: Option<String>,
}

/// Session-scoped public-web interference runtime projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceRuntimeInfo {
    pub mode: InterferenceMode,
    pub status: InterferenceRuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_interference: Option<InterferenceObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_interference: Option<InterferenceObservation>,
    #[serde(default)]
    pub active_policies: Vec<String>,
    pub recovery_in_progress: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recovery_action: Option<InterferenceRecoveryAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recovery_result: Option<InterferenceRecoveryResult>,
    pub handoff_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for InterferenceRuntimeInfo {
    fn default() -> Self {
        Self {
            mode: InterferenceMode::Normal,
            status: InterferenceRuntimeStatus::Inactive,
            current_interference: None,
            last_interference: None,
            active_policies: Vec::new(),
            recovery_in_progress: false,
            last_recovery_action: None,
            last_recovery_result: None,
            handoff_required: false,
            degraded_reason: None,
        }
    }
}

/// Delta between two interference runtime snapshots captured around a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterferenceStateDelta {
    pub before: InterferenceRuntimeInfo,
    pub after: InterferenceRuntimeInfo,
    #[serde(default)]
    pub changed: Vec<String>,
}

/// Attachment status of a session-scoped network rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRuleStatus {
    Configured,
    Active,
    Degraded,
}

/// Session-scoped network rule specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkRuleSpec {
    Rewrite {
        url_pattern: String,
        target_base: String,
    },
    Block {
        url_pattern: String,
    },
    Allow {
        url_pattern: String,
    },
    HeaderOverride {
        url_pattern: String,
        headers: BTreeMap<String, String>,
    },
}

/// Session-scoped request rule projected by the developer integration runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRule {
    pub id: u32,
    pub status: NetworkRuleStatus,
    #[serde(flatten)]
    pub spec: NetworkRuleSpec,
}

/// Browser launch policy projected for diagnostics and agent policy checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPolicyInfo {
    pub headless: bool,
    pub ignore_cert_errors: bool,
    pub hide_infobars: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
    /// How the browser was attached (v1.3: external CDP, profile, auto-discovered, or managed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_target: Option<ConnectionTarget>,
    // v1.4: Stealth diagnostics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_patches: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_default_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humanize_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub humanize_speed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stealth_coverage: Option<StealthCoverageInfo>,
}

// ── v1.3 Model Types ──────────────────────────────────────────────────

/// How the browser was connected/attached.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ConnectionTarget {
    /// `--cdp-url <ws://... or http://...>`
    CdpUrl { url: String },
    /// `--connect` (auto-discovered CDP endpoint)
    AutoDiscovered { url: String, port: u16 },
    /// `--profile <name>` (resolved to a user-data-dir path)
    Profile { name: String, resolved_path: String },
    /// Default: daemon launches and owns its own browser.
    Managed,
}

/// Result of comparing two DOM snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResult {
    /// ID of the new (current) snapshot.
    pub snapshot_id: String,
    /// ID of the baseline snapshot being compared against.
    pub diff_base: String,
    /// Epoch of the new snapshot.
    pub dom_epoch: u64,
    /// Whether any changes were detected.
    pub has_changes: bool,
    /// Elements present in current but not in baseline.
    pub added: Vec<DiffElement>,
    /// Elements present in baseline but not in current.
    pub removed: Vec<DiffElement>,
    /// Elements present in both but with different content.
    pub changed: Vec<ChangedElement>,
    /// Number of elements that are identical in both snapshots.
    pub unchanged_count: u32,
    /// Semantic summary of the diff for agent-friendly consumption.
    pub summary: DiffSummary,
}

/// Lightweight element info used in diff added/removed lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffElement {
    pub index: u32,
    pub tag: ElementTag,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_ref: Option<String>,
}

/// Element that exists in both snapshots but with changed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedElement {
    /// Index in the current snapshot.
    pub index: u32,
    pub tag: ElementTag,
    /// Semantic classes of the observed changes.
    pub semantic_kinds: Vec<DiffSemanticKind>,
    /// List of field-level changes.
    pub changes: Vec<FieldChange>,
}

/// A single field-level change between two snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChange {
    /// Field name (e.g., "text", "attributes.href", "tag").
    pub field: String,
    /// Previous value.
    pub from: String,
    /// Current value.
    pub to: String,
}

/// Semantic classification for a field-level snapshot change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSemanticKind {
    Identity,
    Content,
    Value,
    Attributes,
    Geometry,
    Accessibility,
    Listeners,
}

/// Semantic summary of a snapshot diff.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    pub added_count: u32,
    pub removed_count: u32,
    pub changed_count: u32,
    pub content_changes: u32,
    pub value_changes: u32,
    pub attribute_changes: u32,
    pub geometry_changes: u32,
    pub accessibility_changes: u32,
    pub listener_changes: u32,
    pub identity_changes: u32,
}

/// Unified command result envelope for stdout JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub command: String,
    pub stdout_schema_version: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub session: String,
    pub timing: Timing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
}

impl CommandResult {
    /// Protocol version constant.
    pub const STDOUT_SCHEMA_VERSION: &'static str = "3.0";

    /// Create a success result.
    pub fn success(
        command: impl Into<String>,
        session: impl Into<String>,
        request_id: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            success: true,
            command: command.into(),
            stdout_schema_version: Self::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: request_id.into(),
            command_id: None,
            session: session.into(),
            timing: Timing::default(),
            data: Some(data),
            error: None,
        }
    }

    /// Create an error result.
    pub fn error(
        command: impl Into<String>,
        session: impl Into<String>,
        request_id: impl Into<String>,
        envelope: ErrorEnvelope,
    ) -> Self {
        Self {
            success: false,
            command: command.into(),
            stdout_schema_version: Self::STDOUT_SCHEMA_VERSION.to_string(),
            request_id: request_id.into(),
            command_id: None,
            session: session.into(),
            timing: Timing::default(),
            data: None,
            error: Some(envelope),
        }
    }

    /// Set the command_id.
    pub fn with_command_id(mut self, id: impl Into<String>) -> Self {
        self.command_id = Some(id.into());
        self
    }

    /// Set the timing.
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }
}

/// Load strategy for page navigation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoadStrategy {
    #[default]
    Load,
    #[serde(rename = "domcontentloaded")]
    DomContentLoaded,
    #[serde(rename = "networkidle")]
    NetworkIdle,
}

/// Scroll direction for the scroll command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_result_success_serializes() {
        let result = CommandResult::success(
            "open",
            "default",
            "req-123",
            serde_json::json!({"url": "https://example.com"}),
        );
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["command"], "open");
        assert_eq!(json["stdout_schema_version"], "3.0");
        assert!(json["error"].is_null());
    }

    #[test]
    fn command_result_error_serializes() {
        let envelope =
            crate::error::ErrorEnvelope::new(crate::error::ErrorCode::NavigationFailed, "DNS fail");
        let result = CommandResult::error("open", "default", "req-456", envelope);
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["error"]["code"], "NAVIGATION_FAILED");
        assert!(json["data"].is_null());
    }

    #[test]
    fn load_strategy_serializes() {
        assert_eq!(
            serde_json::to_string(&LoadStrategy::DomContentLoaded).unwrap(),
            "\"domcontentloaded\""
        );
        assert_eq!(
            serde_json::to_string(&LoadStrategy::NetworkIdle).unwrap(),
            "\"networkidle\""
        );
    }

    #[test]
    fn element_tag_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ElementTag::Button).unwrap(),
            "\"button\""
        );
        assert_eq!(
            serde_json::to_string(&ElementTag::TextArea).unwrap(),
            "\"textarea\""
        );
    }

    #[test]
    fn key_combo_parse_single() {
        let combo = KeyCombo::parse("Enter").unwrap();
        assert_eq!(combo.key, "Enter");
        assert!(combo.modifiers.is_empty());
    }

    #[test]
    fn key_combo_parse_with_modifier() {
        let combo = KeyCombo::parse("Control+a").unwrap();
        assert_eq!(combo.key, "a");
        assert_eq!(combo.modifiers, vec![Modifier::Control]);
    }

    #[test]
    fn key_combo_parse_multiple_modifiers() {
        let combo = KeyCombo::parse("Control+Shift+Enter").unwrap();
        assert_eq!(combo.key, "Enter");
        assert_eq!(combo.modifiers.len(), 2);
        assert!(combo.modifiers.contains(&Modifier::Control));
        assert!(combo.modifiers.contains(&Modifier::Shift));
    }

    #[test]
    fn key_combo_parse_modifier_aliases() {
        let combo = KeyCombo::parse("Ctrl+a").unwrap();
        assert_eq!(combo.modifiers, vec![Modifier::Control]);

        let combo = KeyCombo::parse("Cmd+c").unwrap();
        assert_eq!(combo.modifiers, vec![Modifier::Meta]);
    }

    #[test]
    fn key_combo_parse_empty_error() {
        assert!(KeyCombo::parse("").is_err());
    }

    #[test]
    fn key_combo_parse_unknown_modifier_error() {
        let err = KeyCombo::parse("FooBar+a").unwrap_err();
        let envelope = err.into_envelope();
        assert_eq!(envelope.code, crate::error::ErrorCode::InvalidKeyName);
    }
}
