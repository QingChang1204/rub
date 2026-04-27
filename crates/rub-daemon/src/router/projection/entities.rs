use rub_core::model::{Element, Page, Snapshot, TabInfo};

pub(crate) fn attach_subject(data: &mut serde_json::Value, subject: serde_json::Value) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    object.insert("subject".to_string(), subject);
}

pub(crate) fn attach_result(data: &mut serde_json::Value, result: serde_json::Value) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    object.insert("result".to_string(), result);
}

pub(crate) fn element_subject(element: &Element, snapshot_id: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "element",
        "index": element.index,
        "tag": element.tag,
        "text": element.text,
        "snapshot_id": snapshot_id,
    })
}

pub(crate) fn coordinates_subject(x: f64, y: f64) -> serde_json::Value {
    serde_json::json!({
        "kind": "coordinates",
        "x": x,
        "y": y,
    })
}

pub(crate) fn focused_frame_subject(frame_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "kind": "focused_frame",
        "frame_id": frame_id,
    })
}

pub(crate) fn viewport_subject(frame_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "kind": "viewport",
        "frame_id": frame_id,
    })
}

pub(crate) fn tab_subject(index: u32) -> serde_json::Value {
    serde_json::json!({
        "kind": "tab",
        "index": index,
    })
}

pub(crate) fn navigation_subject(action: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "tab_navigation",
        "action": action,
    })
}

pub(crate) fn page_entity(page: &Page) -> serde_json::Value {
    serde_json::json!({
        "url": &page.url,
        "title": &page.title,
        "http_status": page.http_status,
        "final_url": &page.final_url,
        "navigation_warning": page.navigation_warning.as_ref(),
    })
}

pub(crate) fn snapshot_entity(snapshot: &Snapshot) -> serde_json::Value {
    serde_json::json!({
        "snapshot_id": &snapshot.snapshot_id,
        "dom_epoch": snapshot.dom_epoch,
        "url": &snapshot.url,
        "title": &snapshot.title,
        "frame_context": &snapshot.frame_context,
        "frame_lineage": &snapshot.frame_lineage,
        "scroll": &snapshot.scroll,
        "timestamp": &snapshot.timestamp,
        "projection": &snapshot.projection,
        "viewport_filtered": snapshot.viewport_filtered,
        "viewport_count": snapshot.viewport_count,
    })
}

pub(crate) fn tab_entity(tab: &TabInfo) -> serde_json::Value {
    serde_json::json!({
        "index": tab.index,
        "target_id": &tab.target_id,
        "url": &tab.url,
        "title": &tab.title,
        "active": tab.active,
        "active_authority": tab.active_authority,
        "degraded_reason": tab.degraded_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::tab_entity;
    use rub_core::model::{TabActiveAuthority, TabInfo};

    #[test]
    fn tab_entity_projects_active_authority_provenance() {
        let tab = TabInfo {
            index: 1,
            target_id: "tab-active".to_string(),
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            active: true,
            active_authority: Some(TabActiveAuthority::LocalFallback),
            degraded_reason: None,
        };

        let entity = tab_entity(&tab);
        assert_eq!(
            entity["active_authority"],
            serde_json::json!("local_fallback")
        );
    }
}
