//! DOM inspection and snapshot building.

mod diff;
mod scripts;
#[cfg(test)]
mod tests;

use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::accessibility::{
    DisableParams as DisableAccessibilityParams, EnableParams as EnableAccessibilityParams,
    GetFullAxTreeParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, ExecutionContextId};
use std::collections::HashMap;
use std::sync::Arc;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use rub_core::error::RubError;
use rub_core::model::{AXInfo, BoundingBox, Element, ElementTag, ScrollPosition, Snapshot};
use rub_core::port::DEFAULT_SNAPSHOT_LIMIT;

pub use self::diff::diff_snapshots;
pub use self::scripts::{CLEANUP_HIGHLIGHT_JS, highlight_overlay_js};

/// Build a DOM snapshot from the current page state.
pub async fn build_snapshot(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
) -> Result<Snapshot, RubError> {
    build_snapshot_internal(page, dom_epoch, limit, None, false, None).await
}

/// Build a DOM snapshot from an explicit frame context.
pub async fn build_snapshot_for_frame(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
    frame_id: Option<&str>,
) -> Result<Snapshot, RubError> {
    build_snapshot_internal(page, dom_epoch, limit, None, false, frame_id).await
}

/// Build a DOM snapshot augmented with accessibility metadata.
pub async fn build_snapshot_with_a11y(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
) -> Result<Snapshot, RubError> {
    let ax_map = fetch_ax_map(page).await?;
    build_snapshot_internal(page, dom_epoch, limit, Some(ax_map), false, None).await
}

/// Build a DOM snapshot augmented with accessibility metadata for an explicit frame context.
pub async fn build_snapshot_with_a11y_for_frame(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
    frame_id: Option<&str>,
) -> Result<Snapshot, RubError> {
    let ax_map = fetch_ax_map(page).await?;
    build_snapshot_internal(page, dom_epoch, limit, Some(ax_map), false, frame_id).await
}

/// Build a DOM snapshot and promote nodes with JS listeners into the
/// interactive projection.
pub async fn build_snapshot_with_listeners(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
    include_a11y: bool,
) -> Result<Snapshot, RubError> {
    let ax_map = if include_a11y {
        Some(fetch_ax_map(page).await?)
    } else {
        None
    };
    build_snapshot_internal(page, dom_epoch, limit, ax_map, true, None).await
}

/// Build a DOM snapshot for an explicit frame context and promote nodes with JS listeners into
/// the interactive projection.
pub async fn build_snapshot_with_listeners_for_frame(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
    include_a11y: bool,
    frame_id: Option<&str>,
) -> Result<Snapshot, RubError> {
    let ax_map = if include_a11y {
        Some(fetch_ax_map(page).await?)
    } else {
        None
    };
    build_snapshot_internal(page, dom_epoch, limit, ax_map, true, frame_id).await
}

async fn build_snapshot_internal(
    page: &Arc<Page>,
    dom_epoch: u64,
    limit: Option<u32>,
    ax_map: Option<HashMap<i64, AXInfo>>,
    include_listeners: bool,
    frame_id: Option<&str>,
) -> Result<Snapshot, RubError> {
    let snapshot_id = Uuid::now_v7().to_string();
    let frame_context = crate::frame_runtime::resolve_frame_context(page, frame_id).await?;

    let url = frame_context.frame.url.clone().unwrap_or_default();

    let extract_script = scripts::extract_elements_script(include_listeners);
    let raw_elements = if include_listeners {
        extract_raw_elements_with_command_line(
            page,
            &extract_script,
            frame_context.execution_context_id,
        )
        .await?
    } else {
        extract_raw_elements(page, &extract_script, frame_context.execution_context_id).await?
    };

    let total_count = raw_elements.elements.len() as u32;
    let effective_limit = normalize_snapshot_limit(limit);

    let projection_resolution = crate::projection::resolve_backend_refs_for_frame(
        page,
        frame_context.frame.frame_id.as_str(),
        &raw_elements
            .elements
            .iter()
            .map(|raw| raw.dom_index)
            .collect::<Vec<_>>(),
        raw_elements.traversal_count,
    )
    .await?;

    let ax_map = ax_map.unwrap_or_default();
    let elements = raw_elements
        .elements
        .into_iter()
        .enumerate()
        .take(effective_limit)
        .map(|(position, raw)| {
            let element_ref = projection_resolution.refs.get(position).cloned().flatten();
            let ax_info = element_ref
                .as_ref()
                .and_then(|key| crate::targeting::parse_backend_node_id(Some(key.as_str())))
                .and_then(|backend_id| ax_map.get(backend_id.inner()).cloned());

            Element {
                index: raw.index,
                tag: parse_tag(&raw.tag),
                text: raw.text,
                attributes: raw.attributes,
                element_ref,
                bounding_box: raw.bounding_box.map(|bb| BoundingBox {
                    x: bb.x,
                    y: bb.y,
                    width: bb.width,
                    height: bb.height,
                }),
                ax_info,
                listeners: raw.listeners.filter(|listeners| !listeners.is_empty()),
                depth: Some(raw.depth),
            }
        })
        .collect::<Vec<_>>();

    let truncated = elements.len() < total_count as usize;
    let scroll = raw_elements
        .scroll
        .map(|s| ScrollPosition {
            x: s.x,
            y: s.y,
            at_bottom: s.at_bottom,
        })
        .unwrap_or(ScrollPosition {
            x: 0.0,
            y: 0.0,
            at_bottom: false,
        });
    let title = raw_elements.title;

    // See rub_cdp::dialogs::rfc3339_now — Rfc3339 format is infallible in
    // practice; sentinel is non-epoch to avoid silent timestamp corruption.
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "TIMESTAMP_FORMAT_ERROR".to_string());

    Ok(Snapshot {
        snapshot_id,
        dom_epoch,
        frame_context: frame_context.frame,
        frame_lineage: frame_context.lineage,
        url,
        title,
        elements,
        total_count,
        truncated,
        scroll,
        timestamp,
        projection: projection_resolution.projection,
        viewport_filtered: None,
        viewport_count: None,
    })
}

