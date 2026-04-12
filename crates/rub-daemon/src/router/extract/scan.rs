use std::sync::Arc;

use tokio::time::{Duration, sleep};

use super::super::TransactionDeadline;
use super::super::snapshot::build_stable_snapshot;
use super::collection::{ExtractCollectionSpec, extract_collection};
use crate::router::DaemonRouter;
use crate::session::SessionState;
use rub_core::error::{ErrorCode, RubError};

#[derive(Debug, Clone)]
pub(super) struct ExtractScanConfig {
    pub(super) until_count: u32,
    pub(super) dedupe_key: Option<String>,
    pub(super) max_scrolls: u32,
    pub(super) scroll_amount: u32,
    pub(super) settle_ms: u64,
    pub(super) stall_limit: u32,
}

#[derive(Debug)]
pub(super) struct ExtractScanOutcome {
    pub(super) rows: Vec<serde_json::Value>,
    pub(super) returned_count: usize,
    pub(super) unique_count: usize,
    pub(super) pass_count: u32,
    pub(super) scroll_count: u32,
    pub(super) complete: bool,
    pub(super) stop_reason: &'static str,
}

#[derive(Debug)]
pub(super) struct ExtractListWaitOutcome {
    pub(super) rows: Vec<serde_json::Value>,
    pub(super) matched_item: serde_json::Value,
    pub(super) item_count: usize,
    pub(super) elapsed_ms: u64,
}

pub(super) async fn scan_collection(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    collection_name: &str,
    collection: &ExtractCollectionSpec,
    scan: &ExtractScanConfig,
) -> Result<ExtractScanOutcome, RubError> {
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut pass_count = 0u32;
    let mut scroll_count = 0u32;
    let mut no_growth_passes = 0u32;
    let mut bottom_hint = false;
    let (complete, stop_reason) = loop {
        pass_count = pass_count.saturating_add(1);
        let snapshot =
            build_stable_snapshot(router, args, state, deadline, Some(0), false, false).await?;
        let snapshot = state.cache_snapshot(snapshot).await;
        let batch_value =
            extract_collection(router, &snapshot, collection_name, collection).await?;
        let batch_rows = batch_value.as_array().cloned().ok_or_else(|| {
            RubError::Internal("collection scan expected array payload".to_string())
        })?;

        let mut new_rows = 0usize;
        for (row_index, row) in batch_rows.into_iter().enumerate() {
            let fingerprint = row_fingerprint(&row, scan.dedupe_key.as_deref(), row_index)?;
            if seen.insert(fingerprint) {
                rows.push(row);
                new_rows += 1;
            }
        }

        if rows.len() >= scan.until_count as usize {
            rows.truncate(scan.until_count as usize);
            break (true, "target_reached");
        }

        if new_rows == 0 {
            no_growth_passes = no_growth_passes.saturating_add(1);
        } else {
            no_growth_passes = 0;
            bottom_hint = false;
        }

        if bottom_hint && new_rows == 0 {
            break (false, "at_bottom");
        }
        if no_growth_passes >= scan.stall_limit {
            break (false, "stalled");
        }
        if scroll_count >= scan.max_scrolls {
            break (false, "max_scrolls_reached");
        }

        let position = router
            .browser
            .scroll(
                rub_core::model::ScrollDirection::Down,
                Some(scan.scroll_amount),
            )
            .await?;
        scroll_count = scroll_count.saturating_add(1);
        bottom_hint = position.at_bottom;
        sleep(Duration::from_millis(scan.settle_ms)).await;
    };

    Ok(ExtractScanOutcome {
        returned_count: rows.len(),
        unique_count: seen.len(),
        rows,
        pass_count,
        scroll_count,
        complete,
        stop_reason,
    })
}

