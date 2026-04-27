use super::SessionState;
use rub_core::model::{
    DialogKind, DialogRuntimeInfo, DownloadEvent, DownloadRuntimeInfo, DownloadState,
};
use rub_ipc::codec::MAX_FRAME_BYTES;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

pub(crate) const REPLAY_CACHE_LIMIT: usize = 1000;
pub(crate) const REPLAY_CACHE_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 8;
#[cfg(test)]
pub(crate) const REPLAY_SPENT_LIMIT: usize = REPLAY_CACHE_LIMIT * 4;
pub(crate) const POST_COMMIT_PROJECTION_LIMIT: usize = 256;
pub(crate) const POST_COMMIT_PROJECTION_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 4;
pub(crate) const BROWSER_EVENT_PROGRESS_INGRESS_LIMIT: usize = 1_024;
pub(crate) const BROWSER_EVENT_CRITICAL_SOFT_LIMIT: u32 = 256;
pub(crate) const DOWNLOAD_PROGRESS_OVERFLOW_REASON: &str =
    "browser_event_ingress_overflow:download_progress";

#[derive(Debug, Clone)]
pub(crate) struct ReplayCacheEntry {
    pub(crate) fingerprint: String,
    pub(crate) response: rub_ipc::protocol::IpcResponse,
    pub(crate) approx_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PostCommitProjection {
    pub(crate) request: rub_ipc::protocol::IpcRequest,
    pub(crate) response: rub_ipc::protocol::IpcResponse,
    pub(crate) workflow_capture_response: rub_ipc::protocol::IpcResponse,
    pub(crate) workflow_capture_delivery_state:
        crate::workflow_capture::WorkflowCaptureDeliveryState,
    pub(crate) approx_bytes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PostCommitProjectionQueue {
    pub(crate) entries: VecDeque<PostCommitProjection>,
    pub(crate) total_bytes: usize,
    pub(crate) dropped_before_projection: u64,
}

#[derive(Debug, Default)]
pub(crate) struct ReplayProtocolState {
    pub(crate) cache: HashMap<String, ReplayCacheEntry>,
    pub(crate) order: VecDeque<String>,
    pub(crate) in_flight: HashMap<String, ReplayInFlightEntry>,
    pub(crate) spent: HashMap<String, ReplaySpentEntry>,
    pub(crate) spent_order: VecDeque<String>,
    pub(crate) total_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayInFlightEntry {
    pub(crate) fingerprint: String,
    pub(crate) sender: tokio::sync::watch::Sender<ReplayFenceState>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplaySpentEntry {
    pub(crate) fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayFenceState {
    InFlight,
    Released,
}

pub enum BrowserSessionEvent {
    DialogRuntime {
        browser_sequence: u64,
        generation: u64,
        runtime: Box<DialogRuntimeInfo>,
    },
    DialogOpened {
        browser_sequence: u64,
        generation: u64,
        kind: DialogKind,
        message: String,
        url: String,
        tab_target_id: Option<String>,
        frame_id: Option<String>,
        default_prompt: Option<String>,
        has_browser_handler: bool,
    },
    DialogClosed {
        browser_sequence: u64,
        generation: u64,
        accepted: bool,
        user_input: String,
    },
    DownloadRuntime {
        browser_sequence: u64,
        generation: u64,
        runtime: Box<DownloadRuntimeInfo>,
    },
    DownloadRuntimeDegradedMarker {
        browser_sequence: u64,
        generation: u64,
        reason: String,
    },
    DownloadStarted {
        browser_sequence: u64,
        generation: u64,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    },
    DownloadProgress {
        browser_sequence: u64,
        generation: u64,
        guid: String,
        state: DownloadState,
        received_bytes: u64,
        total_bytes: Option<u64>,
        final_path: Option<String>,
    },
}

impl BrowserSessionEvent {
    pub fn browser_sequence(&self) -> u64 {
        match self {
            Self::DialogRuntime {
                browser_sequence, ..
            }
            | Self::DialogOpened {
                browser_sequence, ..
            }
            | Self::DialogClosed {
                browser_sequence, ..
            }
            | Self::DownloadRuntime {
                browser_sequence, ..
            }
            | Self::DownloadRuntimeDegradedMarker {
                browser_sequence, ..
            }
            | Self::DownloadStarted {
                browser_sequence, ..
            }
            | Self::DownloadProgress {
                browser_sequence, ..
            } => *browser_sequence,
        }
    }

    pub fn uses_bounded_progress_ingress(&self) -> bool {
        matches!(
            self,
            Self::DownloadProgress {
                state: DownloadState::InProgress,
                ..
            }
        )
    }
}

#[derive(Clone)]
pub struct BrowserSessionEventSink {
    pub(crate) state: Arc<SessionState>,
    pub(crate) critical_tx: tokio::sync::mpsc::UnboundedSender<BrowserSessionEvent>,
    pub(crate) progress_tx: tokio::sync::mpsc::Sender<BrowserSessionEvent>,
    pub(crate) progress_overflow_coordination: Arc<std::sync::Mutex<()>>,
    pub(crate) progress_overflow_latched: Arc<AtomicBool>,
    pub(crate) progress_overflow_latched_generation: Arc<AtomicU64>,
    pub(crate) progress_overflow_latched_sequence: Arc<AtomicU64>,
    pub(crate) progress_overflow_reopen_generation: Arc<AtomicU64>,
    pub(crate) progress_overflow_reopen_sequence: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadEventWindow {
    pub(crate) events: Vec<DownloadEvent>,
    pub(crate) authoritative: bool,
    pub(crate) degraded_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ReplayCommandClaim {
    Owner,
    Cached(Box<rub_ipc::protocol::IpcResponse>),
    Wait(tokio::sync::watch::Receiver<ReplayFenceState>),
    SpentWithoutCachedResponse,
    Conflict,
}
