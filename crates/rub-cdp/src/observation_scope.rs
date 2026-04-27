use std::sync::Arc;

use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{Element, Snapshot};
use rub_core::observation::ObservationScope;

use crate::live_dom_locator::LOCATOR_JS_HELPERS;

#[derive(Debug, serde::Deserialize)]
struct ObservationScopePayload {
    scope_error: Option<String>,
    root_match_count: usize,
    selected_root_count: usize,
    match_entries: Vec<ObservationScopeMatchEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ObservationScopeMatchEntry {
    index: u32,
    depth: u32,
    element: Element,
}

fn snapshot_uses_listener_promoted_classifier(snapshot: &Snapshot) -> bool {
    snapshot.elements.iter().any(|element| {
        element
            .listeners
            .as_ref()
            .is_some_and(|listeners| !listeners.is_empty())
    })
}

fn observation_scope_script(scope_json: &str, include_listeners: bool) -> String {
    format!(
        r#"JSON.stringify((() => {{
            const scope = {scope_json};
            const includeListeners = {};
            {LOCATOR_JS_HELPERS}
            const interactiveTags = new Set(['a', 'button', 'input', 'textarea', 'select', 'option']);
            const interactiveRoles = new Set([
                'button', 'link', 'menuitem', 'tab', 'checkbox', 'radio',
                'switch', 'textbox', 'combobox', 'listbox', 'option'
            ]);

            const isInteractive = (el) => {{
                const tag = String(el.tagName || '').toLowerCase();
                if (interactiveTags.has(tag)) return true;
                if (el.isContentEditable) return true;
                const role = el.getAttribute && el.getAttribute('role');
                if (role && interactiveRoles.has(role)) return true;
                if (el.onclick || (el.hasAttribute && el.hasAttribute('onclick'))) return true;
                if (el.getAttribute && el.getAttribute('tabindex') !== null) return true;
                return false;
            }};

            const getListeners = (el) => {{
                if (!includeListeners || typeof getEventListeners !== 'function') return [];
                try {{
                    const listeners = getEventListeners(el);
                    return Object.keys(listeners)
                        .filter((name) => Array.isArray(listeners[name]) && listeners[name].length > 0)
                        .sort();
                }} catch (_) {{
                    return [];
                }}
            }};

            const getTag = (el) => {{
                const tag = String(el.tagName || '').toLowerCase();
                if (tag === 'a') return 'link';
                if (tag === 'textarea') return 'textarea';
                if (tag === 'select') return 'select';
                if (tag === 'option') return 'option';
                if (tag === 'input') {{
                    const type = el.type || 'text';
                    if (type === 'checkbox') return 'checkbox';
                    if (type === 'radio') return 'radio';
                    return 'input';
                }}
                if (tag === 'button') return 'button';
                return 'other';
            }};

            const getText = (el) => (el.textContent || '').trim().substring(0, 200);

            const getAttrs = (el) => {{
                const attrs = {{}};
                for (const name of ['href', 'placeholder', 'aria-label', 'aria-readonly', 'type', 'name', 'value', 'role', 'title', 'alt', 'id', 'data-testid', 'data-test-id', 'data-test', 'contenteditable']) {{
                    const val = el.getAttribute(name);
                    if (val != null && val !== '') attrs[name] = val;
                }}
                if (el.isContentEditable && !('contenteditable' in attrs)) {{
                    attrs.contenteditable = 'true';
                }}
                if (el.hasAttribute && el.hasAttribute('disabled')) {{
                    attrs.disabled = '';
                }}
                if (el.hasAttribute && el.hasAttribute('readonly')) {{
                    attrs.readonly = '';
                }}
                return attrs;
            }};

            const getRect = (el) => {{
                const r = el.getBoundingClientRect();
                let x = r.x;
                let y = r.y;
                let current = window;
                while (current !== current.top) {{
                    try {{
                        const frameEl = current.frameElement;
                        if (!frameEl) break;
                        const fr = frameEl.getBoundingClientRect();
                        x += fr.x;
                        y += fr.y;
                        current = current.parent;
                    }} catch (_) {{
                        break;
                    }}
                }}
                return {{ x, y, width: r.width, height: r.height }};
            }};

            try {{
                const roots = resolveLocatorMatches(scope);
                const selectedRoots = selectMatches(roots, scope.selection);
                const elementDepth = (el) => {{
                    let depth = 0;
                    let current = el;
                    while (current && current.parentElement) {{
                        depth += 1;
                        current = current.parentElement;
                    }}
                    return depth;
                }};

                const matchEntries = [];
                const rootDepths = selectedRoots.map((root) => ({{
                    root,
                    depth: elementDepth(root),
                }}));
                const walker = document.createTreeWalker(
                    document.body || document.documentElement,
                    NodeFilter.SHOW_ELEMENT,
                    null
                );

                let node = walker.currentNode;
                let interactiveIndex = 0;
                while (node) {{
                    if (node.nodeType === 1) {{
                        const listeners = getListeners(node);
                        if (isInteractive(node) || listeners.length > 0) {{
                            let relativeDepth = null;
                            for (const {{ root, depth }} of rootDepths) {{
                                if (root === node || root.contains(node)) {{
                                    const candidate = Math.max(0, elementDepth(node) - depth);
                                    relativeDepth = relativeDepth === null
                                        ? candidate
                                        : Math.min(relativeDepth, candidate);
                                }}
                            }}
                            if (relativeDepth !== null) {{
                                matchEntries.push({{
                                    index: interactiveIndex,
                                    depth: relativeDepth,
                                    element: {{
                                        index: interactiveIndex,
                                        tag: getTag(node),
                                        text: getText(node),
                                        attributes: getAttrs(node),
                                        element_ref: null,
                                        bounding_box: getRect(node),
                                        ax_info: null,
                                        listeners: includeListeners ? listeners : null,
                                        depth: null
                                    }}
                                }});
                            }}
                            interactiveIndex++;
                        }}
                    }}
                    node = walker.nextNode();
                }}

                return {{
                    scope_error: null,
                    root_match_count: roots.length,
                    selected_root_count: selectedRoots.length,
                    match_entries: matchEntries,
                }};
            }} catch (error) {{
                return {{
                    scope_error: String((error && error.message) || error || 'invalid scope'),
                    root_match_count: 0,
                    selected_root_count: 0,
                    match_entries: [],
                }};
            }}
        }})())"#,
        if include_listeners { "true" } else { "false" },
    )
}

