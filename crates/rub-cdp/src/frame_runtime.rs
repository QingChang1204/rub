use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    FrameContextInfo, FrameContextStatus, FrameInventoryEntry, FrameRuntimeInfo,
};

const SAME_ORIGIN_ACCESS_JS: &str = r#"
(() => {
    try {
        let current = window;
        while (current !== current.top) {
            void current.parent.document;
            current = current.parent;
        }
        return true;
    } catch (_) {
        return false;
    }
})()
"#;

thread_local! {
    static FRAME_INVENTORY_COLLECTIONS: Cell<u64> = const { Cell::new(0) };
    static FRAME_INVENTORY_ENTRY_STEPS: Cell<u64> = const { Cell::new(0) };
    static FRAME_SAME_ORIGIN_PROBE_REQUESTS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameRuntimeMetrics {
    pub(crate) primary_fast_path_hits: u64,
    pub(crate) inventory_collections: u64,
    pub(crate) inventory_entry_steps: u64,
    pub(crate) same_origin_probe_requests: u64,
}

thread_local! {
    static FRAME_PRIMARY_FAST_PATH_HITS: Cell<u64> = const { Cell::new(0) };
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedFrameContext {
    pub frame: FrameContextInfo,
    pub lineage: Vec<String>,
    pub execution_context_id: Option<ExecutionContextId>,
}

pub async fn capture_frame_runtime(page: &Arc<Page>) -> Result<FrameRuntimeInfo, RubError> {
    let inventory = collect_frame_inventory(page).await?;
    let primary = inventory
        .iter()
        .find(|entry| entry.is_primary)
        .or_else(|| inventory.first())
        .ok_or_else(|| RubError::Internal("Frame inventory is empty".to_string()))?;

    Ok(FrameRuntimeInfo {
        status: FrameContextStatus::Top,
        current_frame: Some(primary.frame.clone()),
        primary_frame: Some(primary.frame.clone()),
        frame_lineage: build_lineage(primary.frame.frame_id.as_str(), &inventory),
        degraded_reason: None,
    })
}

pub async fn list_frame_inventory(page: &Arc<Page>) -> Result<Vec<FrameInventoryEntry>, RubError> {
    collect_frame_inventory(page).await
}

pub(crate) async fn resolve_frame_context(
    page: &Arc<Page>,
    frame_id: Option<&str>,
) -> Result<ResolvedFrameContext, RubError> {
    if frame_id.is_none() {
        FRAME_PRIMARY_FAST_PATH_HITS.with(|count| count.set(count.get().saturating_add(1)));
        return resolve_primary_frame_context(page).await;
    }

    let inventory = collect_frame_inventory(page).await?;
    let frame_id = frame_id.expect("non-primary path always has explicit frame_id");
    let selected = inventory
        .iter()
        .find(|entry| entry.frame.frame_id == frame_id)
        .ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("Frame '{frame_id}' is not present in the current frame inventory"),
                serde_json::json!({
                    "frame_id": frame_id,
                }),
            )
        })?;

    if !selected.is_primary && !matches!(selected.frame.same_origin_accessible, Some(true)) {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Frame '{frame_id}' is not same-origin accessible for frame-scoped snapshot"),
            serde_json::json!({
                "frame_id": frame_id,
                "same_origin_accessible": selected.frame.same_origin_accessible,
            }),
        ));
    }

    let execution_context_id = if selected.is_primary {
        None
    } else {
        Some(
            page.frame_execution_context(FrameId::new(frame_id.to_string()))
                .await
                .map_err(|error| {
                    RubError::Internal(format!("Resolve frame execution context failed: {error}"))
                })?
                .ok_or_else(|| {
                    RubError::domain_with_context(
                        ErrorCode::InvalidInput,
                        format!("Frame '{frame_id}' has no live execution context"),
                        serde_json::json!({
                            "frame_id": frame_id,
                        }),
                    )
                })?,
        )
    };

    Ok(ResolvedFrameContext {
        frame: selected.frame.clone(),
        lineage: build_lineage(frame_id, &inventory),
        execution_context_id,
    })
}

