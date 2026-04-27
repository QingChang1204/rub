use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{BoundingBox, Element, Snapshot};
use std::collections::HashMap;
use std::sync::Arc;

const MATCH_INTERACTIVE_SELECTOR_JS_TEMPLATE: &str = r##"
JSON.stringify((() => {
    const selector = __SELECTOR__;
    const includeListeners = __INCLUDE_LISTENERS__;
    const interactiveTags = new Set(['a', 'button', 'input', 'textarea', 'select', 'option']);
    const interactiveRoles = new Set([
        'button', 'link', 'menuitem', 'tab', 'checkbox', 'radio',
        'switch', 'textbox', 'combobox', 'listbox', 'option'
    ]);

    function isInteractive(el) {
        const tag = el.tagName.toLowerCase();
        if (interactiveTags.has(tag)) return true;
        if (el.isContentEditable) return true;
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

    function getText(el) {
        return (el.textContent || '').trim().substring(0, 200);
    }

    function getAttrs(el) {
        const attrs = {};
        for (const name of ['href', 'placeholder', 'aria-label', 'aria-readonly', 'type', 'name', 'value', 'role', 'title', 'alt', 'id', 'data-testid', 'data-test-id', 'data-test', 'contenteditable']) {
            const val = el.getAttribute(name);
            if (val != null && val !== '') attrs[name] = val;
        }
        if (el.isContentEditable && !('contenteditable' in attrs)) {
            attrs.contenteditable = 'true';
        }
        if (el.hasAttribute && el.hasAttribute('disabled')) {
            attrs.disabled = '';
        }
        if (el.hasAttribute && el.hasAttribute('readonly')) {
            attrs.readonly = '';
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

    try {
        document.querySelector(selector);
    } catch (error) {
        return {
            selector_error: String((error && error.message) || error || 'invalid selector'),
            match_entries: []
        };
    }

    const matchEntries = [];
    const walker = document.createTreeWalker(
        document.body || document.documentElement,
        NodeFilter.SHOW_ELEMENT,
        null
    );

        let node = walker.currentNode;
        let interactiveIndex = 0;
        while (node) {
            const listeners = node && node.nodeType === 1 ? getListeners(node) : [];
            if (node.nodeType === 1 && (isInteractive(node) || listeners.length > 0)) {
                if (node.matches(selector)) {
                    matchEntries.push({
                        index: interactiveIndex,
                        element: {
                            index: interactiveIndex,
                            tag: getTag(node),
                            text: getText(node),
                            attributes: getAttrs(node),
                            element_ref: null,
                            bounding_box: getRect(node),
                            ax_info: null,
                            listeners: includeListeners ? listeners : null,
                            depth: null
                        }
                    });
                }
                interactiveIndex++;
            }
            node = walker.nextNode();
        }

    return {
        selector_error: null,
        match_entries: matchEntries
    };
})())
"##;

#[derive(Debug, serde::Deserialize)]
struct SelectorMatchEntry {
    index: u32,
    element: Element,
}

#[derive(Debug, serde::Deserialize)]
struct SelectorMatchPayload {
    selector_error: Option<String>,
    match_entries: Vec<SelectorMatchEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct HtmlQueryPayload {
    selector_error: Option<String>,
    html: Option<String>,
}

fn selector_recovery_suggestion() -> &'static str {
    "Check the CSS selector syntax, or switch to --role/--label/--testid. Run 'rub observe' to see available elements"
}

fn selector_not_found_suggestion() -> &'static str {
    "Verify the selector in the current frame, or switch to --role/--label/--testid for a more stable locator. Run 'rub observe' to see available elements"
}

pub(crate) async fn get_title(page: &Arc<Page>) -> Result<String, RubError> {
    let result = page
        .evaluate("document.title")
        .await
        .map_err(|e| RubError::Internal(format!("Evaluate document.title failed: {e}")))?;
    Ok(result.into_value::<String>().unwrap_or_default())
}

pub(crate) async fn get_html(page: &Arc<Page>, selector: Option<&str>) -> Result<String, RubError> {
    if let Some(selector) = selector {
        let selector_json = serde_json::to_string(selector).map_err(|error| {
            RubError::Internal(format!("Selector serialization failed: {error}"))
        })?;
        let script = format!(
            r#"JSON.stringify((() => {{
                try {{
                    const selector = {selector_json};
                    const el = document.querySelector(selector);
                    return {{
                        selector_error: null,
                        html: el ? el.outerHTML : null,
                    }};
                }} catch (error) {{
                    return {{
                        selector_error: String(error && error.message ? error.message : error),
                        html: null,
                    }};
                }}
            }})())"#
        );
        let payload: HtmlQueryPayload = serde_json::from_str(
            &crate::js::evaluate_returning_string(page, script.as_str()).await?,
        )
        .map_err(|error| RubError::Internal(format!("Parse get_html result failed: {error}")))?;

        if let Some(selector_error) = payload.selector_error {
            return Err(RubError::domain_with_context_and_suggestion(
                ErrorCode::InvalidInput,
                format!("Invalid selector for get html: {selector_error}"),
                serde_json::json!({
                    "selector": selector,
                }),
                selector_recovery_suggestion(),
            ));
        }

        return payload.html.ok_or_else(|| {
            RubError::domain_with_context_and_suggestion(
                ErrorCode::ElementNotFound,
                format!("No element matching selector: '{selector}'"),
                serde_json::json!({
                    "selector": selector,
                }),
                selector_not_found_suggestion(),
            )
        });
    }

    let result = page
        .evaluate("document.documentElement.outerHTML")
        .await
        .map_err(|e| RubError::Internal(format!("Evaluate get_html failed: {e}")))?;
    match result.into_value::<String>() {
        Ok(html) => Ok(html),
        Err(error) => Err(RubError::Internal(format!(
            "Parse get_html result failed: {error}"
        ))),
    }
}

