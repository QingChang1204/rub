use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
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
    let inventory = collect_frame_inventory(page).await?;
    let primary = inventory
        .iter()
        .find(|entry| entry.is_primary)
        .or_else(|| inventory.first())
        .ok_or_else(|| RubError::Internal("Frame inventory is empty".to_string()))?;

    if let Some(frame_id) = frame_id {
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
                format!(
                    "Frame '{frame_id}' is not same-origin accessible for frame-scoped snapshot"
                ),
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
                        RubError::Internal(format!(
                            "Resolve frame execution context failed: {error}"
                        ))
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

        return Ok(ResolvedFrameContext {
            frame: selected.frame.clone(),
            lineage: build_lineage(frame_id, &inventory),
            execution_context_id,
        });
    }

    Ok(ResolvedFrameContext {
        frame: primary.frame.clone(),
        lineage: build_lineage(primary.frame.frame_id.as_str(), &inventory),
        execution_context_id: None,
    })
}

async fn collect_frame_inventory(page: &Arc<Page>) -> Result<Vec<FrameInventoryEntry>, RubError> {
    let main_frame = page
        .mainframe()
        .await
        .map_err(|error| RubError::Internal(format!("Read main frame failed: {error}")))?
        .ok_or_else(|| RubError::Internal("Main frame is unavailable".to_string()))?;
    let frames = page
        .frames()
        .await
        .map_err(|error| RubError::Internal(format!("Read frame inventory failed: {error}")))?;

    let mut raw_entries = Vec::with_capacity(frames.len().saturating_add(1));
    let mut seen_frame_ids = HashSet::new();
    let mut ordered_frames = Vec::with_capacity(frames.len().saturating_add(1));
    ordered_frames.push(main_frame.clone());
    for frame_id in frames {
        if frame_id != main_frame {
            ordered_frames.push(frame_id);
        }
    }

    let main_frame_key = main_frame.as_ref().to_string();
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
            same_origin_accessible: if frame_id == main_frame {
                Some(true)
            } else {
                probe_same_origin_accessibility(page, &frame_id).await?
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
    use super::{build_lineage, frame_depth};
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
}
