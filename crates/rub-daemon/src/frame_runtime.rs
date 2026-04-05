use std::collections::HashMap;

use rub_core::model::{
    FrameContextInfo, FrameContextStatus, FrameInventoryEntry, FrameRuntimeInfo,
};

/// Session-scoped frame-context authority.
#[derive(Debug, Default)]
pub struct FrameRuntimeState {
    selected_frame_id: Option<String>,
    projection: FrameRuntimeInfo,
}

impl FrameRuntimeState {
    pub fn projection(&self) -> FrameRuntimeInfo {
        self.projection.clone()
    }

    pub fn selected_frame_id(&self) -> Option<String> {
        self.selected_frame_id.clone()
    }

    pub fn select_frame(&mut self, frame_id: Option<String>) {
        self.selected_frame_id = frame_id;
    }

    pub fn replace(&mut self, projection: FrameRuntimeInfo) {
        self.projection = projection;
    }

    pub fn apply_inventory(&mut self, inventory: &[FrameInventoryEntry]) {
        let Some(primary) = inventory
            .iter()
            .find(|entry| entry.is_primary)
            .or_else(|| inventory.first())
        else {
            self.projection = FrameRuntimeInfo {
                status: FrameContextStatus::Degraded,
                degraded_reason: Some("frame_inventory_empty".to_string()),
                ..FrameRuntimeInfo::default()
            };
            return;
        };

        let projection = if let Some(selected_frame_id) = self.selected_frame_id.as_deref() {
            if let Some(selected) = inventory
                .iter()
                .find(|entry| entry.frame.frame_id == selected_frame_id)
            {
                let status = if selected.is_primary {
                    FrameContextStatus::Top
                } else {
                    FrameContextStatus::Child
                };
                FrameRuntimeInfo {
                    status,
                    current_frame: Some(selected.frame.clone()),
                    primary_frame: Some(primary.frame.clone()),
                    frame_lineage: build_lineage(selected_frame_id, inventory),
                    degraded_reason: None,
                }
            } else {
                FrameRuntimeInfo {
                    status: FrameContextStatus::Stale,
                    current_frame: Some(stale_frame_context(
                        selected_frame_id,
                        self.projection.current_frame.as_ref(),
                    )),
                    primary_frame: Some(primary.frame.clone()),
                    frame_lineage: vec![selected_frame_id.to_string()],
                    degraded_reason: Some("selected_frame_not_found".to_string()),
                }
            }
        } else {
            FrameRuntimeInfo {
                status: FrameContextStatus::Top,
                current_frame: Some(primary.frame.clone()),
                primary_frame: Some(primary.frame.clone()),
                frame_lineage: build_lineage(primary.frame.frame_id.as_str(), inventory),
                degraded_reason: None,
            }
        };

        self.projection = projection;
    }

    pub fn project_inventory(&self, inventory: &[FrameInventoryEntry]) -> Vec<FrameInventoryEntry> {
        let current_frame_id = self
            .projection
            .current_frame
            .as_ref()
            .map(|frame| frame.frame_id.as_str());
        let primary_frame_id = self
            .projection
            .primary_frame
            .as_ref()
            .map(|frame| frame.frame_id.as_str());

        inventory
            .iter()
            .cloned()
            .map(|mut entry| {
                entry.is_current =
                    current_frame_id.is_some_and(|frame_id| frame_id == entry.frame.frame_id);
                entry.is_primary =
                    primary_frame_id.is_some_and(|frame_id| frame_id == entry.frame.frame_id);
                entry
            })
            .collect()
    }

    pub fn mark_degraded(&mut self, reason: impl Into<String>) {
        self.projection = FrameRuntimeInfo {
            status: FrameContextStatus::Degraded,
            degraded_reason: Some(reason.into()),
            ..FrameRuntimeInfo::default()
        };
    }
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

fn stale_frame_context(
    selected_frame_id: &str,
    previous: Option<&FrameContextInfo>,
) -> FrameContextInfo {
    if let Some(previous) = previous
        && previous.frame_id == selected_frame_id
    {
        return previous.clone();
    }

    FrameContextInfo {
        frame_id: selected_frame_id.to_string(),
        name: None,
        parent_frame_id: None,
        target_id: None,
        url: None,
        depth: 0,
        same_origin_accessible: None,
    }
}

#[cfg(test)]
mod tests {
    use super::FrameRuntimeState;
    use rub_core::model::{
        FrameContextInfo, FrameContextStatus, FrameInventoryEntry, FrameRuntimeInfo,
    };

    #[test]
    fn frame_runtime_state_tracks_projection_and_degradation() {
        let mut state = FrameRuntimeState::default();
        assert_eq!(state.projection().status, FrameContextStatus::Unknown);

        state.replace(FrameRuntimeInfo {
            status: FrameContextStatus::Top,
            current_frame: Some(FrameContextInfo {
                frame_id: "main-frame".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            }),
            primary_frame: None,
            frame_lineage: vec!["main-frame".to_string()],
            degraded_reason: None,
        });
        assert_eq!(state.projection().status, FrameContextStatus::Top);

        state.mark_degraded("frame_probe_failed:no_page");
        let projection = state.projection();
        assert_eq!(projection.status, FrameContextStatus::Degraded);
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("frame_probe_failed:no_page")
        );
        assert!(projection.current_frame.is_none());
    }

    #[test]
    fn frame_runtime_state_projects_selected_child_frame_from_inventory() {
        let mut state = FrameRuntimeState::default();
        state.select_frame(Some("child-frame".to_string()));

        state.apply_inventory(&[
            FrameInventoryEntry {
                index: 0,
                frame: FrameContextInfo {
                    frame_id: "main-frame".to_string(),
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
                    frame_id: "child-frame".to_string(),
                    name: Some("child".to_string()),
                    parent_frame_id: Some("main-frame".to_string()),
                    target_id: None,
                    url: Some("https://example.test/frame".to_string()),
                    depth: 1,
                    same_origin_accessible: Some(true),
                },
                is_current: false,
                is_primary: false,
            },
        ]);

        let projection = state.projection();
        assert_eq!(projection.status, FrameContextStatus::Child);
        assert_eq!(
            projection
                .current_frame
                .as_ref()
                .map(|frame| frame.frame_id.as_str()),
            Some("child-frame")
        );
        assert_eq!(
            projection.frame_lineage,
            vec!["child-frame".to_string(), "main-frame".to_string()]
        );
    }

    #[test]
    fn frame_runtime_state_marks_missing_selected_frame_as_stale() {
        let mut state = FrameRuntimeState::default();
        state.select_frame(Some("missing-frame".to_string()));

        state.apply_inventory(&[FrameInventoryEntry {
            index: 0,
            frame: FrameContextInfo {
                frame_id: "main-frame".to_string(),
                name: Some("main".to_string()),
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.test".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            is_current: true,
            is_primary: true,
        }]);

        let projection = state.projection();
        assert_eq!(projection.status, FrameContextStatus::Stale);
        assert_eq!(
            projection
                .current_frame
                .as_ref()
                .map(|frame| frame.frame_id.as_str()),
            Some("missing-frame")
        );
        assert_eq!(
            projection.degraded_reason.as_deref(),
            Some("selected_frame_not_found")
        );
    }
}