async fn resolve_primary_frame_context(page: &Arc<Page>) -> Result<ResolvedFrameContext, RubError> {
    let main_frame = page
        .mainframe()
        .await
        .map_err(|error| RubError::Internal(format!("Read main frame failed: {error}")))?
        .ok_or_else(|| RubError::Internal("Main frame is unavailable".to_string()))?;
    let main_frame_key = main_frame.as_ref().to_string();

    Ok(ResolvedFrameContext {
        frame: FrameContextInfo {
            frame_id: main_frame_key.clone(),
            name: page
                .frame_name(main_frame.clone())
                .await
                .map_err(|error| RubError::Internal(format!("Read frame name failed: {error}")))?,
            parent_frame_id: None,
            target_id: Some(page.target_id().as_ref().to_string()),
            url: page
                .frame_url(main_frame)
                .await
                .map_err(|error| RubError::Internal(format!("Read frame URL failed: {error}")))?,
            depth: 0,
            same_origin_accessible: Some(true),
        },
        lineage: vec![main_frame_key],
        execution_context_id: None,
    })
}

async fn collect_frame_inventory(page: &Arc<Page>) -> Result<Vec<FrameInventoryEntry>, RubError> {
    FRAME_INVENTORY_COLLECTIONS.with(|count| count.set(count.get().saturating_add(1)));
    let main_frame = page
        .mainframe()
        .await
        .map_err(|error| RubError::Internal(format!("Read main frame failed: {error}")))?
        .ok_or_else(|| RubError::Internal("Main frame is unavailable".to_string()))?;
    let frames = page
        .frames()
        .await
        .map_err(|error| RubError::Internal(format!("Read frame inventory failed: {error}")))?;

    let main_frame_key = main_frame.as_ref().to_string();
    let ordered_frames = build_ordered_frame_inventory(main_frame.clone(), frames);
    FRAME_INVENTORY_ENTRY_STEPS.with(|count| {
        count.set(
            count
                .get()
                .saturating_add(ordered_frames.len().try_into().unwrap_or(u64::MAX)),
        )
    });

    let mut raw_entries = Vec::with_capacity(ordered_frames.len());
    let mut seen_frame_ids = HashSet::new();
    for frame_id in ordered_frames {
        let frame_key = frame_id.as_ref().to_string();
        if !seen_frame_ids.insert(frame_key.clone()) {
            continue;
        }

        let parent_frame_id = page
            .frame_parent(frame_id.clone())
            .await
            .map_err(|error| RubError::Internal(format!("Read frame parent failed: {error}")))?
            .map(|parent| parent.as_ref().to_string());
        let needs_same_origin_probe = same_origin_probe_required(&frame_key, &main_frame_key);

        raw_entries.push(FrameContextInfo {
            frame_id: frame_key,
            name: page
                .frame_name(frame_id.clone())
                .await
                .map_err(|error| RubError::Internal(format!("Read frame name failed: {error}")))?,
            parent_frame_id,
            target_id: None,
            url: page
                .frame_url(frame_id.clone())
                .await
                .map_err(|error| RubError::Internal(format!("Read frame URL failed: {error}")))?,
            depth: 0,
            same_origin_accessible: if needs_same_origin_probe {
                FRAME_SAME_ORIGIN_PROBE_REQUESTS
                    .with(|count| count.set(count.get().saturating_add(1)));
                probe_same_origin_accessibility(page, &frame_id).await?
            } else {
                Some(true)
            },
        });
    }

    let parent_by_frame = raw_entries
        .iter()
        .map(|entry| (entry.frame_id.clone(), entry.parent_frame_id.clone()))
        .collect::<HashMap<_, _>>();
    let mut depth_cache = HashMap::new();
    let mut frames = raw_entries
        .into_iter()
        .map(|mut frame| {
            let depth = frame_depth(
                &frame.frame_id,
                &main_frame_key,
                &parent_by_frame,
                &mut depth_cache,
            );
            frame.depth = depth;
            if depth == 0 {
                frame.target_id = Some(page.target_id().as_ref().to_string());
                frame.same_origin_accessible = Some(true);
            }
            frame
        })
        .collect::<Vec<_>>();
    frames.sort_by_key(|frame| {
        (
            frame.depth,
            frame.frame_id != main_frame_key,
            frame.frame_id.clone(),
        )
    });

    Ok(frames
        .into_iter()
        .enumerate()
        .map(|(index, frame)| {
            let is_primary = frame.frame_id == main_frame_key;
            FrameInventoryEntry {
                index: index as u32,
                is_current: is_primary,
                is_primary,
                frame,
            }
        })
        .collect())
}

