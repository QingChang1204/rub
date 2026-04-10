use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::stream::StreamExt;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use rub_core::fs::{commit_temporary_file, commit_temporary_file_no_clobber};
use rub_core::model::{SavedAssetEntry, SavedAssetStatus};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::paths::{
    cleanup_tracked_temp_path, reserve_reconciled_output_path, saved_asset_output_path_state,
    set_tracked_output_path, set_tracked_temp_path, temporary_path, tracked_output_path,
};
use super::request::{AssetSource, PreparedAssetSource};

#[derive(Clone)]
pub(super) struct SaveExecutionContext {
    pub(super) client: reqwest::Client,
    pub(super) deadline: Instant,
    pub(super) overwrite: bool,
    pub(super) reserved_output_paths: Arc<Mutex<BTreeSet<PathBuf>>>,
}

pub(super) async fn save_one(
    context: SaveExecutionContext,
    source: PreparedAssetSource,
) -> SavedAssetEntry {
    let PreparedAssetSource { source, headers } = source;
    let source_index = source.index;
    let source_url = source.url.clone();
    let source_name = source.source_name.clone();
    let planned_output_path = source.output_path.clone();
    let timeout_source = AssetSource {
        index: source_index,
        url: source_url.clone(),
        source_name: source_name.clone(),
        output_path: planned_output_path.clone(),
    };
    let temp_path_slot = Arc::new(Mutex::new(None::<PathBuf>));
    let output_path_slot = Arc::new(Mutex::new(None::<PathBuf>));
    let Some(timeout) = context.deadline.checked_duration_since(Instant::now()) else {
        return timeout_entry(
            &timeout_source,
            None,
            "asset_request_timed_out_before_download_started",
        );
    };
    let future = async {
        let mut request = context.client.get(&source_url);
        if let Some(headers) = headers.clone() {
            request = request.headers(headers);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                return Err(error.to_string());
            }
        };
        if response.status() != StatusCode::OK {
            return Err(format!("http_status:{}", response.status()));
        }

        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let mut stream = response.bytes_stream();
        let first_chunk = stream
            .next()
            .await
            .transpose()
            .map_err(|error| error.to_string())?;
        let output_path = reserve_reconciled_output_path(
            &planned_output_path,
            content_type.as_ref(),
            first_chunk.as_deref(),
            context.reserved_output_paths.as_ref(),
        );
        set_tracked_output_path(&output_path_slot, Some(output_path.clone()));
        if !context.overwrite && output_path.exists() {
            return Ok(SaveOneOutcome::SkippedExisting { output_path });
        }
        let tmp_path = temporary_path(&output_path);
        set_tracked_temp_path(&temp_path_slot, Some(tmp_path.clone()));
        if let Some(parent) = tmp_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        let mut file = fs::File::create(&tmp_path)
            .await
            .map_err(|error| error.to_string())?;
        let mut bytes_written = 0u64;
        if let Some(chunk) = first_chunk {
            file.write_all(&chunk)
                .await
                .map_err(|error| error.to_string())?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| error.to_string())?;
            file.write_all(&chunk)
                .await
                .map_err(|error| error.to_string())?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }
        file.flush().await.map_err(|error| error.to_string())?;
        drop(file);
        Ok(SaveOneOutcome::Prepared {
            output_path,
            tmp_path,
            bytes_written,
        })
    };

    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(SaveOneOutcome::Prepared {
            output_path,
            tmp_path,
            bytes_written,
        })) => {
            if context
                .deadline
                .checked_duration_since(Instant::now())
                .is_none()
            {
                let _ = fs::remove_file(&tmp_path).await;
                set_tracked_temp_path(&temp_path_slot, None);
                return timeout_entry(
                    &timeout_source,
                    Some(output_path),
                    "asset_commit_deadline_exceeded_before_commit",
                );
            }
            match tokio::task::spawn_blocking({
                let tmp_path = tmp_path.clone();
                let output_path = output_path.clone();
                move || {
                    if context.overwrite {
                        commit_temporary_file(&tmp_path, &output_path)
                    } else {
                        commit_temporary_file_no_clobber(&tmp_path, &output_path)
                    }
                }
            })
            .await
            {
                Ok(Ok(commit_outcome)) => {
                    set_tracked_temp_path(&temp_path_slot, None);
                    SavedAssetEntry {
                        index: source_index,
                        url: source_url.clone(),
                        status: SavedAssetStatus::Saved,
                        output_path: output_path.display().to_string(),
                        output_path_state: Some(saved_asset_output_path_state(
                            SavedAssetStatus::Saved,
                            Some(commit_outcome.durability_confirmed()),
                        )),
                        source_name: source_name.clone(),
                        bytes_written: Some(bytes_written),
                        durability_confirmed: Some(commit_outcome.durability_confirmed())
                            .filter(|confirmed| !confirmed),
                        error: None,
                    }
                }
                Ok(Err(error)) => {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        let _ = fs::remove_file(&tmp_path).await;
                        set_tracked_temp_path(&temp_path_slot, None);
                        return SavedAssetEntry {
                            index: source_index,
                            url: source_url.clone(),
                            status: SavedAssetStatus::SkippedExisting,
                            output_path: output_path.display().to_string(),
                            output_path_state: Some(saved_asset_output_path_state(
                                SavedAssetStatus::SkippedExisting,
                                None,
                            )),
                            source_name: source_name.clone(),
                            bytes_written: None,
                            durability_confirmed: None,
                            error: None,
                        };
                    }
                    let _ = fs::remove_file(&tmp_path).await;
                    set_tracked_temp_path(&temp_path_slot, None);
                    SavedAssetEntry {
                        index: source_index,
                        url: source_url.clone(),
                        status: SavedAssetStatus::Failed,
                        output_path: output_path.display().to_string(),
                        output_path_state: Some(saved_asset_output_path_state(
                            SavedAssetStatus::Failed,
                            None,
                        )),
                        source_name: source_name.clone(),
                        bytes_written: None,
                        durability_confirmed: None,
                        error: Some(error.to_string()),
                    }
                }
                Err(join_error) => {
                    let _ = fs::remove_file(&tmp_path).await;
                    set_tracked_temp_path(&temp_path_slot, None);
                    SavedAssetEntry {
                        index: source_index,
                        url: source_url.clone(),
                        status: SavedAssetStatus::Failed,
                        output_path: output_path.display().to_string(),
                        output_path_state: Some(saved_asset_output_path_state(
                            SavedAssetStatus::Failed,
                            None,
                        )),
                        source_name: source_name.clone(),
                        bytes_written: None,
                        durability_confirmed: None,
                        error: Some(join_error.to_string()),
                    }
                }
            }
        }
        Ok(Ok(SaveOneOutcome::SkippedExisting { output_path })) => SavedAssetEntry {
            index: source_index,
            url: source_url.clone(),
            status: SavedAssetStatus::SkippedExisting,
            output_path: output_path.display().to_string(),
            output_path_state: Some(saved_asset_output_path_state(
                SavedAssetStatus::SkippedExisting,
                None,
            )),
            source_name: source_name.clone(),
            bytes_written: None,
            durability_confirmed: None,
            error: None,
        },
        Ok(Err(error)) => {
            cleanup_tracked_temp_path(&temp_path_slot).await;
            let output_path = tracked_output_path(&output_path_slot)
                .unwrap_or_else(|| planned_output_path.clone());
            SavedAssetEntry {
                index: source_index,
                url: source_url.clone(),
                status: SavedAssetStatus::Failed,
                output_path: output_path.display().to_string(),
                output_path_state: Some(saved_asset_output_path_state(
                    SavedAssetStatus::Failed,
                    None,
                )),
                source_name: source_name.clone(),
                bytes_written: None,
                durability_confirmed: None,
                error: Some(error),
            }
        }
        Err(_) => {
            cleanup_tracked_temp_path(&temp_path_slot).await;
            timeout_entry(
                &timeout_source,
                tracked_output_path(&output_path_slot),
                "asset_request_timed_out",
            )
        }
    }
}

enum SaveOneOutcome {
    Prepared {
        output_path: PathBuf,
        tmp_path: PathBuf,
        bytes_written: u64,
    },
    SkippedExisting {
        output_path: PathBuf,
    },
}

pub(super) fn timeout_entry(
    source: &AssetSource,
    output_path: Option<PathBuf>,
    reason: &str,
) -> SavedAssetEntry {
    SavedAssetEntry {
        index: source.index,
        url: source.url.clone(),
        status: SavedAssetStatus::TimedOut,
        output_path: output_path
            .unwrap_or_else(|| source.output_path.clone())
            .display()
            .to_string(),
        output_path_state: Some(saved_asset_output_path_state(
            SavedAssetStatus::TimedOut,
            None,
        )),
        source_name: source.source_name.clone(),
        bytes_written: None,
        durability_confirmed: None,
        error: Some(reason.to_string()),
    }
}