fn observation_scope_replay_error(
    snapshot: &Snapshot,
    scope: &ObservationScope,
    root_match_count: usize,
    missing_indices: &[u32],
    mismatched_indices: &[u32],
) -> RubError {
    let authority_state = if snapshot.truncated {
        "observation_scope_replay_truncated_cached_inventory"
    } else {
        "observation_scope_replay_classifier_mismatch"
    };
    let message = if snapshot.truncated {
        "Observation scope matched elements outside the cached snapshot inventory, so scoped replay cannot stay authoritative on a truncated snapshot"
    } else {
        "Observation scope no longer aligns with the cached snapshot interactive inventory, so scoped replay cannot choose an authoritative target set"
    };
    RubError::domain_with_context_and_suggestion(
        ErrorCode::StaleSnapshot,
        message,
        serde_json::json!({
            "scope": scope,
            "frame_id": snapshot.frame_context.frame_id,
            "snapshot_id": snapshot.snapshot_id,
            "snapshot_truncated": snapshot.truncated,
            "root_match_count": root_match_count,
            "missing_indexes": missing_indices,
            "mismatched_indexes": mismatched_indices,
            "authority_state": authority_state,
        }),
        "Run 'rub observe' again to refresh the snapshot before applying a scoped observation",
    )
}

fn resolve_snapshot_observation_scope_matches(
    snapshot: &Snapshot,
    scope: &ObservationScope,
    root_match_count: usize,
    match_entries: Vec<ObservationScopeMatchEntry>,
) -> Result<Vec<(u32, Element)>, RubError> {
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
        return Err(observation_scope_replay_error(
            snapshot,
            scope,
            root_match_count,
            &missing_indices,
            &mismatched_indices,
        ));
    }

    let mut resolved = crate::snapshot_lookup::clone_snapshot_elements_by_index(
        &snapshot_by_index,
        match_entries.iter().map(|matched| matched.index),
    );
    let depth_by_index = match_entries
        .into_iter()
        .map(|matched| (matched.index, matched.depth))
        .collect::<std::collections::HashMap<_, _>>();
    for (index, element) in &mut resolved {
        if let Some(depth) = depth_by_index.get(index) {
            element.depth = Some(*depth);
        }
    }

    Ok(resolved)
}

