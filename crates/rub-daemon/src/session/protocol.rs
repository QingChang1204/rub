use super::SessionState;
use rub_core::model::{
    DialogKind, DialogRuntimeStatus, DownloadMode, DownloadRuntimeStatus, DownloadState,
};
use rub_ipc::codec::MAX_FRAME_BYTES;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

pub(crate) const REPLAY_CACHE_LIMIT: usize = 1000;
pub(crate) const REPLAY_CACHE_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 8;
pub(crate) const POST_COMMIT_PROJECTION_LIMIT: usize = 256;
pub(crate) const POST_COMMIT_PROJECTION_LIMIT_BYTES: usize = MAX_FRAME_BYTES * 4;

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
    pub(crate) total_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayInFlightEntry {
    pub(crate) fingerprint: String,
    pub(crate) sender: tokio::sync::watch::Sender<ReplayFenceState>,
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
        status: DialogRuntimeStatus,
        degraded_reason: Option<String>,
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
        status: DownloadRuntimeStatus,
        mode: DownloadMode,
        download_dir: Option<String>,
        degraded_reason: Option<String>,
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
            | Self::DownloadStarted {
                browser_sequence, ..
            }
            | Self::DownloadProgress {
                browser_sequence, ..
            } => *browser_sequence,
        }
    }
}

#[derive(Clone)]
pub struct BrowserSessionEventSink {
    pub(crate) state: Arc<SessionState>,
    pub(crate) tx: tokio::sync::mpsc::UnboundedSender<BrowserSessionEvent>,
}

#[derive(Debug, Clone)]
pub enum ReplayCommandClaim {
    Owner,
    Cached(Box<rub_ipc::protocol::IpcResponse>),
    Wait(tokio::sync::watch::Receiver<ReplayFenceState>),
    Conflict,
}