fn normalize_snapshot_limit(limit: Option<u32>) -> usize {
    match limit {
        None => DEFAULT_SNAPSHOT_LIMIT as usize,
        Some(0) => usize::MAX,
        Some(value) => value as usize,
    }
}

async fn extract_raw_elements(
    page: &Arc<Page>,
    script: &str,
    context_id: Option<ExecutionContextId>,
) -> Result<ExtractedElementsPayload, RubError> {
    let elements_json = page
        .execute(build_contextual_evaluate_params(script, context_id, false)?)
        .await
        .map_err(|e| RubError::Internal(format!("Element extraction failed: {e}")))?;
    let elements_str = elements_json
        .result
        .result
        .value
        .as_ref()
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            RubError::Internal("Element extraction returned no JSON string".to_string())
        })?;
    serde_json::from_str(elements_str)
        .map_err(|e| RubError::Internal(format!("Element JSON parse failed: {e}")))
}

async fn extract_raw_elements_with_command_line(
    page: &Arc<Page>,
    script: &str,
    context_id: Option<ExecutionContextId>,
) -> Result<ExtractedElementsPayload, RubError> {
    let params = build_contextual_evaluate_params(script, context_id, true)?;
    let response = page
        .execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("Element extraction failed: {e}")))?;
    let elements_str = response
        .result
        .result
        .value
        .as_ref()
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            RubError::Internal("Element extraction returned no JSON string".to_string())
        })?;
    serde_json::from_str(elements_str)
        .map_err(|e| RubError::Internal(format!("Element JSON parse failed: {e}")))
}

fn build_contextual_evaluate_params(
    expression: &str,
    context_id: Option<ExecutionContextId>,
    include_command_line_api: bool,
) -> Result<EvaluateParams, RubError> {
    let mut builder = EvaluateParams::builder()
        .expression(expression)
        .await_promise(true)
        .return_by_value(true)
        .include_command_line_api(include_command_line_api);
    if let Some(context_id) = context_id {
        builder = builder.context_id(context_id);
    }
    builder
        .build()
        .map_err(|e| RubError::Internal(format!("Build evaluate params failed: {e}")))
}

async fn fetch_ax_map(page: &Arc<Page>) -> Result<HashMap<i64, AXInfo>, RubError> {
    page.execute(EnableAccessibilityParams::default())
        .await
        .map_err(|e| RubError::Internal(format!("Accessibility.enable failed: {e}")))?;

    let response = page
        .execute(GetFullAxTreeParams::default())
        .await
        .map_err(|e| RubError::Internal(format!("Accessibility.getFullAXTree failed: {e}")))?;

    let _ = page.execute(DisableAccessibilityParams::default()).await;

    let mut map = HashMap::new();
    for node in response.result.nodes {
        if node.ignored {
            continue;
        }
        let Some(backend_node_id) = node.backend_dom_node_id else {
            continue;
        };

        let role = ax_value_to_string(node.role.as_ref());
        let accessible_name = ax_value_to_string(node.name.as_ref());
        let accessible_description = ax_value_to_string(node.description.as_ref());
        if role.is_none() && accessible_name.is_none() && accessible_description.is_none() {
            continue;
        }

        map.insert(
            *backend_node_id.inner(),
            AXInfo {
                role,
                accessible_name,
                accessible_description,
            },
        );
    }

    Ok(map)
}

fn ax_value_to_string(
    value: Option<&chromiumoxide::cdp::browser_protocol::accessibility::AxValue>,
) -> Option<String> {
    let value = value?.value.as_ref()?;
    match value {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(boolean) => Some(boolean.to_string()),
        _ => None,
    }
}

fn parse_tag(s: &str) -> ElementTag {
    match s {
        "button" => ElementTag::Button,
        "link" => ElementTag::Link,
        "input" => ElementTag::Input,
        "textarea" => ElementTag::TextArea,
        "select" => ElementTag::Select,
        "checkbox" => ElementTag::Checkbox,
        "radio" => ElementTag::Radio,
        "option" => ElementTag::Option,
        _ => ElementTag::Other,
    }
}

#[derive(Debug, serde::Deserialize)]
struct RawElement {
    index: u32,
    dom_index: u32,
    depth: u32,
    tag: String,
    text: String,
    attributes: HashMap<String, String>,
    bounding_box: Option<RawBoundingBox>,
    #[serde(default)]
    listeners: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
struct RawScrollPosition {
    x: f64,
    y: f64,
    at_bottom: bool,
}

#[derive(Debug, serde::Deserialize)]
struct ExtractedElementsPayload {
    elements: Vec<RawElement>,
    traversal_count: u32,
    /// Page title captured in the same JS execution context as element extraction.
    #[serde(default)]
    title: String,
    /// Scroll position captured in the same JS execution context as element extraction.
    #[serde(default)]
    scroll: Option<RawScrollPosition>,
}

#[derive(Debug, serde::Deserialize)]
struct RawBoundingBox {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
