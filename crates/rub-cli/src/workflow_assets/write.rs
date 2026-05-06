use std::path::PathBuf;
use std::time::Instant;

use rub_core::error::RubError;
use serde_json::Value;

use crate::local_asset_writes::{
    LocalAssetWriteConfig, PendingLocalAssetWrite, commit_local_asset_writes,
    commit_local_asset_writes_until, pending_local_asset_write,
};

use super::local_workflow_asset_path_state;

const WORKFLOW_ASSET_WRITE_CONFIG: LocalAssetWriteConfig = LocalAssetWriteConfig {
    persistence_authority: "cli.history_export_asset_persistence",
    duplicate_path_label: "workflow export path",
    write_path_label: "workflow asset",
    resolution_path_label: "workflow export path",
    duplicate_reason: "workflow_asset_duplicate_export_path",
    write_failed_reason: "workflow_asset_write_failed",
    unreadable_reason: "workflow_asset_unreadable_for_rollback",
    path_resolution_reason: "workflow_asset_path_resolution_failed",
    path_authority: "cli.workflow_assets.write.path",
    path_kind: "workflow_asset_reference",
    path_state: local_workflow_asset_path_state,
    include_duplicate_authority_path: true,
};

pub(super) type PendingAssetWrite = PendingLocalAssetWrite;

pub(super) fn commit_asset_writes(writes: Vec<PendingAssetWrite>) -> Result<Vec<Value>, RubError> {
    commit_local_asset_writes(writes, &WORKFLOW_ASSET_WRITE_CONFIG)
}

pub(super) fn commit_asset_writes_until(
    writes: Vec<PendingAssetWrite>,
    deadline: Instant,
    timeout_ms: u64,
    phase: &'static str,
) -> Result<Vec<Value>, RubError> {
    commit_local_asset_writes_until(
        writes,
        &WORKFLOW_ASSET_WRITE_CONFIG,
        deadline,
        timeout_ms,
        phase,
    )
}

pub(super) fn pending_asset_write(
    path: PathBuf,
    contents: Vec<u8>,
    artifact: Value,
) -> Result<PendingAssetWrite, RubError> {
    pending_local_asset_write(path, contents, artifact, &WORKFLOW_ASSET_WRITE_CONFIG)
}

#[cfg(test)]
pub(super) use crate::local_asset_writes::remove_newly_created_asset_if_matches;
