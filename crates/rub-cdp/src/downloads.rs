use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{
    CancelDownloadParams, DownloadProgressState, EventDownloadProgress, EventDownloadWillBegin,
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    DownloadEntry, DownloadMode, DownloadRuntimeInfo, DownloadRuntimeStatus, DownloadState,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing::warn;

use crate::listener_generation::{ListenerGeneration, ListenerGenerationRx, next_listener_event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRuntimeUpdate {
    pub generation: ListenerGeneration,
    pub runtime: DownloadRuntimeInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDownloadStart {
    pub generation: ListenerGeneration,
    pub guid: String,
    pub url: String,
    pub suggested_filename: String,
    pub frame_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDownloadProgress {
    pub generation: ListenerGeneration,
    pub guid: String,
    pub state: DownloadState,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub final_path: Option<String>,
}

type RuntimeCallback = Arc<dyn Fn(DownloadRuntimeUpdate) + Send + Sync>;
type StartCallback = Arc<dyn Fn(BrowserDownloadStart) + Send + Sync>;
type ProgressCallback = Arc<dyn Fn(BrowserDownloadProgress) + Send + Sync>;

const ACTIVE_DOWNLOAD_LIMIT: usize = 32;
const COMPLETED_DOWNLOAD_LIMIT: usize = 64;

#[derive(Clone, Default)]
pub struct DownloadCallbacks {
    pub on_runtime: Option<RuntimeCallback>,
    pub on_started: Option<StartCallback>,
    pub on_progress: Option<ProgressCallback>,
}

impl DownloadCallbacks {
    pub fn is_empty(&self) -> bool {
        self.on_runtime.is_none() && self.on_started.is_none() && self.on_progress.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DownloadRuntimeProjectionState {
    current_generation: ListenerGeneration,
    projection: DownloadRuntimeInfo,
    entries: HashMap<String, DownloadEntry>,
    active_order: VecDeque<String>,
    completed_order: VecDeque<String>,
}

pub(crate) type SharedDownloadRuntimeProjection =
    Arc<tokio::sync::RwLock<DownloadRuntimeProjectionState>>;

pub(crate) fn new_shared_download_runtime_projection() -> SharedDownloadRuntimeProjection {
    Arc::new(tokio::sync::RwLock::new(
        DownloadRuntimeProjectionState::default(),
    ))
}

impl DownloadRuntimeProjectionState {
    pub(crate) fn restore_snapshot(&mut self, snapshot: &DownloadRuntimeProjectionState) {
        *self = snapshot.clone();
    }

    pub(crate) fn mark_runtime_degraded(&mut self, reason: &str) {
        if self.current_generation == 0 {
            return;
        }
        self.projection.status = DownloadRuntimeStatus::Degraded;
        self.projection.degraded_reason =
            append_download_runtime_degraded_reason(self.projection.degraded_reason.take(), reason);
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn projection(&self) -> DownloadRuntimeInfo {
        self.projection.clone()
    }

    pub(crate) fn projection_with_generation(&self) -> (ListenerGeneration, DownloadRuntimeInfo) {
        (self.current_generation, self.projection())
    }

    pub(crate) fn set_runtime(
        &mut self,
        generation: ListenerGeneration,
        runtime: DownloadRuntimeInfo,
    ) {
        if !self.prepare_generation(generation) {
            return;
        }
        self.projection.status = runtime.status;
        self.projection.mode = runtime.mode;
        self.projection.download_dir = runtime.download_dir;
        self.projection.degraded_reason = runtime.degraded_reason;
    }

    pub(crate) fn record_started(
        &mut self,
        generation: ListenerGeneration,
        guid: String,
        url: String,
        suggested_filename: String,
        frame_id: Option<String>,
    ) {
        if !self.prepare_generation(generation) {
            return;
        }
        let entry = self
            .entries
            .entry(guid.clone())
            .or_insert_with(|| DownloadEntry {
                guid: guid.clone(),
                state: DownloadState::Started,
                url: None,
                suggested_filename: None,
                final_path: None,
                mime_hint: None,
                received_bytes: 0,
                total_bytes: None,
                started_at: rfc3339_now(),
                completed_at: None,
                frame_id: None,
                trigger_command_id: None,
            });
        entry.state = DownloadState::Started;
        entry.url = Some(url);
        entry.suggested_filename = Some(suggested_filename);
        entry.frame_id = frame_id;
        if entry.started_at.is_empty() {
            entry.started_at = rfc3339_now();
        }
        let snapshot = entry.clone();
        self.move_to_active(guid);
        self.projection.last_download = Some(snapshot);
        self.refresh_projection_lists();
    }

    pub(crate) fn record_progress(
        &mut self,
        generation: ListenerGeneration,
        guid: String,
        state: DownloadState,
        received_bytes: u64,
        total_bytes: Option<u64>,
        final_path: Option<String>,
    ) {
        if !self.prepare_generation(generation) {
            return;
        }
        let entry = self
            .entries
            .entry(guid.clone())
            .or_insert_with(|| DownloadEntry {
                guid: guid.clone(),
                state: DownloadState::Started,
                url: None,
                suggested_filename: None,
                final_path: None,
                mime_hint: None,
                received_bytes: 0,
                total_bytes: None,
                started_at: rfc3339_now(),
                completed_at: None,
                frame_id: None,
                trigger_command_id: None,
            });
        entry.state = state;
        entry.received_bytes = entry.received_bytes.max(received_bytes);
        if total_bytes.is_some() {
            entry.total_bytes = total_bytes;
        }
        if final_path.is_some() {
            entry.final_path = final_path;
        }
        if matches!(
            state,
            DownloadState::Completed | DownloadState::Failed | DownloadState::Canceled
        ) {
            entry.completed_at = Some(rfc3339_now());
        }
        let snapshot = entry.clone();
        if matches!(
            state,
            DownloadState::Completed | DownloadState::Failed | DownloadState::Canceled
        ) {
            self.move_to_completed(guid);
        } else {
            self.move_to_active(guid);
        }
        self.projection.last_download = Some(snapshot);
        self.refresh_projection_lists();
    }

    fn prepare_generation(&mut self, generation: ListenerGeneration) -> bool {
        if generation < self.current_generation {
            return false;
        }
        if generation > self.current_generation {
            self.current_generation = generation;
            self.projection = DownloadRuntimeInfo::default();
            self.entries.clear();
            self.active_order.clear();
            self.completed_order.clear();
        }
        true
    }

    fn move_to_active(&mut self, guid: String) {
        self.active_order.retain(|existing| existing != &guid);
        self.completed_order.retain(|existing| existing != &guid);
        self.active_order.push_back(guid.clone());
        while self.active_order.len() > ACTIVE_DOWNLOAD_LIMIT {
            if let Some(oldest) = self.active_order.pop_front()
                && !self
                    .completed_order
                    .iter()
                    .any(|existing| existing == &oldest)
            {
                self.entries.remove(&oldest);
            }
        }
    }

    fn move_to_completed(&mut self, guid: String) {
        self.active_order.retain(|existing| existing != &guid);
        self.completed_order.retain(|existing| existing != &guid);
        self.completed_order.push_back(guid.clone());
        while self.completed_order.len() > COMPLETED_DOWNLOAD_LIMIT {
            if let Some(oldest) = self.completed_order.pop_front()
                && !self.active_order.iter().any(|existing| existing == &oldest)
            {
                self.entries.remove(&oldest);
            }
        }
    }

    fn refresh_projection_lists(&mut self) {
        self.projection.active_downloads = self
            .active_order
            .iter()
            .filter_map(|guid| self.entries.get(guid).cloned())
            .collect();
        self.projection.completed_downloads = self
            .completed_order
            .iter()
            .filter_map(|guid| self.entries.get(guid).cloned())
            .collect();
    }
}

fn append_download_runtime_degraded_reason(
    existing: Option<String>,
    reason: &str,
) -> Option<String> {
    match existing {
        None => Some(reason.to_string()),
        Some(existing) if existing.split(',').any(|current| current.trim() == reason) => {
            Some(existing)
        }
        Some(existing) => Some(format!("{existing},{reason}")),
    }
}

pub(crate) struct DownloadRuntimeInstall {
    pub(crate) browser: Arc<Browser>,
    pub(crate) projection_state: SharedDownloadRuntimeProjection,
    pub(crate) callbacks: DownloadCallbacks,
    pub(crate) is_external: bool,
    pub(crate) download_dir: Option<PathBuf>,
    pub(crate) listener_generation: ListenerGeneration,
    pub(crate) listener_generation_rx: ListenerGenerationRx,
    pub(crate) authority_release_in_progress: Arc<AtomicBool>,
}

pub(crate) async fn install_browser_download_runtime(
    install: DownloadRuntimeInstall,
) -> DownloadRuntimeInfo {
    let DownloadRuntimeInstall {
        browser,
        projection_state,
        callbacks,
        is_external,
        download_dir,
        listener_generation,
        listener_generation_rx,
        authority_release_in_progress,
    } = install;

    let runtime = configure_download_behavior(browser.clone(), is_external, download_dir).await;
    projection_state
        .write()
        .await
        .set_runtime(listener_generation, runtime.clone());
    if callbacks.is_empty() {
        return projection_state.read().await.projection();
    }

    let listener_status = spawn_download_event_listeners(
        browser,
        projection_state.clone(),
        callbacks.clone(),
        listener_generation,
        listener_generation_rx,
        authority_release_in_progress,
    )
    .await;
    let reconciled =
        reconcile_download_runtime_with_listener_status(runtime, &callbacks, listener_status);
    projection_state
        .write()
        .await
        .set_runtime(listener_generation, reconciled.clone());
    projection_state.read().await.projection()
}

pub fn publish_download_runtime(
    callbacks: &DownloadCallbacks,
    generation: ListenerGeneration,
    runtime: DownloadRuntimeInfo,
) {
    if let Some(callback) = callbacks.on_runtime.clone() {
        callback(DownloadRuntimeUpdate {
            generation,
            runtime,
        });
    }
}

async fn record_download_started_if_not_releasing(
    projection_state: &SharedDownloadRuntimeProjection,
    listener_generation: ListenerGeneration,
    guid: String,
    url: String,
    suggested_filename: String,
    frame_id: Option<String>,
    authority_release_in_progress: &AtomicBool,
) -> bool {
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }

    let mut projection_state = projection_state.write().await;
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }
    projection_state.record_started(listener_generation, guid, url, suggested_filename, frame_id);
    true
}

struct DownloadProgressRecord {
    guid: String,
    state: DownloadState,
    received_bytes: u64,
    total_bytes: Option<u64>,
    final_path: Option<String>,
}

async fn record_download_progress_if_not_releasing(
    projection_state: &SharedDownloadRuntimeProjection,
    listener_generation: ListenerGeneration,
    progress: DownloadProgressRecord,
    authority_release_in_progress: &AtomicBool,
) -> bool {
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }

    let mut projection_state = projection_state.write().await;
    if authority_release_in_progress.load(Ordering::SeqCst) {
        return false;
    }
    projection_state.record_progress(
        listener_generation,
        progress.guid,
        progress.state,
        progress.received_bytes,
        progress.total_bytes,
        progress.final_path,
    );
    true
}

pub async fn cancel_download(browser: &Arc<Browser>, guid: &str) -> Result<(), RubError> {
    browser
        .execute(CancelDownloadParams::new(guid.to_string()))
        .await
        .map_err(|error| {
            RubError::domain(
                ErrorCode::InvalidInput,
                format!("Failed to cancel download '{guid}': {error}"),
            )
        })?;
    Ok(())
}

async fn configure_download_behavior(
    browser: Arc<Browser>,
    is_external: bool,
    download_dir: Option<PathBuf>,
) -> DownloadRuntimeInfo {
    let (mode, params, runtime_dir) = if is_external {
        (
            DownloadMode::ObserveOnly,
            SetDownloadBehaviorParams::builder()
                .behavior(SetDownloadBehaviorBehavior::Default)
                .events_enabled(true)
                .build(),
            None,
        )
    } else if let Some(path) = download_dir {
        let runtime_dir = path.display().to_string();
        if let Err(error) = std::fs::create_dir_all(&path) {
            return DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Degraded,
                mode: DownloadMode::Managed,
                download_dir: Some(runtime_dir),
                degraded_reason: Some(format!("download_dir_create_failed:{error}")),
                ..DownloadRuntimeInfo::default()
            };
        }
        (
            DownloadMode::Managed,
            SetDownloadBehaviorParams::builder()
                .behavior(SetDownloadBehaviorBehavior::AllowAndName)
                .download_path(runtime_dir.clone())
                .events_enabled(true)
                .build(),
            Some(runtime_dir),
        )
    } else {
        return DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Degraded,
            mode: DownloadMode::Managed,
            download_dir: None,
            degraded_reason: Some("managed_download_dir_missing".to_string()),
            ..DownloadRuntimeInfo::default()
        };
    };

    let params = match params {
        Ok(params) => params,
        Err(error) => {
            return DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Degraded,
                mode,
                download_dir: runtime_dir,
                degraded_reason: Some(format!("download_behavior_build_failed:{error}")),
                ..DownloadRuntimeInfo::default()
            };
        }
    };

    match browser.execute(params).await {
        Ok(_) => DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode,
            download_dir: runtime_dir,
            ..DownloadRuntimeInfo::default()
        },
        Err(error) => DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Degraded,
            mode,
            download_dir: runtime_dir,
            degraded_reason: Some(format!("download_behavior_failed:{error}")),
            ..DownloadRuntimeInfo::default()
        },
    }
}