async fn probe_same_origin_accessibility(
    page: &Arc<Page>,
    frame_id: &FrameId,
) -> Result<Option<bool>, RubError> {
    let Some(context_id) = page
        .frame_execution_context(frame_id.clone())
        .await
        .map_err(|error| {
            RubError::Internal(format!("Resolve frame execution context failed: {error}"))
        })?
    else {
        return Ok(None);
    };

    let params = EvaluateParams::builder()
        .expression(SAME_ORIGIN_ACCESS_JS)
        .await_promise(true)
        .return_by_value(true)
        .context_id(context_id)
        .build()
        .map_err(|error| RubError::Internal(format!("Build evaluate params failed: {error}")))?;
    let response = page
        .execute(params)
        .await
        .map_err(|error| RubError::Internal(format!("Same-origin probe failed: {error}")))?;
    Ok(response
        .result
        .result
        .value
        .and_then(|value| value.as_bool()))
}

fn build_ordered_frame_inventory(main_frame: FrameId, frames: Vec<FrameId>) -> Vec<FrameId> {
    let mut ordered_frames = Vec::with_capacity(frames.len().saturating_add(1));
    ordered_frames.push(main_frame.clone());
    for frame_id in frames {
        if frame_id != main_frame {
            ordered_frames.push(frame_id);
        }
    }
    ordered_frames
}

fn same_origin_probe_required(frame_id: &str, main_frame_id: &str) -> bool {
    frame_id != main_frame_id
}

#[cfg(test)]
fn frame_runtime_metrics_snapshot() -> FrameRuntimeMetrics {
    FrameRuntimeMetrics {
        primary_fast_path_hits: FRAME_PRIMARY_FAST_PATH_HITS.with(Cell::get),
        inventory_collections: FRAME_INVENTORY_COLLECTIONS.with(Cell::get),
        inventory_entry_steps: FRAME_INVENTORY_ENTRY_STEPS.with(Cell::get),
        same_origin_probe_requests: FRAME_SAME_ORIGIN_PROBE_REQUESTS.with(Cell::get),
    }
}

#[cfg(test)]
fn reset_frame_runtime_metrics() {
    FRAME_PRIMARY_FAST_PATH_HITS.with(|count| count.set(0));
    FRAME_INVENTORY_COLLECTIONS.with(|count| count.set(0));
    FRAME_INVENTORY_ENTRY_STEPS.with(|count| count.set(0));
    FRAME_SAME_ORIGIN_PROBE_REQUESTS.with(|count| count.set(0));
}

