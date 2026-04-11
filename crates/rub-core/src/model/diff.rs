use serde::{Deserialize, Serialize};

use super::interaction::ElementTag;

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