pub(crate) async fn get_text(page: &Arc<Page>, element: &Element) -> Result<String, RubError> {
    let resolved = crate::targeting::resolve_read_element(page, element).await?;
    crate::js::call_function_returning_string(
        page,
        &resolved.remote_object_id,
        "function() { return this.textContent || ''; }",
    )
    .await
}

pub(crate) async fn get_outer_html(
    page: &Arc<Page>,
    element: &Element,
) -> Result<String, RubError> {
    let resolved = crate::targeting::resolve_read_element(page, element).await?;
    crate::js::call_function_returning_string(
        page,
        &resolved.remote_object_id,
        "function() { return this.outerHTML || ''; }",
    )
    .await
}

pub(crate) async fn get_value(page: &Arc<Page>, element: &Element) -> Result<String, RubError> {
    let resolved = crate::targeting::resolve_read_element(page, element).await?;
    crate::js::call_function_returning_string(
        page,
        &resolved.remote_object_id,
        "function() { return this.value !== undefined ? String(this.value) : ''; }",
    )
    .await
}

pub(crate) async fn get_attributes(
    page: &Arc<Page>,
    element: &Element,
) -> Result<HashMap<String, String>, RubError> {
    let resolved = crate::targeting::resolve_read_element(page, element).await?;
    let result = crate::js::call_function_returning_string(
        page,
        &resolved.remote_object_id,
        "function() { var o = {}; for (var a of this.attributes) { o[a.name] = a.value; } return JSON.stringify(o); }",
    )
    .await?;
    parse_attributes_json(&result)
}

pub(crate) async fn get_bbox(page: &Arc<Page>, element: &Element) -> Result<BoundingBox, RubError> {
    let resolved = crate::targeting::resolve_read_element(page, element).await?;
    let bbox_json = crate::js::call_function_returning_value(
        page,
        &resolved.remote_object_id,
        crate::targeting::TOP_LEVEL_BOUNDING_BOX_FUNCTION,
    )
    .await
    .map_err(|error| {
        let message = error.to_string();
        if crate::targeting::top_level_geometry_error_reason(&message).is_some() {
            crate::targeting::top_level_geometry_authority_error(&message)
        } else {
            error
        }
    })?;
    serde_json::from_value::<BoundingBox>(bbox_json)
        .map_err(|e| RubError::Internal(format!("Parse bbox failed: {e}")))
}

pub(crate) async fn find_snapshot_elements_by_selector(
    page: &Arc<Page>,
    snapshot: &Snapshot,
    selector: &str,
) -> Result<Vec<Element>, RubError> {
    let include_listeners = snapshot_uses_listener_promoted_classifier(snapshot);
    resolve_selector_matches(
        page,
        snapshot,
        selector,
        MATCH_INTERACTIVE_SELECTOR_JS_TEMPLATE,
        "Selector",
        include_listeners,
    )
    .await
}

