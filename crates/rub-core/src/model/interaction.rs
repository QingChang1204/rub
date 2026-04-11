use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::locator::CanonicalLocator;

use super::runtime::FrameContextInfo;

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
