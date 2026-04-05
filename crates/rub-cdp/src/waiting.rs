use chromiumoxide::Page;
use rub_core::error::{ErrorCode, RubError};
use rub_core::locator::CanonicalLocator;
use rub_core::model::{WaitCondition, WaitKind, WaitState};
use std::sync::Arc;
use tokio::time::{Duration, Instant};

const POLL_INTERVAL_MS: u64 = 100;
const INVALID_SELECTOR_SENTINEL: &str = "__rub_invalid_selector__";

pub(crate) async fn wait_for_condition(
    page: &Arc<Page>,
    condition: &WaitCondition,
) -> Result<(), RubError> {
    let deadline = Instant::now() + Duration::from_millis(condition.timeout_ms);
    let js_check = wait_check_script(condition)?;

    loop {
        let frame_context =
            match crate::frame_runtime::resolve_frame_context(page, condition.frame_id.as_deref())
                .await
            {
                Ok(frame_context) => frame_context,
                Err(error) => {
                    if frame_context_error_is_deterministic(&error) {
                        return Err(error);
                    }
                    if let Some(terminal) = classify_terminal_wait_error(error.to_string()) {
                        return Err(terminal);
                    }
                    if Instant::now() >= deadline {
                        return Err(RubError::domain(
                            ErrorCode::WaitTimeout,
                            format!("Wait condition not met within {}ms", condition.timeout_ms),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
                    continue;
                }
            };

        match crate::js::evaluate_returning_string_in_context(
            page,
            frame_context.execution_context_id,
            js_check.as_str(),
        )
        .await
        {
            Ok(result) => match serde_json::from_str::<bool>(&result) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    if serde_json::from_str::<String>(&result).ok().as_deref()
                        == Some(INVALID_SELECTOR_SENTINEL)
                    {
                        return Err(RubError::domain(
                            ErrorCode::InvalidInput,
                            "Invalid CSS selector in wait locator",
                        ));
                    }
                    return Err(RubError::domain_with_context(
                        ErrorCode::BrowserCrashed,
                        format!("Wait probe returned malformed result: {error}"),
                        serde_json::json!({
                            "reason": "wait_probe_malformed",
                            "frame_id": frame_context.frame.frame_id,
                        }),
                    ));
                }
            },
            Err(error) => {
                if let Some(terminal) = classify_terminal_wait_error(error.to_string()) {
                    return Err(terminal);
                }
                // The page may be navigating or briefly unavailable during poll.
            }
        }

        if Instant::now() >= deadline {
            return Err(RubError::domain(
                ErrorCode::WaitTimeout,
                format!("Wait condition not met within {}ms", condition.timeout_ms),
            ));
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

fn classify_terminal_wait_error(message: String) -> Option<RubError> {
    let normalized = message.to_ascii_lowercase();
    let reason = if normalized.contains("target closed")
        || normalized.contains("session closed")
        || normalized.contains("already closed")
        || normalized.contains("connection closed")
        || normalized.contains("browser has disconnected")
    {
        Some("wait_target_closed")
    } else {
        None
    }?;

    Some(RubError::domain_with_context(
        ErrorCode::BrowserCrashed,
        format!("Wait aborted because the page is no longer live: {message}"),
        serde_json::json!({
            "reason": reason,
            "phase": "wait_probe",
        }),
    ))
}

fn frame_context_error_is_deterministic(error: &RubError) -> bool {
    matches!(error, RubError::Domain(envelope) if envelope.code == ErrorCode::InvalidInput)
}

fn wait_check_script(condition: &WaitCondition) -> Result<String, RubError> {
    match &condition.kind {
        WaitKind::Locator { locator, state } => locator_wait_script(locator, *state),
        WaitKind::Text { text } => Ok(text_wait_script(text)),
    }
}

fn locator_wait_script(locator: &CanonicalLocator, state: WaitState) -> Result<String, RubError> {
    let locator = serde_json::to_string(locator).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize wait locator: {error}"),
        )
    })?;
    let state = serde_json::to_string(&state).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize wait state: {error}"),
        )
    })?;
    let invalid_selector = serde_json::to_string(INVALID_SELECTOR_SENTINEL).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Failed to serialize wait selector sentinel: {error}"),
        )
    })?;
    Ok(format!(
        r#"(() => {{
            const locator = {locator};
            const state = {state};
            const normalize = (value) => String(value ?? '')
                .replace(/\s+/g, ' ')
                .trim()
                .toLocaleLowerCase();
            const visibleText = (el) => normalize(el.innerText || el.textContent || '');
            const textish = (...values) => values
                .map((value) => normalize(value))
                .find((value) => value.length > 0) || '';
            const fallbackRole = (el) => {{
                const tag = String(el.tagName || '').toLowerCase();
                if (tag === 'button') return 'button';
                if (tag === 'a' && el.hasAttribute('href')) return 'link';
                if (tag === 'textarea') return 'textbox';
                if (tag === 'select') return 'combobox';
                if (tag === 'option') return 'option';
                if (tag === 'input') {{
                    const type = String(el.getAttribute('type') || '').toLowerCase();
                    if (type === 'checkbox') return 'checkbox';
                    if (type === 'radio') return 'radio';
                    if (type === 'button' || type === 'submit' || type === 'reset') return 'button';
                    return 'textbox';
                }}
                return '';
            }};
            const semanticRole = (el) => textish(el.getAttribute && el.getAttribute('role'), fallbackRole(el));
            const accessibleLabel = (el) => {{
                const labels = [];
                if (el.getAttribute) {{
                    labels.push(el.getAttribute('aria-label'));
                    labels.push(el.getAttribute('placeholder'));
                    labels.push(el.getAttribute('title'));
                    labels.push(el.getAttribute('alt'));
                    labels.push(el.getAttribute('value'));
                    labels.push(el.getAttribute('name'));
                }}
                const id = el.getAttribute && el.getAttribute('id');
                if (id) {{
                    const viaFor = document.querySelector(`label[for="${{CSS.escape(id)}}"]`);
                    if (viaFor) labels.push(viaFor.innerText || viaFor.textContent);
                }}
                if (typeof el.closest === 'function') {{
                    const viaClosest = el.closest('label');
                    if (viaClosest) labels.push(viaClosest.innerText || viaClosest.textContent);
                }}
                labels.push(el.innerText || el.textContent);
                return textish(...labels);
            }};
            const testingId = (el) => {{
                if (!el.getAttribute) return '';
                return textish(
                    el.getAttribute('data-testid'),
                    el.getAttribute('data-test-id'),
                    el.getAttribute('data-test')
                );
            }};
            const allElements = () => Array.from(document.querySelectorAll('*'));
            const pickSelection = (elements, selection) => {{
                if (!selection) return elements[0] || null;
                switch (selection) {{
                    case 'first':
                        return elements[0] || null;
                    case 'last':
                        return elements.length ? elements[elements.length - 1] : null;
                    default:
                        if (typeof selection === 'object' && selection !== null && Number.isInteger(selection.nth)) {{
                            return elements[selection.nth] || null;
                        }}
                        return elements[0] || null;
                }}
            }};
            const isVisible = (el) => {{
                if (!el) return false;
                const style = getComputedStyle(el);
                if (style.display === 'none' || style.visibility === 'hidden') return false;
                const rects = el.getClientRects();
                if (!rects || rects.length === 0) return false;
                const rect = el.getBoundingClientRect();
                return rect.width > 0 && rect.height > 0;
            }};
            const matches = (() => {{
                switch (locator.kind) {{
                    case 'selector':
                        try {{
                            return Array.from(document.querySelectorAll(locator.css));
                        }} catch (_error) {{
                            return {invalid_selector};
                        }}
                    case 'target_text': {{
                        const needle = normalize(locator.text);
                        if (!needle) return [];
                        return allElements().filter((el) => {{
                            const candidate = textish(
                                el.innerText,
                                el.textContent,
                                el.getAttribute && el.getAttribute('aria-label'),
                                el.getAttribute && el.getAttribute('title'),
                                el.getAttribute && el.getAttribute('placeholder')
                            );
                            return candidate.includes(needle);
                        }});
                    }}
                    case 'role': {{
                        const needle = normalize(locator.role);
                        if (!needle) return [];
                        return allElements().filter((el) => semanticRole(el) === needle);
                    }}
                    case 'label': {{
                        const needle = normalize(locator.label);
                        if (!needle) return [];
                        return allElements().filter((el) => {{
                            const candidate = accessibleLabel(el);
                            return candidate.length > 0 && candidate.includes(needle);
                        }});
                    }}
                    case 'test_id': {{
                        const needle = normalize(locator.testid);
                        if (!needle) return [];
                        return allElements().filter((el) => testingId(el) === needle);
                    }}
                    default:
                        return [];
                }}
            }})();
            const selected = pickSelection(matches, locator.selection);
            switch (state) {{
                case 'attached':
                    return JSON.stringify(selected !== null);
                case 'detached':
                    return JSON.stringify(selected === null);
                case 'hidden':
                    return JSON.stringify(selected === null || !isVisible(selected));
                case 'visible':
                default:
                    return JSON.stringify(selected !== null && isVisible(selected));
            }}
        }})()"#,
        invalid_selector = invalid_selector
    ))
}