async fn resolve_selector_matches(
    page: &Arc<Page>,
    snapshot: &Snapshot,
    selector: &str,
    template: &str,
    error_label: &str,
    include_listeners: bool,
) -> Result<Vec<Element>, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(
        page,
        Some(snapshot.frame_context.frame_id.as_str()),
    )
    .await?;
    let document_before = crate::runtime_state::probe_live_read_document_fence(
        page,
        frame_context.execution_context_id,
    )
    .await;
    let selector_json = serde_json::to_string(selector)
        .map_err(|e| RubError::Internal(format!("Selector serialization failed: {e}")))?;
    let script = template.replace("__SELECTOR__", &selector_json).replace(
        "__INCLUDE_LISTENERS__",
        if include_listeners { "true" } else { "false" },
    );
    let raw = crate::js::evaluate_returning_string_in_context(
        page,
        frame_context.execution_context_id,
        &script,
    )
    .await?;
    let document_after = crate::runtime_state::probe_live_read_document_fence(
        page,
        frame_context.execution_context_id,
    )
    .await;
    crate::runtime_state::ensure_live_read_document_fence(
        error_label,
        snapshot.frame_context.frame_id.as_str(),
        document_before.as_ref(),
        document_after.as_ref(),
    )?;
    let payload: SelectorMatchPayload = serde_json::from_str(&raw)
        .map_err(|e| RubError::Internal(format!("Selector payload parse failed: {e}")))?;

    if let Some(selector_error) = payload.selector_error {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!("Invalid selector: {selector_error}"),
            serde_json::json!({
                "selector": selector,
                "kind": error_label,
            }),
            selector_recovery_suggestion(),
        ));
    }

    resolve_snapshot_selector_matches(snapshot, selector, error_label, payload.match_entries)
}

fn snapshot_uses_listener_promoted_classifier(snapshot: &Snapshot) -> bool {
    snapshot.elements.iter().any(|element| {
        element
            .listeners
            .as_ref()
            .is_some_and(|listeners| !listeners.is_empty())
    })
}

fn resolve_snapshot_selector_matches(
    snapshot: &Snapshot,
    selector: &str,
    error_label: &str,
    match_entries: Vec<SelectorMatchEntry>,
) -> Result<Vec<Element>, RubError> {
    let snapshot_by_index = crate::snapshot_lookup::build_snapshot_index_lookup(snapshot);
    let missing_indices = match_entries
        .iter()
        .map(|matched| matched.index)
        .filter(|index| !snapshot_by_index.contains_key(index))
        .collect::<Vec<_>>();
    let mismatched_indices = match_entries
        .iter()
        .filter_map(|matched| {
            let expected = snapshot_by_index.get(&matched.index)?;
            (!crate::targeting::snapshot_element_replay_matches_authority(
                expected,
                &matched.element,
            ))
            .then_some(matched.index)
        })
        .collect::<Vec<_>>();

    if !missing_indices.is_empty() || !mismatched_indices.is_empty() {
        return Err(snapshot_selector_replay_error(
            snapshot,
            selector,
            error_label,
            &missing_indices,
            &mismatched_indices,
        ));
    }

    let mut resolved = crate::snapshot_lookup::clone_snapshot_elements_by_index(
        &snapshot_by_index,
        match_entries.iter().map(|matched| matched.index),
    );

    if resolved.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!(
                "{error_label} '{selector}' did not resolve to an interactive snapshot element"
            ),
            serde_json::json!({
                "selector": selector,
                "kind": error_label,
            }),
            selector_not_found_suggestion(),
        ));
    }

    resolved.sort_by_key(|(index, _)| *index);
    Ok(resolved.into_iter().map(|(_, element)| element).collect())
}

fn snapshot_selector_replay_error(
    snapshot: &Snapshot,
    selector: &str,
    error_label: &str,
    missing_indices: &[u32],
    mismatched_indices: &[u32],
) -> RubError {
    let authority_state = if snapshot.truncated {
        "snapshot_selector_replay_truncated_cached_inventory"
    } else {
        "snapshot_selector_replay_classifier_mismatch"
    };
    let message = if snapshot.truncated {
        format!(
            "{error_label} '{selector}' matched elements outside the cached snapshot inventory, so selector-backed snapshot replay cannot stay authoritative on a truncated snapshot"
        )
    } else {
        format!(
            "{error_label} '{selector}' no longer aligns with the cached snapshot interactive inventory, so selector-backed snapshot replay cannot choose an authoritative target"
        )
    };
    RubError::domain_with_context_and_suggestion(
        ErrorCode::StaleSnapshot,
        message,
        serde_json::json!({
            "selector": selector,
            "kind": error_label,
            "authority_state": authority_state,
            "snapshot_id": snapshot.snapshot_id,
            "snapshot_truncated": snapshot.truncated,
            "missing_snapshot_indices": missing_indices,
            "mismatched_snapshot_indices": mismatched_indices,
        }),
        "Refresh state to rebuild the snapshot inventory before using selector-backed snapshot addressing again",
    )
}

