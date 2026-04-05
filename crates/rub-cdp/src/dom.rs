//! DOM inspection and snapshot building.

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
use rub_core::model::{
    AXInfo, BoundingBox, ChangedElement, DiffElement, DiffResult, DiffSemanticKind, DiffSummary,
    Element, ElementTag, FieldChange, ScrollPosition, Snapshot,
};

/// JavaScript template to extract interactive elements from the page.
///
/// `__INCLUDE_LISTENERS__` is replaced at runtime so the listener-augmented
/// projection and the plain interactive projection share one classifier authority.
const EXTRACT_ELEMENTS_JS_TEMPLATE: &str = r##"
(() => {
    const includeListeners = __INCLUDE_LISTENERS__;
    const interactiveTags = new Set(['a', 'button', 'input', 'textarea', 'select', 'option']);
    const interactiveRoles = new Set([
        'button', 'link', 'menuitem', 'tab', 'checkbox', 'radio',
        'switch', 'textbox', 'combobox', 'listbox', 'option'
    ]);

    const elements = [];
    const root = document.body || document.documentElement;
    let index = 0;
    let domIndex = 0;

    function getTag(el) {
        const tag = el.tagName.toLowerCase();
        if (tag === 'a') return 'link';
        if (tag === 'textarea') return 'textarea';
        if (tag === 'select') return 'select';
        if (tag === 'option') return 'option';
        if (tag === 'input') {
            const type = el.type || 'text';
            if (type === 'checkbox') return 'checkbox';
            if (type === 'radio') return 'radio';
            return 'input';
        }
        if (tag === 'button') return 'button';
        return 'other';
    }

    function isInteractive(el) {
        const tag = el.tagName.toLowerCase();
        if (interactiveTags.has(tag)) return true;
        const role = el.getAttribute('role');
        if (role && interactiveRoles.has(role)) return true;
        if (el.onclick || el.hasAttribute('onclick')) return true;
        if (el.getAttribute('tabindex') !== null) return true;
        return false;
    }

    function getListeners(el) {
        if (!includeListeners || typeof getEventListeners !== 'function') return [];
        try {
            const listeners = getEventListeners(el);
            return Object.keys(listeners)
                .filter((name) => Array.isArray(listeners[name]) && listeners[name].length > 0)
                .sort();
        } catch (_) {
            return [];
        }
    }

    function getText(el) {
        return (el.textContent || '').trim().substring(0, 200);
    }

    function getAttrs(el) {
        const attrs = {};
        for (const name of ['href', 'placeholder', 'aria-label', 'type', 'name', 'value', 'role', 'title', 'alt', 'id', 'data-testid', 'data-test-id', 'data-test']) {
            const val = el.getAttribute(name);
            if (val != null && val !== '') attrs[name] = val;
        }
        return attrs;
    }

    function getRect(el) {
        const r = el.getBoundingClientRect();
        let x = r.x;
        let y = r.y;
        let current = window;
        while (current !== current.top) {
            try {
                const frameEl = current.frameElement;
                if (!frameEl) break;
                const fr = frameEl.getBoundingClientRect();
                x += fr.x;
                y += fr.y;
                current = current.parent;
            } catch (_) {
                break;
            }
        }
        return { x, y, width: r.width, height: r.height };
    }

    function walk(node, depth) {
        if (!node || node.nodeType !== 1) return;
        const listeners = getListeners(node);
        if (isInteractive(node) || listeners.length > 0) {
            // L0 stealth: do NOT set any DOM attributes here.
            // Index determined by traversal order.
            const extracted = {
                index,
                dom_index: domIndex,
                depth,
                tag: getTag(node),
                text: getText(node),
                attributes: getAttrs(node),
                bounding_box: getRect(node),
            };
            if (includeListeners) {
                extracted.listeners = listeners;
            }
            elements.push(extracted);
            index++;
        }
        domIndex++;
        for (const child of Array.from(node.children || [])) {
            walk(child, depth + 1);
        }
    }

    if (root) {
        walk(root, 0);
    }

    return JSON.stringify({
        elements,
        traversal_count: domIndex,
        title: document.title,
        scroll: (() => {
            let vp = window;
            try { while (vp !== vp.top) vp = vp.parent; } catch (_) { vp = window; }
            return {
                x: vp.pageXOffset || vp.document.documentElement.scrollLeft,
                y: vp.pageYOffset || vp.document.documentElement.scrollTop,
                at_bottom: (vp.innerHeight + vp.pageYOffset) >= (vp.document.documentElement.scrollHeight - 2)
            };
        })()
    });
})()
"##;

