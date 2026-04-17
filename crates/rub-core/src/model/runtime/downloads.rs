use serde::{Deserialize, Serialize};

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
