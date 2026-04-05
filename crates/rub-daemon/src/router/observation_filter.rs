use super::*;
use crate::router::request_args::parse_optional_u32_arg;
use rub_core::error::ErrorCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ObservationProjectionMode {
    Interactive,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ObservationProjectionPolicy {
    pub mode: ObservationProjectionMode,
    pub depth_limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(super) struct ObservationProjectionMetadata {
    pub mode: ObservationProjectionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth_count: Option<u32>,
}

pub(super) fn parse_observation_projection(
    args: &serde_json::Value,
    compact_via_format: bool,
) -> Result<ObservationProjectionPolicy, RubError> {
    let compact = args
        .get("compact")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let depth_limit = parse_optional_u32_arg(args, "depth")?;

    let mode = if compact || compact_via_format {
        ObservationProjectionMode::Compact
    } else {
        ObservationProjectionMode::Interactive
    };

    if compact && compact_via_format {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Observation compact projection is already implied by --format compact; remove redundant --compact",
        ));
    }

    Ok(ObservationProjectionPolicy { mode, depth_limit })
}

pub(super) fn apply_observation_projection(
    snapshot: &mut rub_core::model::Snapshot,
    policy: ObservationProjectionPolicy,
) -> ObservationProjectionMetadata {
    let depth_count = apply_projection_depth(snapshot, policy.depth_limit);
    ObservationProjectionMetadata {
        mode: policy.mode,
        depth_limit: policy.depth_limit,
        depth_count,
    }
}

pub(super) fn attach_observation_projection_metadata(
    value: &mut serde_json::Value,
    metadata: ObservationProjectionMetadata,
) {
    if let Some(object) = value.as_object_mut()
        && (metadata.mode != ObservationProjectionMode::Interactive
            || metadata.depth_limit.is_some())
    {
        object.insert(
            "observation_projection".to_string(),
            serde_json::json!(metadata),
        );
    }
}

fn apply_projection_depth(
    snapshot: &mut rub_core::model::Snapshot,
    depth_limit: Option<u32>,
) -> Option<u32> {
    let depth_limit = depth_limit?;
    let base_depth = snapshot
        .elements
        .iter()
        .filter_map(|element| element.depth)
        .min()
        .unwrap_or(0);
    snapshot.elements.retain(|element| {
        element.depth.unwrap_or(u32::MAX) <= base_depth.saturating_add(depth_limit)
    });
    let depth_count = snapshot.elements.len() as u32;
    snapshot.total_count = depth_count;
    snapshot.truncated = false;
    Some(depth_count)
}

#[cfg(test)]
mod tests {
    use super::{
        ObservationProjectionMetadata, ObservationProjectionMode, ObservationProjectionPolicy,
        apply_observation_projection, attach_observation_projection_metadata,
        parse_observation_projection,
    };
    use rub_core::model::{
        BoundingBox, Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot,
        SnapshotProjection,
    };
    use std::collections::HashMap;

    #[test]
    fn parse_projection_rejects_redundant_compact_flag() {
        let result = parse_observation_projection(&serde_json::json!({ "compact": true }), true);
        assert!(result.is_err());
    }

    #[test]
    fn apply_projection_depth_filters_elements_by_relative_depth() {
        let mut snapshot = sample_snapshot();
        let metadata = apply_observation_projection(
            &mut snapshot,
            ObservationProjectionPolicy {
                mode: ObservationProjectionMode::Compact,
                depth_limit: Some(1),
            },
        );

        assert_eq!(snapshot.elements.len(), 2);
        assert_eq!(snapshot.total_count, 2);
        assert_eq!(metadata.depth_count, Some(2));
    }

    #[test]
    fn apply_projection_depth_is_relative_to_scoped_root_depth() {
        let mut snapshot = sample_snapshot();
        for (depth, element) in [6, 7, 8].into_iter().zip(snapshot.elements.iter_mut()) {
            element.depth = Some(depth);
        }

        let metadata = apply_observation_projection(
            &mut snapshot,
            ObservationProjectionPolicy {
                mode: ObservationProjectionMode::Compact,
                depth_limit: Some(1),
            },
        );

        assert_eq!(snapshot.elements.len(), 2);
        assert_eq!(snapshot.elements[0].depth, Some(6));
        assert_eq!(snapshot.elements[1].depth, Some(7));
        assert_eq!(metadata.depth_count, Some(2));
    }

    #[test]
    fn attach_projection_metadata_publishes_compact_and_depth() {
        let mut value = serde_json::json!({});
        attach_observation_projection_metadata(
            &mut value,
            ObservationProjectionMetadata {
                mode: ObservationProjectionMode::Compact,
                depth_limit: Some(2),
                depth_count: Some(3),
            },
        );
        assert_eq!(value["observation_projection"]["mode"], "compact");
        assert_eq!(value["observation_projection"]["depth_limit"], 2);
        assert_eq!(value["observation_projection"]["depth_count"], 3);
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            snapshot_id: "snap-1".to_string(),
            dom_epoch: 1,
            frame_context: FrameContextInfo {
                frame_id: "main".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.com".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            frame_lineage: vec!["main".to_string()],
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            elements: vec![
                element(0, "Top", 0),
                element(1, "Mid", 1),
                element(2, "Deep", 2),
            ],
            total_count: 3,
            truncated: false,
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-03-30T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 3,
                backend_traversal_count: 3,
                resolved_ref_count: 3,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: None,
        }
    }

    fn element(index: u32, text: &str, depth: u32) -> Element {
        Element {
            index,
            tag: ElementTag::Button,
            text: text.to_string(),
            attributes: HashMap::new(),
            element_ref: Some(format!("main:{index}")),
            bounding_box: Some(BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            }),
            ax_info: None,
            listeners: None,
            depth: Some(depth),
        }
    }
}
