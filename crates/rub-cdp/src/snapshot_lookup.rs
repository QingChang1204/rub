use rub_core::model::{Element, Snapshot};
use std::collections::HashMap;

pub(crate) type SnapshotIndexLookup<'a> = HashMap<u32, &'a Element>;

pub(crate) fn build_snapshot_index_lookup(snapshot: &Snapshot) -> SnapshotIndexLookup<'_> {
    snapshot
        .elements
        .iter()
        .map(|element| (element.index, element))
        .collect()
}

pub(crate) fn clone_snapshot_elements_by_index(
    snapshot_index: &SnapshotIndexLookup<'_>,
    indexes: impl IntoIterator<Item = u32>,
) -> Vec<(u32, Element)> {
    indexes
        .into_iter()
        .filter_map(|index| {
            snapshot_index
                .get(&index)
                .map(|element| (element.index, (*element).clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{build_snapshot_index_lookup, clone_snapshot_elements_by_index};
    use rub_core::model::{
        Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot, SnapshotProjection,
    };
    use std::collections::HashMap;

    #[test]
    fn clone_snapshot_elements_by_index_preserves_requested_order() {
        let snapshot = sample_snapshot();
        let index = build_snapshot_index_lookup(&snapshot);

        let resolved = clone_snapshot_elements_by_index(&index, [3, 1]);

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].0, 3);
        assert_eq!(resolved[0].1.text, "Third");
        assert_eq!(resolved[1].0, 1);
        assert_eq!(resolved[1].1.text, "First");
    }

    #[test]
    fn clone_snapshot_elements_by_index_skips_missing_indexes() {
        let snapshot = sample_snapshot();
        let index = build_snapshot_index_lookup(&snapshot);

        let resolved = clone_snapshot_elements_by_index(&index, [9, 2]);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, 2);
        assert_eq!(resolved[0].1.text, "Second");
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            snapshot_id: "snap-lookup".to_string(),
            dom_epoch: 1,
            frame_context: FrameContextInfo {
                frame_id: "frame-main".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            frame_lineage: vec!["frame-main".to_string()],
            url: "https://example.test".to_string(),
            title: "Example".to_string(),
            total_count: 3,
            viewport_count: Some(3),
            truncated: false,
            elements: vec![
                sample_element(1, "First"),
                sample_element(2, "Second"),
                sample_element(3, "Third"),
            ],
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-04-15T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 3,
                backend_traversal_count: 3,
                resolved_ref_count: 3,
                warning: None,
            },
            viewport_filtered: None,
        }
    }

    fn sample_element(index: u32, text: &str) -> Element {
        Element {
            index,
            tag: ElementTag::Button,
            text: text.to_string(),
            attributes: HashMap::new(),
            element_ref: Some(format!("frame-main:{index}")),
            target_id: None,
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: Some(0),
        }
    }
}