fn text_wait_script(text: &str) -> String {
    let text_literal = serde_json::to_string(text).expect("json string serialization");
    format!(
        r#"(() => {{
            const normalize = (value) => String(value || '')
                .replace(/\s+/g, ' ')
                .trim()
                .toLocaleLowerCase();
            const body = document.body;
            const root = document.documentElement;
            const haystack = normalize(
                (body && body.innerText) ||
                (root && root.innerText) ||
                (body && body.textContent) ||
                (root && root.textContent) ||
                ''
            );
            const needle = normalize({text_literal});
            return JSON.stringify(needle.length > 0 && haystack.includes(needle));
        }})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::{
        INVALID_SELECTOR_SENTINEL, classify_terminal_wait_error,
        frame_context_error_is_deterministic, locator_wait_script, text_wait_script,
    };
    use rub_core::error::{ErrorCode, RubError};
    use rub_core::locator::{CanonicalLocator, LocatorSelection};
    use rub_core::model::WaitState;

    #[test]
    fn text_wait_script_normalizes_whitespace_and_case() {
        let script = text_wait_script("Enter   Account Information");
        assert!(script.contains(".replace(/\\s+/g, ' ')"));
        assert!(script.contains(".toLocaleLowerCase()"));
        assert!(script.contains("Enter   Account Information"));
    }

    #[test]
    fn text_wait_script_escapes_newlines_via_json_string_literal() {
        let script = text_wait_script("Line 1\nLine 2");
        assert!(script.contains("Line 1\\nLine 2"), "{script}");
        assert!(!script.contains("Line 1\nLine 2"), "{script}");
    }

    #[test]
    fn locator_visible_wait_script_checks_rendered_visibility() {
        let script = locator_wait_script(
            &CanonicalLocator::Selector {
                css: ".ready".to_string(),
                selection: Some(LocatorSelection::First),
            },
            WaitState::Visible,
        )
        .expect("selector locator should serialize");
        assert!(script.contains("getComputedStyle"));
        assert!(script.contains("getClientRects"));
        assert!(script.contains("style.display === 'none'"));
        assert!(script.contains("\"kind\":\"selector\""));
        assert!(script.contains("\"selection\":\"first\""));
    }

    #[test]
    fn selector_wait_script_marks_invalid_css_as_input_error_sentinel() {
        let script = locator_wait_script(
            &CanonicalLocator::Selector {
                css: "[".to_string(),
                selection: Some(LocatorSelection::First),
            },
            WaitState::Visible,
        )
        .expect("selector locator should serialize");
        assert!(script.contains(INVALID_SELECTOR_SENTINEL));
        assert!(script.contains("return "));
    }

    #[test]
    fn terminal_wait_error_classifier_maps_context_loss_and_target_close() {
        assert!(
            classify_terminal_wait_error(
                "Execution context was destroyed, most likely because of a navigation.".to_string(),
            )
            .is_none(),
            "context replacement should be retried rather than treated as terminal"
        );

        let target_closed = classify_terminal_wait_error("Target closed".to_string())
            .expect("target close should classify");
        assert_eq!(
            target_closed.into_envelope().code,
            ErrorCode::BrowserCrashed
        );

        assert!(classify_terminal_wait_error("temporary protocol hiccup".to_string()).is_none());
    }

    #[test]
    fn frame_context_invalid_input_is_not_retried() {
        let error = RubError::domain(
            ErrorCode::InvalidInput,
            "Frame 'child' is not present in the current frame inventory",
        );
        assert!(frame_context_error_is_deterministic(&error));
        assert!(!frame_context_error_is_deterministic(&RubError::domain(
            ErrorCode::BrowserCrashed,
            "transient context replacement",
        )));
    }
}
