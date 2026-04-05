use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{
    CancelDownloadParams, DownloadProgressState, EventDownloadProgress, EventDownloadWillBegin,
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{DownloadMode, DownloadRuntimeInfo, DownloadRuntimeStatus, DownloadState};
use std::path::PathBuf;
use std::sync::Arc;
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

pub async fn install_browser_download_runtime(
    browser: Arc<Browser>,
    callbacks: DownloadCallbacks,
    is_external: bool,
    download_dir: Option<PathBuf>,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
) -> DownloadRuntimeInfo {
    let runtime = configure_download_behavior(browser.clone(), is_external, download_dir).await;
    if callbacks.is_empty() {
        return runtime;
    }

    let listener_status = spawn_download_event_listeners(
        browser,
        callbacks.clone(),
        listener_generation,
        listener_generation_rx,
    )
    .await;
    reconcile_download_runtime_with_listener_status(runtime, &callbacks, listener_status)
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
    callbacks: DownloadCallbacks,
    listener_generation: ListenerGeneration,
    listener_generation_rx: ListenerGenerationRx,
) -> DownloadListenerStatus {
    let mut status = DownloadListenerStatus::default();
    if let Some(callback) = callbacks.on_started.clone()
        && let Ok(mut listener) = browser.event_listener::<EventDownloadWillBegin>().await
    {
        status.started_listener = true;
        let generation_rx = listener_generation_rx.clone();
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
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
        tokio::spawn(async move {
            let mut generation_rx = generation_rx;
            while let Some(event) =
                next_listener_event(&mut listener, listener_generation, &mut generation_rx).await
            {
                callback(BrowserDownloadProgress {
                    generation: listener_generation,
                    guid: event.guid.clone(),
                    state: normalize_download_state(&event.state),
                    received_bytes: normalize_bytes(event.received_bytes),
                    total_bytes: normalize_optional_bytes(event.total_bytes),
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

#[cfg(test)]
mod tests {
    use super::{
        DownloadCallbacks, DownloadListenerStatus, normalize_download_state,
        normalize_optional_bytes, reconcile_download_runtime_with_listener_status,
    };
    use chromiumoxide::cdp::browser_protocol::browser::DownloadProgressState;
    use rub_core::model::{
        DownloadMode, DownloadRuntimeInfo, DownloadRuntimeStatus, DownloadState,
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
}
