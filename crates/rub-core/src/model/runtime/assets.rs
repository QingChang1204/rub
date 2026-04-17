use serde::{Deserialize, Serialize};

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