async fn spawn_download_event_listeners(
    browser: Arc<Browser>,
    projection_state: SharedDownloadRuntimeProjection,
    callbacks: DownloadCallbacks,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
    authority_release_in_progress: Arc<AtomicBool>,
) -> DownloadListenerStatus {
    let mut status = DownloadListenerStatus::default();
    if let Some(callback) = callbacks.on_started.clone()
        && let Ok(mut listener) = browser.event_listener::<EventDownloadWillBegin>().await
    {
        status.started_listener = true;
        let generation_rx = listener_generation_rx.clone();
        let projection_state = projection_state.clone();
        let authority_release_in_progress = authority_release_in_progress.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                if authority_release_in_progress.load(Ordering::SeqCst) {
                    continue;
                }
                if !record_download_started_if_not_releasing(
                    &projection_state,
                    listener_generation,
                    event.guid.clone(),
                    event.url.clone(),
                    event.suggested_filename.clone(),
                    Some(event.frame_id.as_ref().to_string()),
                    &authority_release_in_progress,
                )
                .await
                {
                    continue;
                }
                if authority_release_in_progress.load(Ordering::SeqCst) {
                    continue;
                }
                callback(BrowserDownloadStart {
                    generation: listener_generation,
                    guid: event.guid.clone(),
                    url: event.url.clone(),
                    suggested_filename: event.suggested_filename.clone(),
                    frame_id: Some(event.frame_id.as_ref().to_string()),
                });
            }
        });
    }

    if let Some(callback) = callbacks.on_progress.clone()
        && let Ok(mut listener) = browser.event_listener::<EventDownloadProgress>().await
    {
        status.progress_listener = true;
        let generation_rx = listener_generation_rx.clone();
        let projection_state = projection_state.clone();
        let authority_release_in_progress = authority_release_in_progress.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                if authority_release_in_progress.load(Ordering::SeqCst) {
                    continue;
                }
                let state = normalize_download_state(&event.state);
                let received_bytes = normalize_bytes(event.received_bytes);
                let total_bytes = normalize_optional_bytes(event.total_bytes);
                if !record_download_progress_if_not_releasing(
                    &projection_state,
                    listener_generation,
                    DownloadProgressRecord {
                        guid: event.guid.clone(),
                        state,
                        received_bytes,
                        total_bytes,
                        final_path: event.file_path.clone(),
                    },
                    &authority_release_in_progress,
                )
                .await
                {
                    continue;
                }
                if authority_release_in_progress.load(Ordering::SeqCst) {
                    continue;
                }
                callback(BrowserDownloadProgress {
                    generation: listener_generation,
                    guid: event.guid.clone(),
                    state,
                    received_bytes,
                    total_bytes,
                    final_path: event.file_path.clone(),
                });
            }
        });
    }

    if callbacks.on_started.is_none() && callbacks.on_progress.is_none() {
        warn!(
            "Download runtime installed without event listeners; runtime projection may stay empty"
        );
    }

    status
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DownloadListenerStatus {
    started_listener: bool,
    progress_listener: bool,
}