fn build_lineage(current_frame_id: &str, inventory: &[FrameInventoryEntry]) -> Vec<String> {
    let parent_by_frame = inventory
        .iter()
        .map(|entry| {
            (
                entry.frame.frame_id.as_str(),
                entry.frame.parent_frame_id.as_deref(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut lineage = Vec::new();
    let mut current = Some(current_frame_id);
    while let Some(frame_id) = current {
        if lineage.iter().any(|existing| existing == frame_id) {
            break;
        }
        lineage.push(frame_id.to_string());
        current = parent_by_frame.get(frame_id).copied().flatten();
    }
    lineage
}

fn frame_depth(
    frame_id: &str,
    main_frame_id: &str,
    parent_by_frame: &HashMap<String, Option<String>>,
    depth_cache: &mut HashMap<String, u32>,
) -> u32 {
    if let Some(depth) = depth_cache.get(frame_id) {
        return *depth;
    }
    if frame_id == main_frame_id {
        depth_cache.insert(frame_id.to_string(), 0);
        return 0;
    }

    let mut seen = HashSet::new();
    let mut current = frame_id;
    let mut depth = 0;
    while let Some(parent) = parent_by_frame
        .get(current)
        .and_then(|value| value.as_deref())
    {
        if !seen.insert(current.to_string()) {
            break;
        }
        depth += 1;
        if parent == main_frame_id {
            break;
        }
        current = parent;
    }

    depth_cache.insert(frame_id.to_string(), depth);
    depth
}

#[cfg(test)]
mod tests {
    use super::{
        FrameRuntimeMetrics, build_lineage, build_ordered_frame_inventory, frame_depth,
        frame_runtime_metrics_snapshot, reset_frame_runtime_metrics, same_origin_probe_required,
    };
    use chromiumoxide::cdp::browser_protocol::page::FrameId;
    use rub_core::model::{FrameContextInfo, FrameInventoryEntry};
    use std::collections::HashMap;

    #[test]
    fn frame_depth_counts_parent_lineage_from_main_frame() {
        let parents = HashMap::from([
            ("main".to_string(), None),
            ("child".to_string(), Some("main".to_string())),
            ("grandchild".to_string(), Some("child".to_string())),
        ]);
        let mut cache = HashMap::new();

        assert_eq!(frame_depth("main", "main", &parents, &mut cache), 0);
        assert_eq!(frame_depth("child", "main", &parents, &mut cache), 1);
        assert_eq!(frame_depth("grandchild", "main", &parents, &mut cache), 2);
    }

    #[test]
    fn frame_depth_breaks_cycles_without_panicking() {
        let parents = HashMap::from([
            ("main".to_string(), None),
            ("loop-a".to_string(), Some("loop-b".to_string())),
            ("loop-b".to_string(), Some("loop-a".to_string())),
        ]);
        let mut cache = HashMap::new();

        let depth = frame_depth("loop-a", "main", &parents, &mut cache);
        assert!(depth >= 1);
    }

    #[test]
    fn build_lineage_collects_current_to_root_order() {
        let inventory = vec![
            FrameInventoryEntry {
                index: 0,
                frame: FrameContextInfo {
                    frame_id: "main".to_string(),
                    name: Some("main".to_string()),
                    parent_frame_id: None,
                    target_id: Some("target-1".to_string()),
                    url: Some("https://example.test".to_string()),
                    depth: 0,
                    same_origin_accessible: Some(true),
                },
                is_current: true,
                is_primary: true,
            },
            FrameInventoryEntry {
                index: 1,
                frame: FrameContextInfo {
                    frame_id: "child".to_string(),
                    name: Some("child".to_string()),
                    parent_frame_id: Some("main".to_string()),
                    target_id: None,
                    url: Some("https://example.test/frame".to_string()),
                    depth: 1,
                    same_origin_accessible: Some(true),
                },
                is_current: false,
                is_primary: false,
            },
        ];

        assert_eq!(
            build_lineage("child", &inventory),
            vec!["child".to_string(), "main".to_string()]
        );
    }

    #[test]
    fn ordered_frame_inventory_keeps_primary_first_and_skips_duplicate_main() {
        let ordered = build_ordered_frame_inventory(
            FrameId::new("main".to_string()),
            vec![
                FrameId::new("main".to_string()),
                FrameId::new("child".to_string()),
                FrameId::new("grandchild".to_string()),
            ],
        );

        let ids = ordered
            .into_iter()
            .map(|frame_id| frame_id.as_ref().to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["main", "child", "grandchild"]);
    }

    #[test]
    fn frame_runtime_metrics_capture_probe_plan_baseline() {
        reset_frame_runtime_metrics();
        let main_frame_id = "main".to_string();
        let ordered = build_ordered_frame_inventory(
            FrameId::new(main_frame_id.clone()),
            vec![
                FrameId::new("child".to_string()),
                FrameId::new("grandchild".to_string()),
            ],
        );
        super::FRAME_INVENTORY_COLLECTIONS.with(|count| count.set(1));
        super::FRAME_INVENTORY_ENTRY_STEPS
            .with(|count| count.set(ordered.len().try_into().unwrap_or(u64::MAX)));
        let probe_requests = ordered
            .iter()
            .filter(|frame_id| same_origin_probe_required(frame_id.as_ref(), &main_frame_id))
            .count() as u64;
        super::FRAME_SAME_ORIGIN_PROBE_REQUESTS.with(|count| count.set(probe_requests));

        assert_eq!(
            frame_runtime_metrics_snapshot(),
            FrameRuntimeMetrics {
                primary_fast_path_hits: 0,
                inventory_collections: 1,
                inventory_entry_steps: 3,
                same_origin_probe_requests: 2,
            }
        );
    }

    #[test]
    fn frame_runtime_metrics_snapshot_tracks_primary_fast_path_hits() {
        reset_frame_runtime_metrics();
        super::FRAME_PRIMARY_FAST_PATH_HITS.with(|count| count.set(2));

        assert_eq!(
            frame_runtime_metrics_snapshot(),
            FrameRuntimeMetrics {
                primary_fast_path_hits: 2,
                inventory_collections: 0,
                inventory_entry_steps: 0,
                same_origin_probe_requests: 0,
            }
        );
    }
}