fn parse_attributes_json(raw: &str) -> Result<HashMap<String, String>, RubError> {
    serde_json::from_str(raw)
        .map_err(|e| RubError::Internal(format!("Parse attributes failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::{
        SelectorMatchEntry, parse_attributes_json, resolve_snapshot_selector_matches,
        snapshot_uses_listener_promoted_classifier,
    };
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot, SnapshotProjection,
    };
    use std::collections::HashMap;

    #[test]
    fn parse_attributes_json_rejects_invalid_payload() {
        let error = parse_attributes_json("{").expect_err("invalid attributes should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InternalError);
    }

    #[test]
    fn snapshot_selector_replay_rejects_missing_indices_on_truncated_snapshot() {
        let mut snapshot = sample_snapshot();
        snapshot.truncated = true;

        let error = resolve_snapshot_selector_matches(
            &snapshot,
            ".cta",
            "Selector",
            vec![
                sample_match_entry(0, "First"),
                sample_match_entry(9, "Ghost"),
            ],
        )
        .expect_err("missing selector replay indexes on truncated snapshots must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope.context.expect("selector context")["authority_state"],
            "snapshot_selector_replay_truncated_cached_inventory"
        );
    }

    #[test]
    fn snapshot_selector_replay_rejects_classifier_mismatch_without_truncation() {
        let snapshot = sample_snapshot();

        let error = resolve_snapshot_selector_matches(
            &snapshot,
            "[contenteditable]",
            "Selector",
            vec![sample_match_entry(7, "Missing")],
        )
        .expect_err("missing selector replay indexes must fail closed even on full snapshots");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope.context.expect("selector context")["authority_state"],
            "snapshot_selector_replay_classifier_mismatch"
        );
    }

    #[test]
    fn snapshot_selector_replay_rejects_in_range_identity_mismatch() {
        let snapshot = sample_snapshot();

        let error = resolve_snapshot_selector_matches(
            &snapshot,
            ".cta",
            "Selector",
            vec![sample_match_entry(0, "Inserted")],
        )
        .expect_err("in-range selector replay mismatch must fail closed");
        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope.context.expect("selector context")["mismatched_snapshot_indices"],
            serde_json::json!([0])
        );
    }

    #[test]
    fn snapshot_selector_matcher_uses_listener_promoted_classifier_when_snapshot_has_listeners() {
        let mut snapshot = sample_snapshot();
        snapshot.elements[0].listeners = Some(vec!["click".to_string()]);
        assert!(snapshot_uses_listener_promoted_classifier(&snapshot));
    }

    #[test]
    fn snapshot_selector_matcher_skips_listener_classifier_when_snapshot_has_no_promoted_nodes() {
        let snapshot = sample_snapshot();
        assert!(!snapshot_uses_listener_promoted_classifier(&snapshot));
    }

    #[test]
    fn live_bbox_probe_uses_top_level_coordinate_projection() {
        assert!(
            crate::targeting::TOP_LEVEL_BOUNDING_BOX_FUNCTION.contains("current.frameElement"),
            "{}",
            crate::targeting::TOP_LEVEL_BOUNDING_BOX_FUNCTION
        );
        assert!(
            crate::targeting::TOP_LEVEL_BOUNDING_BOX_FUNCTION.contains("current = current.parent"),
            "{}",
            crate::targeting::TOP_LEVEL_BOUNDING_BOX_FUNCTION
        );
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            snapshot_id: "snap-selector".to_string(),
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
            elements: vec![sample_element(0, "First"), sample_element(1, "Second")],
            total_count: 2,
            truncated: false,
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-04-17T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 2,
                backend_traversal_count: 2,
                resolved_ref_count: 2,
                warning: None,
            },
            viewport_filtered: None,
            viewport_count: Some(2),
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

    fn sample_match_entry(index: u32, text: &str) -> SelectorMatchEntry {
        SelectorMatchEntry {
            index,
            element: sample_element(index, text),
        }
    }
}