fn reconcile_download_runtime_with_listener_status(
    mut runtime: DownloadRuntimeInfo,
    callbacks: &DownloadCallbacks,
    listener_status: DownloadListenerStatus,
) -> DownloadRuntimeInfo {
    if runtime.status != DownloadRuntimeStatus::Active {
        return runtime;
    }

    let missing_started = callbacks.on_started.is_some() && !listener_status.started_listener;
    let missing_progress = callbacks.on_progress.is_some() && !listener_status.progress_listener;
    if !(missing_started || missing_progress) {
        return runtime;
    }

    runtime.status = DownloadRuntimeStatus::Degraded;
    runtime.degraded_reason = Some(
        if missing_started && missing_progress {
            "download_event_listener_install_failed"
        } else if missing_started {
            "download_start_listener_install_failed"
        } else {
            "download_progress_listener_install_failed"
        }
        .to_string(),
    );
    runtime
}

fn normalize_download_state(state: &DownloadProgressState) -> DownloadState {
    match state {
        DownloadProgressState::InProgress => DownloadState::InProgress,
        DownloadProgressState::Completed => DownloadState::Completed,
        DownloadProgressState::Canceled => DownloadState::Canceled,
    }
}

fn normalize_optional_bytes(value: f64) -> Option<u64> {
    if value.is_finite() && value >= 0.0 {
        Some(value as u64)
    } else {
        None
    }
}

