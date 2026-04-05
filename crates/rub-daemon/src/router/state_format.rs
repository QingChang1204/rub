use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{Element, Snapshot};

use super::element_semantics::{accessible_label, attr, attr_is, fallback_role, non_empty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StateFormat {
    Snapshot,
    A11y,
    Compact,
}

impl StateFormat {
    pub(super) fn parse(value: Option<&str>) -> Result<Self, RubError> {
        match value.unwrap_or("snapshot") {
            "snapshot" => Ok(Self::Snapshot),
            "interactive" => Ok(Self::Snapshot),
            "a11y" => Ok(Self::A11y),
            "compact" => Ok(Self::Compact),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Invalid state format '{other}'. Valid formats: snapshot, interactive, a11y, compact"
                ),
            )),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::A11y => "a11y",
            Self::Compact => "compact",
        }
    }
}

pub(super) fn project_snapshot(
    snapshot: &Snapshot,
    format: StateFormat,
) -> Result<serde_json::Value, RubError> {
    match format {
        StateFormat::Snapshot => serde_json::to_value(snapshot).map_err(RubError::from),
        StateFormat::A11y => {
            serde_json::to_value(build_a11y_projection(snapshot)).map_err(RubError::from)
        }
        StateFormat::Compact => {
            serde_json::to_value(build_compact_projection(snapshot)).map_err(RubError::from)
        }
    }
}

pub(super) fn summarize_snapshot_a11y(snapshot: &Snapshot) -> String {
    if snapshot.elements.is_empty() {
        return "No interactive elements matched the current scope/projection".to_string();
    }
    snapshot
        .elements
        .iter()
        .map(format_a11y_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn summarize_snapshot_compact(snapshot: &Snapshot) -> String {
    if snapshot.elements.is_empty() {
        return "No interactive elements matched the current scope/projection".to_string();
    }
    snapshot
        .elements
        .iter()
        .map(format_compact_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn summarize_element_label(element: &Element) -> String {
    accessible_label(element)
}

#[derive(Debug, serde::Serialize)]
struct A11yProjection<'a> {
    snapshot_id: &'a str,
    dom_epoch: u64,
    url: &'a str,
    title: &'a str,
    format: &'static str,
    a11y_text: String,
    entry_count: u32,
    total_count: u32,
    truncated: bool,
    scroll: rub_core::model::ScrollPosition,
}

#[derive(Debug, serde::Serialize)]
struct CompactProjection<'a> {
    snapshot_id: &'a str,
    dom_epoch: u64,
    url: &'a str,
    title: &'a str,
    format: &'static str,
    compact_text: String,
    entries: Vec<CompactProjectionEntry>,
    entry_count: u32,
    total_count: u32,
    truncated: bool,
    scroll: rub_core::model::ScrollPosition,
}

#[derive(Debug, serde::Serialize)]
struct CompactProjectionEntry {
    index: u32,
    depth: u32,
    role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    label: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    flags: Vec<String>,
}

fn build_a11y_projection(snapshot: &Snapshot) -> A11yProjection<'_> {
    A11yProjection {
        snapshot_id: &snapshot.snapshot_id,
        dom_epoch: snapshot.dom_epoch,
        url: &snapshot.url,
        title: &snapshot.title,
        format: "a11y",
        a11y_text: summarize_snapshot_a11y(snapshot),
        entry_count: snapshot.elements.len() as u32,
        total_count: snapshot.total_count,
        truncated: snapshot.truncated,
        scroll: snapshot.scroll,
    }
}

fn build_compact_projection(snapshot: &Snapshot) -> CompactProjection<'_> {
    CompactProjection {
        snapshot_id: &snapshot.snapshot_id,
        dom_epoch: snapshot.dom_epoch,
        url: &snapshot.url,
        title: &snapshot.title,
        format: "compact",
        compact_text: summarize_snapshot_compact(snapshot),
        entries: snapshot.elements.iter().map(build_compact_entry).collect(),
        entry_count: snapshot.elements.len() as u32,
        total_count: snapshot.total_count,
        truncated: snapshot.truncated,
        scroll: snapshot.scroll,
    }
}

fn format_a11y_line(element: &Element) -> String {
    let role = element
        .ax_info
        .as_ref()
        .and_then(|info| info.role.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| fallback_role(element).to_string());
    let label = accessible_label(element);
    let suffix = accessibility_suffixes(element);

    if label.is_empty() {
        format!("[{}] {}{}", element.index, role, suffix)
    } else {
        format!("[{}] {} \"{}\"{}", element.index, role, label, suffix)
    }
}

fn format_compact_line(element: &Element) -> String {
    let entry = build_compact_entry(element);
    let mut line = format!("[{}@{}] {}", entry.index, entry.depth, entry.role);
    if !entry.label.is_empty() {
        line.push(' ');
        line.push('"');
        line.push_str(&entry.label);
        line.push('"');
    }
    if !entry.flags.is_empty() {
        line.push_str(" [");
        line.push_str(&entry.flags.join(", "));
        line.push(']');
    }
    line
}

