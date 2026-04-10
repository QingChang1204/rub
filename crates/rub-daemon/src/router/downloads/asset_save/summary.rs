use std::path::Path;

use rub_core::model::{BulkAssetSaveSummary, SavedAssetEntry, SavedAssetStatus};

use super::paths::bulk_output_dir_state;

pub(super) fn summarize_results(
    source_count: u32,
    attempted_count: u32,
    output_dir: &Path,
    results: &[SavedAssetEntry],
) -> BulkAssetSaveSummary {
    let mut saved_count = 0u32;
    let mut skipped_existing_count = 0u32;
    let mut failed_count = 0u32;
    let mut timed_out_count = 0u32;

    for result in results {
        match result.status {
            SavedAssetStatus::Saved => saved_count = saved_count.saturating_add(1),
            SavedAssetStatus::SkippedExisting => {
                skipped_existing_count = skipped_existing_count.saturating_add(1)
            }
            SavedAssetStatus::Failed => failed_count = failed_count.saturating_add(1),
            SavedAssetStatus::TimedOut => timed_out_count = timed_out_count.saturating_add(1),
        }
    }

    BulkAssetSaveSummary {
        complete: timed_out_count == 0,
        source_count,
        attempted_count,
        saved_count,
        skipped_existing_count,
        failed_count,
        timed_out_count,
        output_dir: output_dir.display().to_string(),
        output_dir_state: Some(bulk_output_dir_state()),
    }
}
