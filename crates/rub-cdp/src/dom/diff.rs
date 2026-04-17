use std::collections::{BTreeSet, HashMap, HashSet};

use rub_core::model::{
    BoundingBox, ChangedElement, DiffElement, DiffResult, DiffSemanticKind, DiffSummary, Element,
    FieldChange, Snapshot,
};

/// Compare two snapshots and produce a structured diff.
///
/// Matching strategy:
/// 1. Primary: `element_ref` (stable CDP backend node id)
/// 2. Fallback: `(tag, text)` tuple for elements without refs
pub fn diff_snapshots(old: &Snapshot, new: &Snapshot) -> DiffResult {
    let mut old_by_ref: HashMap<String, &Element> = HashMap::new();
    for el in &old.elements {
        if let Some(ref r) = el.element_ref {
            old_by_ref.insert(r.clone(), el);
        }
    }

    let mut matched_old_refs: HashSet<String> = HashSet::new();
    let mut matched_old_indices: HashSet<u32> = HashSet::new();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged_count: u32 = 0;

    for new_el in &new.elements {
        let old_el = new_el
            .element_ref
            .as_ref()
            .and_then(|r| old_by_ref.get(r).copied());

        if let Some(old_el) = old_el {
            if let Some(ref r) = new_el.element_ref {
                matched_old_refs.insert(r.clone());
            }
            matched_old_indices.insert(old_el.index);

            let changes = compute_field_changes(old_el, new_el);
            if changes.is_empty() {
                unchanged_count += 1;
            } else {
                let semantic_kinds = semantic_kinds_for_changes(&changes);
                changed.push(ChangedElement {
                    index: new_el.index,
                    tag: new_el.tag,
                    semantic_kinds,
                    changes,
                });
            }
        } else {
            let fallback_match = old.elements.iter().find(|oe| {
                !matched_old_indices.contains(&oe.index)
                    && oe.tag == new_el.tag
                    && oe.text == new_el.text
            });

            if let Some(oe) = fallback_match {
                matched_old_indices.insert(oe.index);
                let changes = compute_field_changes(oe, new_el);
                if changes.is_empty() {
                    unchanged_count += 1;
                } else {
                    let semantic_kinds = semantic_kinds_for_changes(&changes);
                    changed.push(ChangedElement {
                        index: new_el.index,
                        tag: new_el.tag,
                        semantic_kinds,
                        changes,
                    });
                }
            } else {
                added.push(DiffElement {
                    index: new_el.index,
                    tag: new_el.tag,
                    text: new_el.text.clone(),
                    element_ref: new_el.element_ref.clone(),
                });
            }
        }
    }

    let removed: Vec<DiffElement> = old
        .elements
        .iter()
        .filter(|oe| !matched_old_indices.contains(&oe.index))
        .map(|oe| DiffElement {
            index: oe.index,
            tag: oe.tag,
            text: oe.text.clone(),
            element_ref: oe.element_ref.clone(),
        })
        .collect();

    let has_changes = !added.is_empty() || !removed.is_empty() || !changed.is_empty();
    let summary = summarize_diff(&added, &removed, &changed);

    DiffResult {
        snapshot_id: new.snapshot_id.clone(),
        diff_base: old.snapshot_id.clone(),
        dom_epoch: new.dom_epoch,
        has_changes,
        added,
        removed,
        changed,
        unchanged_count,
        summary,
    }
}