pub(crate) async fn find_snapshot_elements_in_observation_scope(
    page: &Arc<Page>,
    snapshot: &Snapshot,
    scope: &ObservationScope,
) -> Result<(Vec<Element>, u32), RubError> {
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
    let scope_json = serde_json::to_string(scope)
        .map_err(|error| RubError::Internal(format!("Scope serialization failed: {error}")))?;
    let script = observation_scope_script(
        &scope_json,
        snapshot_uses_listener_promoted_classifier(snapshot),
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
        "observation_scope",
        snapshot.frame_context.frame_id.as_str(),
        document_before.as_ref(),
        document_after.as_ref(),
    )?;
    let payload: ObservationScopePayload = serde_json::from_str(&raw)
        .map_err(|error| RubError::Internal(format!("Scope payload parse failed: {error}")))?;

    if let Some(scope_error) = payload.scope_error {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("Invalid observation scope: {scope_error}"),
            serde_json::json!({
                "scope": scope,
                "frame_id": snapshot.frame_context.frame_id,
            }),
        ));
    }

    if payload.selected_root_count == 0 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            "Observation scope did not resolve to any content root",
            serde_json::json!({
                "scope": scope,
                "root_match_count": payload.root_match_count,
            }),
            "Verify the scope locator matches a visible element on the current page. Run 'rub observe' without --scope to see all available elements",
        ));
    }

    let mut resolved = resolve_snapshot_observation_scope_matches(
        snapshot,
        scope,
        payload.root_match_count,
        payload.match_entries,
    )?;

    if resolved.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            "Observation scope resolved no interactive snapshot descendants",
            serde_json::json!({
                "scope": scope,
                "root_match_count": payload.root_match_count,
            }),
            "The scope root was found but contains no interactive elements. Try a broader scope or run 'rub observe' to see the full page",
        ));
    }

    resolved.sort_by_key(|(index, _)| *index);
    Ok((
        resolved.into_iter().map(|(_, element)| element).collect(),
        payload.root_match_count as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ObservationScopeMatchEntry, find_snapshot_elements_in_observation_scope,
        observation_scope_script, resolve_snapshot_observation_scope_matches,
    };
    use rub_core::error::ErrorCode;
    use rub_core::model::{
        Element, ElementTag, FrameContextInfo, ScrollPosition, Snapshot, SnapshotProjection,
    };
    use rub_core::observation::{ObservationScope, ObservationSelection};
    use std::collections::HashMap;

    #[test]
    fn observation_scope_script_serializes_semantic_scope_and_selection() {
        let scope = ObservationScope::Role {
            role: "main".to_string(),
            selection: Some(ObservationSelection::First),
        };
        let serialized = serde_json::to_string(&scope).unwrap();
        assert!(serialized.contains("\"kind\":\"role\""));
        assert!(serialized.contains("\"role\":\"main\""));
        assert!(serialized.contains("\"selection\":\"first\""));
        let _ = find_snapshot_elements_in_observation_scope;
    }

    #[test]
    fn observation_scope_script_uses_snapshot_classifier_extensions() {
        let script = observation_scope_script(
            &serde_json::to_string(&ObservationScope::Role {
                role: "main".to_string(),
                selection: None,
            })
            .expect("scope should serialize"),
            true,
        );
        assert!(script.contains("if (el.isContentEditable) return true;"));
        assert!(script.contains("const includeListeners = true;"));
        assert!(script.contains("if (isInteractive(node) || listeners.length > 0)"));
    }

    #[test]
    fn observation_scope_replay_fails_closed_on_missing_snapshot_index() {
        let error = resolve_snapshot_observation_scope_matches(
            &sample_snapshot(false),
            &ObservationScope::Role {
                role: "main".to_string(),
                selection: None,
            },
            1,
            vec![sample_match_entry(9, 0, "Ghost")],
        )
        .expect_err("missing scoped replay index should fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("authority_state"))
                .and_then(|value| value.as_str()),
            Some("observation_scope_replay_classifier_mismatch")
        );
    }

    #[test]
    fn observation_scope_replay_fails_closed_on_truncated_snapshot_gap() {
        let error = resolve_snapshot_observation_scope_matches(
            &sample_snapshot(true),
            &ObservationScope::Role {
                role: "main".to_string(),
                selection: None,
            },
            1,
            vec![sample_match_entry(9, 0, "Ghost")],
        )
        .expect_err("truncated scoped replay gap should fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("authority_state"))
                .and_then(|value| value.as_str()),
            Some("observation_scope_replay_truncated_cached_inventory")
        );
    }

    #[test]
    fn observation_scope_replay_fails_closed_on_in_range_identity_mismatch() {
        let error = resolve_snapshot_observation_scope_matches(
            &sample_snapshot(false),
            &ObservationScope::Role {
                role: "main".to_string(),
                selection: None,
            },
            1,
            vec![sample_match_entry(1, 0, "Inserted")],
        )
        .expect_err("in-range scoped replay mismatch should fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|context| context.get("mismatched_indexes"))
                .cloned(),
            Some(serde_json::json!([1]))
        );
    }

    fn sample_snapshot(truncated: bool) -> Snapshot {
        Snapshot {
            snapshot_id: "scope-snap".to_string(),
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
            total_count: 2,
            viewport_count: Some(2),
            truncated,
            elements: vec![sample_element(1, "First"), sample_element(2, "Second")],
            scroll: ScrollPosition {
                x: 0.0,
                y: 0.0,
                at_bottom: false,
            },
            timestamp: "2026-04-18T00:00:00Z".to_string(),
            projection: SnapshotProjection {
                verified: true,
                js_traversal_count: 2,
                backend_traversal_count: 2,
                resolved_ref_count: 2,
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

    fn sample_match_entry(index: u32, depth: u32, text: &str) -> ObservationScopeMatchEntry {
        ObservationScopeMatchEntry {
            index,
            depth,
            element: sample_element(index, text),
        }
    }
}