fn extract_elements_script(include_listeners: bool) -> String {
    EXTRACT_ELEMENTS_JS_TEMPLATE.replace(
        "__INCLUDE_LISTENERS__",
        if include_listeners { "true" } else { "false" },
    )
}

/// JavaScript to remove all injected highlight overlays (shadow host cleanup).
pub const CLEANUP_HIGHLIGHT_JS: &str = r##"
(() => {
    // Clean shadow DOM host
    const host = document.getElementById('__rub_overlay_host__');
    if (host) host.remove();
    // Legacy cleanup (pre-v1.4)
    const labels = document.querySelectorAll('[data-rub-highlight]');
    for (const l of labels) l.remove();
})()
"##;

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

    let extract_script = extract_elements_script(include_listeners);
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
    let effective_limit = match limit {
        Some(0) | None => usize::MAX,
        Some(value) => value as usize,
    };

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

/// Build the overlay script for `screenshot --highlight` from a published snapshot.
pub fn highlight_overlay_js(snapshot: &Snapshot) -> Result<String, RubError> {
    let overlays = snapshot
        .elements
        .iter()
        .filter_map(|element| {
            let bbox = element.bounding_box?;
            if bbox.width == 0.0 && bbox.height == 0.0 {
                return None;
            }
            Some(serde_json::json!({
                "index": element.index,
                "left": snapshot.scroll.x + bbox.x,
                "top": snapshot.scroll.y + bbox.y,
            }))
        })
        .collect::<Vec<_>>();
    let overlays_json = serde_json::to_string(&overlays)
        .map_err(|e| RubError::Internal(format!("Highlight overlay JSON failed: {e}")))?;

    Ok(format!(
        r#"
        (() => {{
            const overlays = {overlays_json};
            // Use a shadow DOM host to isolate overlays from main DOM
            let host = document.getElementById('__rub_overlay_host__');
            if (!host) {{
                host = document.createElement('div');
                host.id = '__rub_overlay_host__';
                host.style.cssText = 'position:absolute;top:0;left:0;width:0;height:0;overflow:visible;z-index:2147483647;pointer-events:none';
                document.body.appendChild(host);
            }}
            const shadow = host.shadowRoot || host.attachShadow({{ mode: 'closed' }});
            while (shadow.firstChild) {{
                shadow.removeChild(shadow.firstChild);
            }}
            let count = 0;
            for (const item of overlays) {{
                const label = document.createElement('div');
                label.textContent = String(item.index);
                label.style.cssText = [
                    'position:absolute',
                    `top:${{item.top}}px`,
                    `left:${{item.left}}px`,
                    'background:rgba(255,59,48,0.85)',
                    'color:#fff',
                    'font:bold 11px/14px system-ui,sans-serif',
                    'padding:1px 4px',
                    'border-radius:3px',
                    'pointer-events:none',
                    'white-space:nowrap',
                ].join(';');
                shadow.appendChild(label);
                count++;
            }}
            return count;
        }})()
        "#
    ))
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

// ── v1.3: State Diff ────────────────────────────────────────────────

/// Compare two snapshots and produce a structured diff.
///
/// Matching strategy:
/// 1. Primary: `element_ref` (stable CDP backend node id)
/// 2. Fallback: `(tag, text)` tuple for elements without refs
pub fn diff_snapshots(old: &Snapshot, new: &Snapshot) -> DiffResult {
    use std::collections::HashSet;

    // Build old element lookup by element_ref
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
        // Try matching by element_ref
        let old_el = new_el
            .element_ref
            .as_ref()
            .and_then(|r| old_by_ref.get(r).copied());

        if let Some(old_el) = old_el {
            // Matched by ref
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
            // Try fallback match by (tag, text)
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

    // Remaining old elements not matched → removed
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

    // Check attribute changes
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
    use std::collections::BTreeSet;

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

#[cfg(test)]
mod tests {
    use super::{ExtractedElementsPayload, extract_elements_script, highlight_overlay_js};
    use rub_core::model::{Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot};
    use std::collections::HashMap;

    /// Contract test: the JSON payload emitted by EXTRACT_ELEMENTS_JS_TEMPLATE must
    /// deserialize cleanly into ExtractedElementsPayload with all three top-level fields:
    /// `elements`, `title`, and `scroll`.
    ///
    /// This test does NOT require a browser — it exercises the serde contract directly.
    /// If the JS script's output shape or the Rust struct diverge, this test will catch it.
    #[test]
    fn extracted_elements_payload_deserializes_all_three_fields() {
        let json = serde_json::json!({
            "elements": [
                {
                    "index": 0,
                    "dom_index": 0,
                    "tag": "BUTTON",
                    "text": "Click me",
                    "attributes": {},
                    "bounding_box": null,
                    "listeners": null,
                    "depth": 0
                }
            ],
            "traversal_count": 1,
            "title": "Test Page Title",
            "scroll": {
                "x": 0.0,
                "y": 42.5,
                "at_bottom": false
            }
        });

        let payload: ExtractedElementsPayload = serde_json::from_value(json)
            .expect("ExtractedElementsPayload must deserialize the unified JS payload shape");

        assert_eq!(payload.elements.len(), 1, "elements must be present");
        assert_eq!(payload.traversal_count, 1);
        assert_eq!(
            payload.title, "Test Page Title",
            "title must be extracted from the same JS context as elements"
        );
        let scroll = payload
            .scroll
            .expect("scroll must be present when JS returns it");
        assert_eq!(scroll.y, 42.5, "scroll.y must round-trip through serde");
        assert!(!scroll.at_bottom);
    }

    /// Backward-compatibility contract: payloads without `title` or `scroll` (e.g., from
    /// an older rub-cdp or a mocked frame context) must still deserialize successfully.
    /// The #[serde(default)] attributes must provide safe fallback values.
    #[test]
    fn extracted_elements_payload_handles_missing_optional_fields_via_serde_default() {
        let json_without_title_scroll = serde_json::json!({
            "elements": [],
            "traversal_count": 0
            // no "title", no "scroll" — simulates pre-merge payload shape
        });

        let payload: ExtractedElementsPayload = serde_json::from_value(json_without_title_scroll)
            .expect("ExtractedElementsPayload must not fail when title/scroll are absent");

        assert_eq!(
            payload.title, "",
            "title must default to empty string when absent (serde default)"
        );
        assert!(
            payload.scroll.is_none(),
            "scroll must default to None when absent (serde default)"
        );
    }

    #[test]
    fn extract_script_tracks_dom_index_without_dom_mutation() {
        let extract_script = extract_elements_script(true);
        assert!(extract_script.contains("listeners.length > 0"));
        assert!(extract_script.contains("dom_index"));
        assert!(extract_script.contains("traversal_count"));
        assert!(!extract_script.contains("setAttribute"));
    }

    #[test]
    fn plain_snapshot_resolution_uses_same_dom_index_model() {
        let extract_script = extract_elements_script(false);
        assert!(extract_script.contains("const includeListeners = false"));
        assert!(extract_script.contains("domIndex++"));
    }

    #[test]
    fn highlight_overlay_script_avoids_inner_html_assignment() {
        let snapshot = Snapshot {
            snapshot_id: "snap-1".to_string(),
            dom_epoch: 1,
            frame_context: FrameContextInfo {
                frame_id: "main".to_string(),
                name: None,
                parent_frame_id: None,
                target_id: Some("target-1".to_string()),
                url: Some("https://example.com".to_string()),
                depth: 0,
                same_origin_accessible: Some(true),
            },
            frame_lineage: vec!["main".to_string()],
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            elements: vec![Element {
                index: 0,
                tag: ElementTag::Button,
                text: "Example".to_string(),
                attributes: HashMap::new(),
                element_ref: None,
                bounding_box: Some(rub_core::model::BoundingBox {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 40.0,
                }),
                ax_info: None,
                listeners: Some(Vec::new()),
                depth: Some(0),
            }],
            total_count: 1,
            truncated: false,
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-03-29T00:00:00Z".to_string(),
            projection: rub_core::model::SnapshotProjection {
                verified: true,
                js_traversal_count: 1,
                backend_traversal_count: 1,
                resolved_ref_count: 1,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: None,
        };

        let script = highlight_overlay_js(&snapshot).unwrap();
        assert!(!script.contains("innerHTML"));
        assert!(script.contains("while (shadow.firstChild)"));
        assert!(script.contains("document.createElement('div')"));
    }
}