pub(super) async fn wait_for_collection_match(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    deadline: TransactionDeadline,
    collection_name: &str,
    collection: &ExtractCollectionSpec,
    wait: &super::spec::ExtractListWaitConfig,
) -> Result<ExtractListWaitOutcome, RubError> {
    const POLL_INTERVAL_MS: u64 = 250;

    let started = std::time::Instant::now();
    let timeout = wait.timeout;

    loop {
        let snapshot =
            build_stable_snapshot(router, args, state, deadline, Some(0), false, false).await?;
        let snapshot = state.cache_snapshot(snapshot).await;
        let batch_value =
            extract_collection(router, &snapshot, collection_name, collection).await?;
        let rows = batch_value.as_array().cloned().ok_or_else(|| {
            RubError::Internal("collection wait expected array payload".to_string())
        })?;

        if let Some(matched_item) = rows
            .iter()
            .find(|row| collection_row_matches_wait_probe(row, &wait.field_path, &wait.contains))
            .cloned()
        {
            return Ok(ExtractListWaitOutcome {
                item_count: rows.len(),
                rows,
                matched_item,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }

        if started.elapsed() >= timeout {
            return Err(collection_wait_timeout_error(
                collection_name,
                &wait.field_path,
                &wait.contains,
                started.elapsed(),
                rows.len(),
            ));
        }

        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

fn row_fingerprint(
    row: &serde_json::Value,
    dedupe_key: Option<&str>,
    row_index: usize,
) -> Result<String, RubError> {
    if let Some(path) = dedupe_key {
        let value = lookup_json_path(row, path).ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("scan_key '{path}' was missing from extracted row {row_index}"),
                serde_json::json!({
                    "scan_key": path,
                    "row_index": row_index,
                    "row": row,
                }),
            )
        })?;
        let fingerprint = match value {
            serde_json::Value::String(text) => text.clone(),
            other => serde_json::to_string(other).map_err(RubError::from)?,
        };
        if fingerprint.trim().is_empty() {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("scan_key '{path}' resolved to an empty value in row {row_index}"),
                serde_json::json!({
                    "scan_key": path,
                    "row_index": row_index,
                    "row": row,
                }),
            ));
        }
        return Ok(fingerprint);
    }

    serde_json::to_string(row).map_err(RubError::from)
}

pub(super) fn lookup_json_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

fn collection_row_matches_wait_probe(
    row: &serde_json::Value,
    field_path: &str,
    contains: &str,
) -> bool {
    let Some(value) = lookup_json_path(row, field_path) else {
        return false;
    };
    let haystack = normalize_wait_text(match value {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    });
    let needle = normalize_wait_text(contains.to_string());
    !needle.is_empty() && haystack.contains(&needle)
}

fn normalize_wait_text(value: String) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn collection_wait_timeout_error(
    collection_name: &str,
    field_path: &str,
    contains: &str,
    elapsed: Duration,
    item_count: usize,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::WaitTimeout,
        format!(
            "List wait timed out before any '{collection_name}' row matched {field_path} contains '{contains}'"
        ),
        serde_json::json!({
            "kind": "collection_extract",
            "collection": collection_name,
            "field_path": field_path,
            "contains": contains,
            "elapsed_ms": elapsed.as_millis() as u64,
            "item_count": item_count,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{collection_row_matches_wait_probe, lookup_json_path};
    use serde_json::json;

    #[test]
    fn lookup_json_path_supports_nested_row_fields() {
        let row = json!({
            "subject": {
                "text": "Confirm your new account"
            }
        });

        assert_eq!(
            lookup_json_path(&row, "subject.text"),
            Some(&json!("Confirm your new account"))
        );
        assert_eq!(lookup_json_path(&row, "subject.missing"), None);
    }

    #[test]
    fn collection_wait_probe_matches_case_insensitive_normalized_contains() {
        let row = json!({
            "subject": "  Confirm   your NEW account  ",
            "from": "Discourse Demo"
        });

        assert!(collection_row_matches_wait_probe(
            &row,
            "subject",
            "confirm your new account"
        ));
        assert!(!collection_row_matches_wait_probe(
            &row,
            "from",
            "activation"
        ));
    }
}