fn normalize_bytes(value: f64) -> u64 {
    normalize_optional_bytes(value).unwrap_or(0)
}

fn rfc3339_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::new())
}

#[cfg(test)]
mod tests {
    use super::{
        DownloadCallbacks, DownloadListenerStatus, DownloadProgressRecord,
        DownloadRuntimeProjectionState, new_shared_download_runtime_projection,
        normalize_download_state, normalize_optional_bytes,
        reconcile_download_runtime_with_listener_status, record_download_progress_if_not_releasing,
        record_download_started_if_not_releasing,
    };
    use chromiumoxide::cdp::browser_protocol::browser::DownloadProgressState;
    use rub_core::model::{
        DownloadMode, DownloadRuntimeInfo, DownloadRuntimeStatus, DownloadState,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn normalize_progress_state_matches_runtime_state() {
        assert_eq!(
            normalize_download_state(&DownloadProgressState::InProgress),
            DownloadState::InProgress
        );
        assert_eq!(
            normalize_download_state(&DownloadProgressState::Completed),
            DownloadState::Completed
        );
        assert_eq!(
            normalize_download_state(&DownloadProgressState::Canceled),
            DownloadState::Canceled
        );
    }

    #[test]
    fn normalize_optional_bytes_discards_invalid_values() {
        assert_eq!(normalize_optional_bytes(128.0), Some(128));
        assert_eq!(normalize_optional_bytes(-1.0), None);
        assert_eq!(normalize_optional_bytes(f64::NAN), None);
    }

    #[test]
    fn empty_callbacks_report_empty() {
        assert!(DownloadCallbacks::default().is_empty());
    }

    #[test]
    fn active_runtime_degrades_when_requested_download_listener_is_missing() {
        let runtime = DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/downloads".to_string()),
            ..DownloadRuntimeInfo::default()
        };
        let callbacks = DownloadCallbacks {
            on_started: Some(std::sync::Arc::new(|_| {})),
            on_progress: None,
            on_runtime: None,
        };

        let reconciled = reconcile_download_runtime_with_listener_status(
            runtime,
            &callbacks,
            DownloadListenerStatus::default(),
        );
        assert_eq!(reconciled.status, DownloadRuntimeStatus::Degraded);
        assert_eq!(
            reconciled.degraded_reason.as_deref(),
            Some("download_start_listener_install_failed")
        );
    }

