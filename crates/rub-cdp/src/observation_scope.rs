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
    let scope_json = serde_json::to_string(scope)
        .map_err(|error| RubError::Internal(format!("Scope serialization failed: {error}")))?;
    let script = format!(
        r#"JSON.stringify((() => {{
            const scope = {scope_json};
            {LOCATOR_JS_HELPERS}
            const interactiveTags = new Set(['a', 'button', 'input', 'textarea', 'select', 'option']);
            const interactiveRoles = new Set([
                'button', 'link', 'menuitem', 'tab', 'checkbox', 'radio',
                'switch', 'textbox', 'combobox', 'listbox', 'option'
            ]);

            const isInteractive = (el) => {{
                const tag = String(el.tagName || '').toLowerCase();
                if (interactiveTags.has(tag)) return true;
                const role = el.getAttribute && el.getAttribute('role');
                if (role && interactiveRoles.has(role)) return true;
                if (el.onclick || (el.hasAttribute && el.hasAttribute('onclick'))) return true;
                if (el.getAttribute && el.getAttribute('tabindex') !== null) return true;
                return false;
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
                    if (node.nodeType === 1 && isInteractive(node)) {{
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
                            matchEntries.push({{ index: interactiveIndex, depth: relativeDepth }});
                        }}
                        interactiveIndex++;
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
    );
    let payload: ObservationScopePayload = serde_json::from_str(
        &crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            &script,
        )
        .await?,
    )
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

    let snapshot_by_index = crate::snapshot_lookup::build_snapshot_index_lookup(snapshot);
    let mut resolved = crate::snapshot_lookup::clone_snapshot_elements_by_index(
        &snapshot_by_index,
        payload.match_entries.iter().map(|matched| matched.index),
    );
    let depth_by_index = payload
        .match_entries
        .into_iter()
        .map(|matched| (matched.index, matched.depth))
        .collect::<std::collections::HashMap<_, _>>();
    for (index, element) in &mut resolved {
        if let Some(depth) = depth_by_index.get(index) {
            element.depth = Some(*depth);
        }
    }

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
    use super::find_snapshot_elements_in_observation_scope;
    use rub_core::observation::{ObservationScope, ObservationSelection};

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
}