fn compute_field_changes(old: &Element, new: &Element) -> Vec<FieldChange> {
    let mut changes = Vec::new();

    if old.text != new.text {
        changes.push(FieldChange {
            field: "text".to_string(),
            from: old.text.clone(),
            to: new.text.clone(),
        });
    }

    if old.tag != new.tag {
        changes.push(FieldChange {
            field: "tag".to_string(),
            from: format!("{:?}", old.tag).to_lowercase(),
            to: format!("{:?}", new.tag).to_lowercase(),
        });
    }

    if old.bounding_box != new.bounding_box {
        changes.push(FieldChange {
            field: "bounding_box".to_string(),
            from: format_bounding_box(old.bounding_box),
            to: format_bounding_box(new.bounding_box),
        });
    }

    for (key, old_val) in &old.attributes {
        match new.attributes.get(key) {
            Some(new_val) if new_val != old_val => {
                changes.push(FieldChange {
                    field: format!("attributes.{key}"),
                    from: old_val.clone(),
                    to: new_val.clone(),
                });
            }
            None => {
                changes.push(FieldChange {
                    field: format!("attributes.{key}"),
                    from: old_val.clone(),
                    to: String::new(),
                });
            }
            _ => {}
        }
    }

    for (key, new_val) in &new.attributes {
        if !old.attributes.contains_key(key) {
            changes.push(FieldChange {
                field: format!("attributes.{key}"),
                from: String::new(),
                to: new_val.clone(),
            });
        }
    }

    if old.listeners != new.listeners {
        changes.push(FieldChange {
            field: "listeners".to_string(),
            from: format_listeners(old.listeners.as_deref()),
            to: format_listeners(new.listeners.as_deref()),
        });
    }

    append_ax_change(
        &mut changes,
        "ax.role",
        old.ax_info.as_ref().and_then(|info| info.role.as_deref()),
        new.ax_info.as_ref().and_then(|info| info.role.as_deref()),
    );
    append_ax_change(
        &mut changes,
        "ax.accessible_name",
        old.ax_info
            .as_ref()
            .and_then(|info| info.accessible_name.as_deref()),
        new.ax_info
            .as_ref()
            .and_then(|info| info.accessible_name.as_deref()),
    );
    append_ax_change(
        &mut changes,
        "ax.accessible_description",
        old.ax_info
            .as_ref()
            .and_then(|info| info.accessible_description.as_deref()),
        new.ax_info
            .as_ref()
            .and_then(|info| info.accessible_description.as_deref()),
    );

    changes
}

fn semantic_kinds_for_changes(changes: &[FieldChange]) -> Vec<DiffSemanticKind> {
    let mut kinds = BTreeSet::new();
    for change in changes {
        let kind = if change.field == "tag" {
            DiffSemanticKind::Identity
        } else if change.field == "text" {
            DiffSemanticKind::Content
        } else if change.field == "bounding_box" {
            DiffSemanticKind::Geometry
        } else if change.field == "listeners" {
            DiffSemanticKind::Listeners
        } else if change.field.starts_with("ax.") {
            DiffSemanticKind::Accessibility
        } else if change.field.starts_with("attributes.value")
            || change.field.starts_with("attributes.checked")
            || change.field.starts_with("attributes.selected")
        {
            DiffSemanticKind::Value
        } else if change.field.starts_with("attributes.aria-")
            || change.field.starts_with("attributes.role")
        {
            DiffSemanticKind::Accessibility
        } else {
            DiffSemanticKind::Attributes
        };
        kinds.insert(kind);
    }

    kinds.into_iter().collect()
}

fn summarize_diff(
    added: &[DiffElement],
    removed: &[DiffElement],
    changed: &[ChangedElement],
) -> DiffSummary {
    let mut summary = DiffSummary {
        added_count: added.len() as u32,
        removed_count: removed.len() as u32,
        changed_count: changed.len() as u32,
        ..DiffSummary::default()
    };

    for element in changed {
        for kind in &element.semantic_kinds {
            match kind {
                DiffSemanticKind::Identity => summary.identity_changes += 1,
                DiffSemanticKind::Content => summary.content_changes += 1,
                DiffSemanticKind::Value => summary.value_changes += 1,
                DiffSemanticKind::Attributes => summary.attribute_changes += 1,
                DiffSemanticKind::Geometry => summary.geometry_changes += 1,
                DiffSemanticKind::Accessibility => summary.accessibility_changes += 1,
                DiffSemanticKind::Listeners => summary.listener_changes += 1,
            }
        }
    }

    summary
}

fn format_bounding_box(rect: Option<BoundingBox>) -> String {
    rect.map(|rect| {
        format!(
            "{:.1},{:.1},{:.1},{:.1}",
            rect.x, rect.y, rect.width, rect.height
        )
    })
    .unwrap_or_default()
}

fn format_listeners(listeners: Option<&[String]>) -> String {
    listeners
        .map(|listeners| listeners.join(","))
        .unwrap_or_default()
}

fn append_ax_change(
    changes: &mut Vec<FieldChange>,
    field: &str,
    old: Option<&str>,
    new: Option<&str>,
) {
    if old == new {
        return;
    }
    changes.push(FieldChange {
        field: field.to_string(),
        from: old.unwrap_or_default().to_string(),
        to: new.unwrap_or_default().to_string(),
    });
}