fn build_compact_entry(element: &Element) -> CompactProjectionEntry {
    CompactProjectionEntry {
        index: element.index,
        depth: element.depth.unwrap_or(0),
        role: element
            .ax_info
            .as_ref()
            .and_then(|info| info.role.as_deref())
            .map(str::to_string)
            .unwrap_or_else(|| fallback_role(element).to_string()),
        label: accessible_label(element),
        flags: element_flags(element),
    }
}

fn accessibility_suffixes(element: &Element) -> String {
    let suffixes = element_flags(element);
    if suffixes.is_empty() {
        String::new()
    } else {
        format!(" [{}]", suffixes.join(", "))
    }
}

fn element_flags(element: &Element) -> Vec<String> {
    let mut flags = Vec::new();

    if attr(element, "disabled").is_some() || attr_is(element, "aria-disabled", "true") {
        flags.push("disabled".to_string());
    }
    if attr_is(element, "checked", "true") || attr_is(element, "aria-checked", "true") {
        flags.push("checked".to_string());
    }
    if attr_is(element, "selected", "true") || attr_is(element, "aria-selected", "true") {
        flags.push("selected".to_string());
    }
    if let Some(description) = element
        .ax_info
        .as_ref()
        .and_then(|info| info.accessible_description.as_deref())
        .and_then(non_empty)
    {
        flags.push(format!("desc={description:?}"));
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use rub_core::model::{AXInfo, BoundingBox, ElementTag, ScrollPosition, SnapshotProjection};
    use std::collections::HashMap;

    fn sample_snapshot() -> Snapshot {
        let mut button_attrs = HashMap::new();
        button_attrs.insert("aria-label".to_string(), "Launch Rocket".to_string());

        let mut disabled_attrs = HashMap::new();
        disabled_attrs.insert("aria-disabled".to_string(), "true".to_string());

        Snapshot {
            snapshot_id: "snap-1".to_string(),
            dom_epoch: 2,
            frame_context: rub_core::model::FrameContextInfo {
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
                Element {
                    index: 0,
                    tag: ElementTag::Button,
                    text: "Go".to_string(),
                    attributes: button_attrs,
                    element_ref: Some("main:1".to_string()),
                    bounding_box: Some(BoundingBox {
                        x: 0.0,
                        y: 0.0,
                        width: 40.0,
                        height: 20.0,
                    }),
                    ax_info: Some(AXInfo {
                        role: Some("button".to_string()),
                        accessible_name: Some("Launch Rocket".to_string()),
                        accessible_description: None,
                    }),
                    listeners: None,
                    depth: Some(1),
                },
                Element {
                    index: 1,
                    tag: ElementTag::Link,
                    text: "".to_string(),
                    attributes: disabled_attrs,
                    element_ref: Some("main:2".to_string()),
                    bounding_box: None,
                    ax_info: Some(AXInfo {
                        role: Some("link".to_string()),
                        accessible_name: Some("Terms".to_string()),
                        accessible_description: Some("Opens legal page".to_string()),
                    }),
                    listeners: None,
                    depth: Some(2),
                },
            ],
            total_count: 2,
            truncated: false,
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: true,
            },
            timestamp: "2026-03-30T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 2,
                backend_traversal_count: 2,
                resolved_ref_count: 2,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: None,
        }
    }

    #[test]
    fn a11y_projection_prefers_accessible_names_and_flags() {
        let snapshot = sample_snapshot();
        let projection = build_a11y_projection(&snapshot);
        assert_eq!(projection.format, "a11y");
        assert!(
            projection
                .a11y_text
                .contains("[0] button \"Launch Rocket\"")
        );
        assert!(
            projection
                .a11y_text
                .contains("[1] link \"Terms\" [disabled, desc=\"Opens legal page\"]")
        );
    }

    #[test]
    fn parse_state_format_defaults_to_snapshot() {
        assert_eq!(StateFormat::parse(None).unwrap(), StateFormat::Snapshot);
        assert_eq!(StateFormat::parse(Some("a11y")).unwrap(), StateFormat::A11y);
        assert_eq!(
            StateFormat::parse(Some("compact")).unwrap(),
            StateFormat::Compact
        );
    }

    #[test]
    fn compact_projection_includes_depth_aware_entries() {
        let snapshot = sample_snapshot();
        let projection = build_compact_projection(&snapshot);
        assert_eq!(projection.format, "compact");
        assert_eq!(projection.entries.len(), 2);
        assert_eq!(projection.entries[0].depth, 1);
        assert_eq!(projection.entries[1].depth, 2);
        assert!(
            projection
                .compact_text
                .contains("[0@1] button \"Launch Rocket\""),
            "{projection:?}"
        );
        assert!(
            projection
                .compact_text
                .contains("[1@2] link \"Terms\" [disabled, desc=\"Opens legal page\"]"),
            "{projection:?}"
        );
    }

    #[test]
    fn empty_summaries_explain_projection_boundary() {
        let mut snapshot = sample_snapshot();
        snapshot.elements.clear();

        assert_eq!(
            summarize_snapshot_compact(&snapshot),
            "No interactive elements matched the current scope/projection"
        );
        assert_eq!(
            summarize_snapshot_a11y(&snapshot),
            "No interactive elements matched the current scope/projection"
        );
    }
}
