use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
use rub_core::InteractionOutcome;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{Element, InteractionActuation, InteractionSemanticClass, SelectOutcome};
use std::sync::Arc;

use crate::humanize::HumanizeConfig;

pub(crate) async fn input_text(
    page: &Arc<Page>,
    element: &Element,
    text: &str,
    clear: bool,
    humanize: &HumanizeConfig,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    ensure_text_control_editable(page, &resolved.remote_object_id).await?;
    let before_page =
        crate::interaction::capture_related_page_baseline(page, &resolved.remote_object_id).await;

    crate::interaction::prepare_text_input(page, &resolved.remote_object_id, clear).await?;
    crate::keyboard::focus_pause(humanize).await;

    if clear && text.is_empty() {
        crate::interaction::clear_text_input(page, &resolved.remote_object_id).await?;
    } else {
        crate::keyboard::type_text(page, text, humanize).await?;
    }

    let confirmation =
        crate::interaction::confirm_input(page, &resolved.remote_object_id, text, before_page)
            .await;

    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::SetValue,
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Keyboard),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn type_into(
    page: &Arc<Page>,
    element: &Element,
    text: &str,
    clear: bool,
    humanize: &HumanizeConfig,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    ensure_text_control_editable(page, &resolved.remote_object_id).await?;
    let before_page =
        crate::interaction::capture_related_page_baseline(page, &resolved.remote_object_id).await;

    crate::interaction::prepare_text_input(page, &resolved.remote_object_id, clear).await?;
    crate::keyboard::focus_pause(humanize).await;

    let focused_target =
        crate::interaction::observe_element(page, &resolved.remote_object_id).await?;
    if !focused_target.active {
        return Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element could not be focused for typing",
        ));
    }

    let confirmation = if clear && text.is_empty() {
        crate::interaction::clear_text_input(page, &resolved.remote_object_id).await?;
        crate::interaction::confirm_input(page, &resolved.remote_object_id, "", before_page).await
    } else {
        crate::keyboard::type_text(page, text, humanize).await?;
        crate::interaction::confirm_input(page, &resolved.remote_object_id, text, before_page).await
    };

    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::SetValue,
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Keyboard),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn upload_file(
    page: &Arc<Page>,
    element: &Element,
    path: &str,
) -> Result<InteractionOutcome, RubError> {
    if !std::path::Path::new(path).exists() {
        return Err(RubError::domain(
            ErrorCode::FileNotFound,
            format!("File not found: {path}"),
        ));
    }

    let resolved = crate::targeting::resolve_element(page, element).await?;
    ensure_control_enabled(page, &resolved.remote_object_id).await?;
    let before_page =
        crate::interaction::capture_related_page_baseline(page, &resolved.remote_object_id).await;
    let backend_node_id = resolved.backend_node_id.ok_or_else(|| {
        RubError::domain(ErrorCode::ElementNotFound, "Element has no backend node id")
    })?;

    let kind = crate::js::call_function_returning_string(
        page,
        &resolved.remote_object_id,
        "function() { return this.tagName === 'INPUT' && this.type === 'file' ? 'FILE_INPUT' : 'NOT_FILE_INPUT'; }",
    )
    .await?;
    if kind != "FILE_INPUT" {
        return Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is not an <input type=\"file\">",
        ));
    }

    let abs_path = std::fs::canonicalize(path)
        .map_err(|e| RubError::Internal(format!("Cannot resolve path: {e}")))?
        .to_string_lossy()
        .to_string();

    let params = SetFileInputFilesParams::builder()
        .files(vec![abs_path])
        .backend_node_id(backend_node_id)
        .build()
        .map_err(|e| RubError::Internal(format!("Build SetFileInputFiles failed: {e}")))?;
    page.execute(params)
        .await
        .map_err(|e| RubError::Internal(format!("SetFileInputFiles failed: {e}")))?;

    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path);
    let confirmation = crate::interaction::confirm_upload(
        page,
        &resolved.remote_object_id,
        file_name,
        before_page,
    )
    .await;

    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::SetValue,
        element_verified: resolved.verified,
        actuation: Some(InteractionActuation::Programmatic),
        confirmation: Some(confirmation),
    })
}

pub(crate) async fn select_option(
    page: &Arc<Page>,
    element: &Element,
    value: &str,
) -> Result<SelectOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    ensure_control_enabled(page, &resolved.remote_object_id).await?;
    let before_page =
        crate::interaction::capture_related_page_baseline(page, &resolved.remote_object_id).await;

    let value_literal = js_string_literal(value)?;
    let js = format!(
        r#"function() {{
            const targetValue = {value_literal};
            if (this.tagName !== 'SELECT') return 'NOT_SELECT';
            const opts = Array.from(this.options);
            const match = opts.find(o => o.text === targetValue || o.value === targetValue);
            if (!match) return 'NO_MATCH';
            this.value = match.value;
            this.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return JSON.stringify({{ status: 'OK', selected_value: match.value, selected_text: match.text }});
        }}"#
    );

    let result =
        crate::js::call_function_returning_string(page, &resolved.remote_object_id, &js).await?;
    match result.as_str() {
        value if value.starts_with('{') => {
            let payload: serde_json::Value = serde_json::from_str(value)
                .map_err(|e| RubError::Internal(format!("Parse select payload failed: {e}")))?;
            let selected_value = payload["selected_value"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let selected_text = payload["selected_text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let confirmation = crate::interaction::confirm_select(
                page,
                &resolved.remote_object_id,
                &selected_value,
                &selected_text,
                before_page,
            )
            .await;
            Ok(SelectOutcome {
                semantic_class: InteractionSemanticClass::SelectChoice,
                element_verified: resolved.verified,
                selected_value,
                selected_text,
                actuation: Some(InteractionActuation::Programmatic),
                confirmation: Some(confirmation),
            })
        }
        "NOT_SELECT" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is not a <select>",
        )),
        "NO_MATCH" => Err(RubError::domain(
            ErrorCode::NoMatchingOption,
            format!("No option matching '{value}' found"),
        )),
        other => Err(RubError::Internal(format!(
            "Unexpected select result: {other}"
        ))),
    }
}

fn js_string_literal(value: &str) -> Result<String, RubError> {
    let serialized = serde_json::to_string(value)
        .map_err(|e| RubError::Internal(format!("Serialize JS string literal failed: {e}")))?;
    Ok(serialized
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

async fn ensure_control_enabled(
    page: &Arc<Page>,
    object_id: &chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId,
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

    if matches!(state.as_str(), "DISABLED" | "FIELDSET_DISABLED") {
        return Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is disabled",
        ));
    }

    if state == "ARIA_DISABLED" {
        return Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is aria-disabled",
        ));
    }

    Ok(())
}

async fn ensure_text_control_editable(
    page: &Arc<Page>,
    object_id: &chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId,
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
            if (typeof this.readOnly === 'boolean' && this.readOnly) return 'READONLY';
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
        "READONLY" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is readonly",
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::js_string_literal;

    #[test]
    fn js_string_literal_preserves_js_unsafe_newlines_and_separators() {
        let literal = js_string_literal("line1\nline2\r\u{2028}\u{2029}'\\").unwrap();
        assert_eq!(literal, "\"line1\\nline2\\r\\u2028\\u2029'\\\\\"");
    }
}
