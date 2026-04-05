use chromiumoxide::Page;
use chromiumoxide::cdp::js_protocol::runtime::{ExecutionContextId, RemoteObjectId};
use rub_core::error::{ErrorCode, RubError};
use std::sync::Arc;

pub(crate) async fn ensure_active_text_target_editable(page: &Arc<Page>) -> Result<(), RubError> {
    let state = crate::js::evaluate_returning_string(
        page,
        r#"(function() {
            const el = document.activeElement;
            if (!el) return 'NO_ACTIVE';
            const tag = String(el.tagName || '').toLowerCase();
            if (el.isContentEditable) return 'OK';
            const inputType = tag === 'input' ? String(el.getAttribute('type') || '').toLowerCase() : '';
            const textLikeInput =
                tag === 'input'
                && !['checkbox', 'radio', 'file', 'submit', 'button', 'reset', 'image', 'color', 'range', 'hidden'].includes(inputType);
            const editable = tag === 'textarea' || textLikeInput;
            if (!editable) return 'NOT_EDITABLE';
            if (typeof el.disabled === 'boolean' && el.disabled) return 'DISABLED';
            if (typeof el.readOnly === 'boolean' && el.readOnly) return 'READONLY';
            return 'OK';
        })()"#,
    )
    .await?;

    match state.as_str() {
        "DISABLED" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Active element is disabled",
        )),
        "READONLY" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Active element is readonly",
        )),
        "NOT_EDITABLE" | "NO_ACTIVE" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Active element is not an editable text target",
        )),
        _ => Ok(()),
    }
}

pub(crate) async fn ensure_active_text_target_editable_in_context(
    page: &Arc<Page>,
    context_id: Option<ExecutionContextId>,
) -> Result<(), RubError> {
    let state = crate::js::evaluate_returning_string_in_context(
        page,
        context_id,
        r#"(function() {
            const el = document.activeElement;
            if (!el) return 'NO_ACTIVE';
            const tag = String(el.tagName || '').toLowerCase();
            if (el.isContentEditable) return 'OK';
            const inputType = tag === 'input' ? String(el.getAttribute('type') || '').toLowerCase() : '';
            const textLikeInput =
                tag === 'input'
                && !['checkbox', 'radio', 'file', 'submit', 'button', 'reset', 'image', 'color', 'range', 'hidden'].includes(inputType);
            const editable = tag === 'textarea' || textLikeInput;
            if (!editable) return 'NOT_EDITABLE';
            if (typeof el.disabled === 'boolean' && el.disabled) return 'DISABLED';
            if (typeof el.readOnly === 'boolean' && el.readOnly) return 'READONLY';
            return 'OK';
        })()"#,
    )
    .await?;

    match state.as_str() {
        "DISABLED" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Active element is disabled",
        )),
        "READONLY" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Active element is readonly",
        )),
        "NOT_EDITABLE" | "NO_ACTIVE" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Active element is not an editable text target",
        )),
        _ => Ok(()),
    }
}

pub(crate) async fn ensure_activation_target_enabled(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<(), RubError> {
    let state = crate::js::call_function_returning_string(
        page,
        object_id,
        r#"function() {
            const ariaDisabled = this.getAttribute && this.getAttribute('aria-disabled') === 'true';
            const disabledFieldset =
                typeof this.closest === 'function' ? this.closest('fieldset[disabled]') : null;
            const disabledAncestor =
                typeof this.closest === 'function' ? this.closest('[aria-disabled="true"]') : null;
            if (typeof this.disabled === 'boolean' && this.disabled) return 'DISABLED';
            if (ariaDisabled) return 'ARIA_DISABLED';
            if (disabledFieldset) return 'FIELDSET_DISABLED';
            if (disabledAncestor && disabledAncestor !== this) return 'ARIA_DISABLED';
            return 'OK';
        }"#,
    )
    .await?;

    match state.as_str() {
        "DISABLED" | "FIELDSET_DISABLED" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is disabled",
        )),
        "ARIA_DISABLED" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is aria-disabled",
        )),
        _ => Ok(()),
    }
}

pub(crate) async fn prepare_text_input(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    clear: bool,
) -> Result<(), RubError> {
    let js = if clear {
        r#"function() {
            this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
            this.focus();
            if (typeof this.select === 'function') {
                this.select();
                return 'selected';
            }
            if (this.isContentEditable) {
                const range = document.createRange();
                range.selectNodeContents(this);
                const sel = window.getSelection();
                if (sel) {
                    sel.removeAllRanges();
                    sel.addRange(range);
                }
                return 'selected';
            }
            return 'focused';
        }"#
    } else {
        r#"function() {
            this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
            this.focus();
            return 'focused';
        }"#
    };
    call_function(page, object_id, js, true).await
}

pub(crate) async fn clear_text_input(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<(), RubError> {
    call_function(
        page,
        object_id,
        r#"function() {
            if ('value' in this) {
                this.value = '';
                this.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward', data: null }));
                this.dispatchEvent(new Event('change', { bubbles: true }));
                return 'cleared';
            }
            if (this.isContentEditable) {
                this.textContent = '';
                this.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward', data: null }));
                this.dispatchEvent(new Event('change', { bubbles: true }));
                return 'cleared';
            }
            return 'noop';
        }"#,
        true,
    )
    .await
}

async fn call_function(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
    await_promise: bool,
) -> Result<(), RubError> {
    crate::js::call_function(page, object_id, function_declaration, await_promise).await
}
