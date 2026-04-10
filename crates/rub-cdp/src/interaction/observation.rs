use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::BackendNodeId;
use chromiumoxide::cdp::js_protocol::runtime::{ExecutionContextId, RemoteObjectId};
use rub_core::error::RubError;
use serde::Deserialize;
use std::sync::Arc;
use tokio::time::Duration;

const OBSERVATION_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub(crate) struct ElementObservation {
    pub(crate) checked: Option<bool>,
    pub(crate) hovered: bool,
    pub(crate) value: Option<String>,
    pub(crate) selected_text: Option<String>,
    pub(crate) file_names: Option<Vec<String>>,
    pub(crate) text: Option<String>,
    pub(crate) disabled: Option<bool>,
    pub(crate) open: Option<bool>,
    pub(crate) aria_expanded: Option<String>,
    pub(crate) aria_pressed: Option<String>,
    pub(crate) aria_selected: Option<String>,
    pub(crate) active: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct InteractionBaseline {
    pub(crate) before_element: Option<ElementObservation>,
    pub(crate) before_page: PageObservation,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveInteractionBaseline {
    pub(crate) before_active: Option<ActiveElementObservation>,
    pub(crate) before_page: PageObservation,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveElementObservation {
    pub(crate) backend_node_id: Option<BackendNodeId>,
    pub(crate) identity: ActiveElementIdentity,
    pub(crate) observation: ElementObservation,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ActiveElementIdentity {
    pub(crate) tag_name: String,
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) input_type: Option<String>,
    pub(crate) role: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct PageObservation {
    pub(crate) available: bool,
    pub(crate) url: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) element_count: Option<u32>,
    pub(crate) text_hash: Option<u64>,
    pub(crate) text_length: Option<u32>,
    pub(crate) markup_hash: Option<u64>,
    pub(crate) context_replaced: bool,
}

#[derive(Debug, Deserialize)]
struct PageProbe {
    url: Option<String>,
    title: Option<String>,
    element_count: Option<u32>,
    text_hash: Option<u64>,
    text_length: Option<u32>,
    markup_hash: Option<u64>,
}

pub(crate) async fn capture_interaction_baseline(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> InteractionBaseline {
    InteractionBaseline {
        before_element: observe_element(page, object_id).await.ok(),
        before_page: observe_related_page(page, object_id).await,
    }
}

pub(crate) async fn capture_page_baseline(page: &Arc<Page>) -> PageObservation {
    observe_page(page).await
}

pub(crate) async fn capture_related_page_baseline(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> PageObservation {
    observe_related_page(page, object_id).await
}

pub(crate) async fn capture_active_interaction_baseline(
    page: &Arc<Page>,
) -> ActiveInteractionBaseline {
    capture_active_interaction_baseline_in_context(page, None).await
}

pub(crate) async fn capture_active_interaction_baseline_in_context(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
) -> ActiveInteractionBaseline {
    ActiveInteractionBaseline {
        before_active: observe_active_element_in_context(page, context_id)
            .await
            .ok(),
        before_page: observe_page(page).await,
    }
}

pub(crate) fn page_changed(before: &PageObservation, after: &PageObservation) -> bool {
    before.available
        && after.available
        && (before.context_replaced != after.context_replaced
            || before.url != after.url
            || before.title != after.title)
}

pub(crate) fn page_mutated(before: &PageObservation, after: &PageObservation) -> bool {
    before.available
        && after.available
        && (before.element_count != after.element_count
            || before.text_hash != after.text_hash
            || before.text_length != after.text_length
            || before.markup_hash != after.markup_hash)
}

pub(crate) fn element_state_changed(
    before: &ElementObservation,
    after: &ElementObservation,
) -> bool {
    before.value != after.value
        || before.text != after.text
        || before.disabled != after.disabled
        || before.open != after.open
        || before.aria_expanded != after.aria_expanded
        || before.aria_pressed != after.aria_pressed
        || before.aria_selected != after.aria_selected
}

pub(crate) fn typed_effect_observed(
    before: &ElementObservation,
    after: &ElementObservation,
    typed_text: &str,
) -> bool {
    if typed_text.is_empty() {
        return false;
    }

    if let (Some(before_value), Some(after_value)) = (&before.value, &after.value)
        && before_value != after_value
    {
        return after_value.contains(typed_text);
    }

    if let (Some(before_text), Some(after_text)) = (&before.text, &after.text)
        && before_text != after_text
    {
        return after_text.contains(typed_text);
    }

    false
}

pub(crate) fn typed_effect_contradicted(
    before: &ElementObservation,
    after: &ElementObservation,
    typed_text: &str,
) -> bool {
    if typed_effect_observed(before, after, typed_text) {
        return false;
    }

    matches!(
        (&before.value, &after.value),
        (Some(before_value), Some(after_value)) if before_value != after_value
    ) || matches!(
        (&before.text, &after.text),
        (Some(before_text), Some(after_text)) if before_text != after_text
    )
}

pub(crate) async fn observe_element(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<ElementObservation, RubError> {
    let value = tokio::time::timeout(
        OBSERVATION_PROBE_TIMEOUT,
        crate::js::call_function_returning_value(
            page,
            object_id,
            r#"function() {
                return {
                    checked: typeof this.checked === 'boolean' ? this.checked : null,
                    hovered: typeof this.matches === 'function' ? this.matches(':hover') : false,
                    value: 'value' in this ? String(this.value ?? '') : null,
                    selected_text: this.tagName === 'SELECT' && this.selectedOptions && this.selectedOptions[0]
                        ? String(this.selectedOptions[0].text ?? '')
                        : null,
                    file_names: this.files ? Array.from(this.files).map(file => String(file.name || '')) : null,
                    text: (this.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 200) || null,
                    disabled: typeof this.disabled === 'boolean' ? this.disabled : null,
                    open: typeof this.open === 'boolean' ? this.open : null,
                    aria_expanded: this.getAttribute ? this.getAttribute('aria-expanded') : null,
                    aria_pressed: this.getAttribute ? this.getAttribute('aria-pressed') : null,
                    aria_selected: this.getAttribute ? this.getAttribute('aria-selected') : null,
                    active: document.activeElement === this
                };
            }"#,
        ),
    )
    .await
    .map_err(|_| RubError::Internal("Element observation timed out".to_string()))??;
    serde_json::from_value(value)
        .map_err(|e| RubError::Internal(format!("Element observation parse failed: {e}")))
}

pub(crate) async fn observe_active_element(
    page: &Arc<Page>,
) -> Result<ActiveElementObservation, RubError> {
    let object_id = crate::js::evaluate_returning_object_id(page, "document.activeElement").await?;
    let backend_node_id = crate::targeting::backend_node_id_for_object(page, &object_id)
        .await
        .ok();
    let identity = observe_active_element_identity(page, &object_id).await?;
    let observation = observe_element(page, &object_id).await?;
    Ok(ActiveElementObservation {
        backend_node_id,
        identity,
        observation,
    })
}

pub(crate) async fn observe_active_element_in_context(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
) -> Result<ActiveElementObservation, RubError> {
    let object_id = crate::js::evaluate_returning_object_id_in_context(
        page,
        context_id,
        "document.activeElement",
    )
    .await?;
    let backend_node_id = crate::targeting::backend_node_id_for_object(page, &object_id)
        .await
        .ok();
    let identity = observe_active_element_identity(page, &object_id).await?;
    let observation = observe_element(page, &object_id).await?;
    Ok(ActiveElementObservation {
        backend_node_id,
        identity,
        observation,
    })
}

async fn observe_active_element_identity(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<ActiveElementIdentity, RubError> {
    let value = tokio::time::timeout(
        OBSERVATION_PROBE_TIMEOUT,
        crate::js::call_function_returning_value(
            page,
            object_id,
            r#"function() {
                return {
                    tag_name: String(this.tagName || '').toLowerCase(),
                    id: this.id ? String(this.id) : null,
                    name: this.getAttribute ? (this.getAttribute('name') || null) : null,
                    input_type: this.getAttribute ? (this.getAttribute('type') || null) : null,
                    role: this.getAttribute ? (this.getAttribute('role') || null) : null,
                };
            }"#,
        ),
    )
    .await
    .map_err(|_| {
        RubError::Internal("Active element identity observation timed out".to_string())
    })??;
    serde_json::from_value(value)
        .map_err(|e| RubError::Internal(format!("Active element identity parse failed: {e}")))
}

pub(crate) fn active_element_matches(
    before: &ActiveElementObservation,
    after: &ActiveElementObservation,
) -> bool {
    if let (Some(before_backend), Some(after_backend)) =
        (before.backend_node_id, after.backend_node_id)
    {
        return before_backend == after_backend;
    }

    before.identity == after.identity
}

pub(crate) fn active_element_changed(
    before: &ActiveElementObservation,
    after: &ActiveElementObservation,
) -> bool {
    !active_element_matches(before, after)
}

pub(crate) async fn observe_page(page: &Arc<Page>) -> PageObservation {
    let probe_result = tokio::time::timeout(
        OBSERVATION_PROBE_TIMEOUT,
        page.evaluate(
            r#"(() => {
                const normalize = (value) => String(value || '')
                    .replace(/\s+/g, ' ')
                    .trim();
                const hash = (value) => {
                    let h = 2166136261 >>> 0;
                    for (let i = 0; i < value.length; i++) {
                        h ^= value.charCodeAt(i);
                        h = Math.imul(h, 16777619) >>> 0;
                    }
                    return h >>> 0;
                };
                const root = document.body || document.documentElement;
                const normalizedText = normalize(
                    (root && (root.innerText || root.textContent)) || ''
                );
                return {
                    url: location.href,
                    title: document.title,
                    element_count: document.querySelectorAll('*').length,
                    text_hash: hash(normalizedText),
                    text_length: normalizedText.length,
                    markup_hash: hash((document.documentElement && document.documentElement.outerHTML) || '')
                };
            })()"#,
        ),
    )
    .await;

    let (probe, context_replaced) = match probe_result {
        Ok(Ok(value)) => (value.into_value::<PageProbe>().ok(), false),
        Ok(Err(err)) => {
            let message = err.to_string();
            (None, is_context_replaced_error(&message))
        }
        Err(_) => (None, false),
    };

    page_observation_from_probe(probe, context_replaced)
}

pub(crate) async fn observe_related_page(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> PageObservation {
    let probe_result = tokio::time::timeout(
        OBSERVATION_PROBE_TIMEOUT,
        crate::js::call_function_returning_value(
            page,
            object_id,
            r#"function() {
                const normalize = (value) => String(value || '')
                    .replace(/\s+/g, ' ')
                    .trim();
                const hash = (value) => {
                    let h = 2166136261 >>> 0;
                    for (let i = 0; i < value.length; i++) {
                        h ^= value.charCodeAt(i);
                        h = Math.imul(h, 16777619) >>> 0;
                    }
                    return h >>> 0;
                };
                const doc = this.ownerDocument || document;
                const root = doc.body || doc.documentElement;
                const normalizedText = normalize(
                    (root && (root.innerText || root.textContent)) || ''
                );
                return {
                    url: doc.location ? doc.location.href : location.href,
                    title: doc.title,
                    element_count: doc.querySelectorAll('*').length,
                    text_hash: hash(normalizedText),
                    text_length: normalizedText.length,
                    markup_hash: hash((doc.documentElement && doc.documentElement.outerHTML) || '')
                };
            }"#,
        ),
    )
    .await;
    let (probe, context_replaced) = match probe_result {
        Ok(Ok(value)) => (serde_json::from_value::<PageProbe>(value).ok(), false),
        Ok(Err(err)) => {
            let message = err.to_string();
            (None, is_context_replaced_error(&message))
        }
        Err(_) => (None, false),
    };

    page_observation_from_probe(probe, context_replaced)
}

fn page_observation_from_probe(
    probe: Option<PageProbe>,
    context_replaced: bool,
) -> PageObservation {
    PageObservation {
        available: probe.is_some(),
        url: probe.as_ref().and_then(|value| value.url.clone()),
        title: probe.as_ref().and_then(|value| value.title.clone()),
        element_count: probe.as_ref().and_then(|value| value.element_count),
        text_hash: probe.as_ref().and_then(|value| value.text_hash),
        text_length: probe.as_ref().and_then(|value| value.text_length),
        markup_hash: probe.as_ref().and_then(|value| value.markup_hash),
        context_replaced,
    }
}

pub(crate) fn is_context_replaced_error(message: &str) -> bool {
    message.contains("Cannot find context with specified id")
        || message.contains("Execution context was destroyed")
        || message.contains("Inspected target navigated or closed")
}

pub(crate) fn confirmation_observation_degraded(
    before_page: &PageObservation,
    after_page: &PageObservation,
    before_element_available: bool,
    after_element_available: bool,
) -> bool {
    !before_page.available
        || !after_page.available
        || !before_element_available
        || !after_element_available
}