    #[test]
    fn active_runtime_stays_active_when_listener_requirements_are_met() {
        let runtime = DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::ObserveOnly,
            ..DownloadRuntimeInfo::default()
        };
        let callbacks = DownloadCallbacks {
            on_started: Some(std::sync::Arc::new(|_| {})),
            on_progress: Some(std::sync::Arc::new(|_| {})),
            on_runtime: None,
        };

        let reconciled = reconcile_download_runtime_with_listener_status(
            runtime.clone(),
            &callbacks,
            DownloadListenerStatus {
                started_listener: true,
                progress_listener: true,
            },
        );
        assert_eq!(reconciled, runtime);
    }

    #[test]
    fn configured_runtime_remains_authoritative_without_download_callbacks() {
        let runtime = DownloadRuntimeInfo {
            status: DownloadRuntimeStatus::Active,
            mode: DownloadMode::Managed,
            download_dir: Some("/tmp/downloads".to_string()),
            ..DownloadRuntimeInfo::default()
        };

        let reconciled = reconcile_download_runtime_with_listener_status(
            runtime.clone(),
            &DownloadCallbacks::default(),
            DownloadListenerStatus::default(),
        );
        assert_eq!(reconciled, runtime);
    }

    #[test]
    fn projection_state_tracks_started_and_completed_downloads() {
        let mut state = DownloadRuntimeProjectionState::default();
        state.set_runtime(
            7,
            DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                download_dir: Some("/tmp/downloads".to_string()),
                ..DownloadRuntimeInfo::default()
            },
        );
        state.record_started(
            7,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            Some("frame-main".to_string()),
        );
        state.record_progress(
            7,
            "guid-1".to_string(),
            DownloadState::Completed,
            128,
            Some(128),
            Some("/tmp/downloads/guid-1".to_string()),
        );

        let (_, projection) = state.projection_with_generation();
        assert!(projection.active_downloads.is_empty());
        assert_eq!(projection.completed_downloads.len(), 1);
        assert_eq!(
            projection
                .last_download
                .as_ref()
                .map(|entry| entry.guid.as_str()),
            Some("guid-1")
        );
    }

    #[test]
    fn projection_state_resets_entries_on_new_generation() {
        let mut state = DownloadRuntimeProjectionState::default();
        state.set_runtime(
            7,
            DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                download_dir: Some("/tmp/downloads".to_string()),
                ..DownloadRuntimeInfo::default()
            },
        );
        state.record_started(
            7,
            "guid-1".to_string(),
            "https://example.test/report.csv".to_string(),
            "report.csv".to_string(),
            None,
        );

        state.set_runtime(
            8,
            DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::ObserveOnly,
                ..DownloadRuntimeInfo::default()
            },
        );

        let (generation, projection) = state.projection_with_generation();
        assert_eq!(generation, 8);
        assert!(projection.active_downloads.is_empty());
        assert!(projection.completed_downloads.is_empty());
        assert!(projection.last_download.is_none());
        assert_eq!(projection.mode, DownloadMode::ObserveOnly);
    }

    #[tokio::test]
    async fn started_event_rechecks_release_fence_after_waiting_for_write_authority() {
        let projection_state = new_shared_download_runtime_projection();
        projection_state.write().await.set_runtime(
            7,
            DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                ..DownloadRuntimeInfo::default()
            },
        );
        let write_guard = projection_state.write().await;
        let release_in_progress = Arc::new(AtomicBool::new(false));
        let state_for_task = projection_state.clone();
        let release_for_task = release_in_progress.clone();
        let record_task = tokio::spawn(async move {
            record_download_started_if_not_releasing(
                &state_for_task,
                7,
                "guid-1".to_string(),
                "https://example.test/report.csv".to_string(),
                "report.csv".to_string(),
                Some("frame-main".to_string()),
                &release_for_task,
            )
            .await
        });

        tokio::task::yield_now().await;
        release_in_progress.store(true, Ordering::SeqCst);
        drop(write_guard);

        assert!(
            !record_task.await.expect("record task should finish"),
            "release fence must reject started event after waiting for write authority"
        );
        let projection = projection_state.read().await.projection();
        assert!(projection.last_download.is_none());
        assert!(projection.active_downloads.is_empty());
    }

    #[tokio::test]
    async fn progress_event_rechecks_release_fence_after_waiting_for_write_authority() {
        let projection_state = new_shared_download_runtime_projection();
        projection_state.write().await.set_runtime(
            7,
            DownloadRuntimeInfo {
                status: DownloadRuntimeStatus::Active,
                mode: DownloadMode::Managed,
                ..DownloadRuntimeInfo::default()
            },
        );
        let write_guard = projection_state.write().await;
        let release_in_progress = Arc::new(AtomicBool::new(false));
        let state_for_task = projection_state.clone();
        let release_for_task = release_in_progress.clone();
        let record_task = tokio::spawn(async move {
            record_download_progress_if_not_releasing(
                &state_for_task,
                7,
                DownloadProgressRecord {
                    guid: "guid-1".to_string(),
                    state: DownloadState::Completed,
                    received_bytes: 128,
                    total_bytes: Some(128),
                    final_path: Some("/tmp/downloads/guid-1".to_string()),
                },
                &release_for_task,
            )
            .await
        });

        tokio::task::yield_now().await;
        release_in_progress.store(true, Ordering::SeqCst);
        drop(write_guard);

        assert!(
            !record_task.await.expect("record task should finish"),
            "release fence must reject progress event after waiting for write authority"
        );
        let projection = projection_state.read().await.projection();
        assert!(projection.last_download.is_none());
        assert!(projection.completed_downloads.is_empty());
    }
}
