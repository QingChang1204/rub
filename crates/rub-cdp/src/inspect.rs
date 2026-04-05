use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{BoundingBox, Element, Snapshot};
use std::collections::HashMap;
use std::sync::Arc;

const MATCH_INTERACTIVE_SELECTOR_JS_TEMPLATE: &str = r##"
JSON.stringify((() => {
    const selector = __SELECTOR__;
    const interactiveTags = new Set(['a', 'button', 'input', 'textarea', 'select', 'option']);
    const interactiveRoles = new Set([
        'button', 'link', 'menuitem', 'tab', 'checkbox', 'radio',
        'switch', 'textbox', 'combobox', 'listbox', 'option'
    ]);

    function isInteractive(el) {
        const tag = el.tagName.toLowerCase();
        if (interactiveTags.has(tag)) return true;
        const role = el.getAttribute('role');
        if (role && interactiveRoles.has(role)) return true;
        if (el.onclick || el.hasAttribute('onclick')) return true;
        if (el.getAttribute('tabindex') !== null) return true;
        return false;
    }

    try {
        document.querySelector(selector);
    } catch (error) {
        return {
            selector_error: String((error && error.message) || error || 'invalid selector'),
            match_indices: []
        };
    }

    const matchIndices = [];
    const walker = document.createTreeWalker(
        document.body || document.documentElement,
        NodeFilter.SHOW_ELEMENT,
        null
    );

    let node = walker.currentNode;
    let interactiveIndex = 0;
    while (node) {
        if (node.nodeType === 1 && isInteractive(node)) {
            if (node.matches(selector)) {
                matchIndices.push(interactiveIndex);
            }
            interactiveIndex++;
        }
        node = walker.nextNode();
    }

    return {
        selector_error: null,
        match_indices: matchIndices
    };
})())
"##;

#[derive(Debug, serde::Deserialize)]
struct SelectorMatchPayload {
    selector_error: Option<String>,
    match_indices: Vec<u32>,
}

#[derive(Debug, serde::Deserialize)]
struct HtmlQueryPayload {
    selector_error: Option<String>,
    html: Option<String>,
}

fn selector_recovery_suggestion() -> &'static str {
    "Check the CSS selector syntax, or switch to --role/--label/--testid. Run 'rub inspect page --format compact' to inspect nearby content"
}

fn selector_not_found_suggestion() -> &'static str {
    "Verify the selector in the current frame, or switch to --role/--label/--testid for a more stable locator. Run 'rub inspect page --format compact' to inspect nearby content"
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
        "function() { const r = this.getBoundingClientRect(); return { x: r.x || 0, y: r.y || 0, width: r.width || 0, height: r.height || 0 }; }",
    )
    .await?;
    serde_json::from_value::<BoundingBox>(bbox_json)
        .map_err(|e| RubError::Internal(format!("Parse bbox failed: {e}")))
}

pub(crate) async fn find_snapshot_elements_by_selector(
    page: &Arc<Page>,
    snapshot: &Snapshot,
    selector: &str,
) -> Result<Vec<Element>, RubError> {
    resolve_selector_matches(
        page,
        snapshot,
        selector,
        MATCH_INTERACTIVE_SELECTOR_JS_TEMPLATE,
        "Selector",
    )
    .await
}

async fn resolve_selector_matches(
    page: &Arc<Page>,
    snapshot: &Snapshot,
    selector: &str,
    template: &str,
    error_label: &str,
) -> Result<Vec<Element>, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(
        page,
        Some(snapshot.frame_context.frame_id.as_str()),
    )
    .await?;
    let selector_json = serde_json::to_string(selector)
        .map_err(|e| RubError::Internal(format!("Selector serialization failed: {e}")))?;
    let script = template.replace("__SELECTOR__", &selector_json);
    let payload: SelectorMatchPayload = serde_json::from_str(
        &crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            &script,
        )
        .await?,
    )
    .map_err(|e| RubError::Internal(format!("Selector payload parse failed: {e}")))?;

    if let Some(selector_error) = payload.selector_error {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            format!("Invalid selector: {selector_error}"),
            serde_json::json!({
                "selector": selector,
                "kind": error_label,
                "frame_id": snapshot.frame_context.frame_id,
            }),
            selector_recovery_suggestion(),
        ));
    }

    let mut snapshot_by_index = HashMap::new();
    for element in &snapshot.elements {
        snapshot_by_index.insert(element.index, element);
    }

    let mut resolved = Vec::new();
    for matched_index in payload.match_indices {
        if let Some(element) = snapshot_by_index.get(&matched_index) {
            resolved.push((element.index, (*element).clone()));
        }
    }

    if resolved.is_empty() {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            format!(
                "{error_label} '{selector}' did not resolve to an interactive snapshot element"
            ),
            serde_json::json!({
                "selector": selector,
                "kind": error_label,
                "snapshot_id": snapshot.snapshot_id,
            }),
            selector_not_found_suggestion(),
        ));
    }

    resolved.sort_by_key(|(index, _)| *index);
    Ok(resolved.into_iter().map(|(_, element)| element).collect())
}

fn parse_attributes_json(raw: &str) -> Result<HashMap<String, String>, RubError> {
    serde_json::from_str(raw)
        .map_err(|e| RubError::Internal(format!("Parse attributes failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::parse_attributes_json;
    use rub_core::error::ErrorCode;

    #[test]
    fn parse_attributes_json_rejects_invalid_payload() {
        let error = parse_attributes_json("{").expect_err("invalid attributes should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InternalError);
    }
}
