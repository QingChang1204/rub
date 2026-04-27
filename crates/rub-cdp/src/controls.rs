use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
use chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId;
use rub_core::InteractionOutcome;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{Element, InteractionActuation, InteractionSemanticClass, SelectOutcome};
use std::sync::Arc;

use crate::dialogs::SharedDialogRuntime;
use crate::humanize::HumanizeConfig;
use crate::interaction::EditableProjectionKind;

pub(crate) async fn type_into(
    page: &Arc<Page>,
    element: &Element,
    text: &str,
    clear: bool,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    input_text_with_resolved_target(
        page,
        &resolved.remote_object_id,
        resolved.verified,
        text,
        clear,
        humanize,
        dialog_runtime,
    )
    .await
}

pub(crate) async fn upload_file(
    page: &Arc<Page>,
    element: &Element,
    path: &str,
    dialog_runtime: &SharedDialogRuntime,
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
    let expected_target_id = page.target_id().as_ref().to_string();
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
    let page_for_upload = page.clone();
    let fence = crate::interaction::await_actuation_or_dialog(
        async move {
            page_for_upload
                .execute(params)
                .await
                .map_err(|e| RubError::Internal(format!("SetFileInputFiles failed: {e}")))?;
            Ok(())
        },
        dialog_runtime.clone(),
        "file_upload",
        &expected_target_id,
    )
    .await?;

    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path);
    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::SetValue,
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(confirmation),
        });
    }
    if matches!(
        fence.fence,
        crate::interaction::ActuationFence::DialogOpened
    ) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::SetValue,
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(
        fence.fence,
        crate::interaction::ActuationFence::Indeterminate
    ) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::SetValue,
            element_verified: resolved.verified,
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "file_upload",
            )),
        });
    }

    let confirmation = crate::interaction::confirm_upload(
        page,
        &resolved.remote_object_id,
        file_name,
        before_page,
        dialog_runtime,
        &fence.dialog_baseline,
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
    dialog_runtime: &SharedDialogRuntime,
) -> Result<SelectOutcome, RubError> {
    let resolved = crate::targeting::resolve_element(page, element).await?;
    ensure_control_enabled(page, &resolved.remote_object_id).await?;
    let before_page =
        crate::interaction::capture_related_page_baseline(page, &resolved.remote_object_id).await;
    let expected_target_id = page.target_id().as_ref().to_string();

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

    let page_for_select = page.clone();
    let object_id_for_select = resolved.remote_object_id.clone();
    let value_for_select = value.to_string();
    let fence: crate::interaction::ActuationResultFenceOutcome<(String, String)> =
        crate::interaction::await_actuation_result_or_dialog(
            async move {
                let result = crate::js::call_function_returning_string(
                    &page_for_select,
                    &object_id_for_select,
                    &js,
                )
                .await?;
                parse_select_result(&result, &value_for_select)
            },
            dialog_runtime.clone(),
            "select_option",
            &expected_target_id,
        )
        .await?;

    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        let (selected_value, selected_text) = fence.result.unwrap_or_default();
        return Ok(SelectOutcome {
            semantic_class: InteractionSemanticClass::SelectChoice,
            element_verified: resolved.verified,
            selected_value,
            selected_text,
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(confirmation),
        });
    }
    if matches!(
        fence.fence,
        crate::interaction::ActuationFence::DialogOpened
    ) {
        let (selected_value, selected_text) = fence.result.unwrap_or_default();
        return Ok(SelectOutcome {
            semantic_class: InteractionSemanticClass::SelectChoice,
            element_verified: resolved.verified,
            selected_value,
            selected_text,
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(
        fence.fence,
        crate::interaction::ActuationFence::Indeterminate
    ) {
        let (selected_value, selected_text) = fence.result.unwrap_or_default();
        return Ok(SelectOutcome {
            semantic_class: InteractionSemanticClass::SelectChoice,
            element_verified: resolved.verified,
            selected_value,
            selected_text,
            actuation: Some(InteractionActuation::Programmatic),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "select_option",
            )),
        });
    }

    let (selected_value, selected_text) = fence
        .result
        .ok_or_else(|| RubError::Internal("Select actuation completed without result".into()))?;
    let confirmation = crate::interaction::confirm_select(
        page,
        &resolved.remote_object_id,
        &selected_value,
        &selected_text,
        before_page,
        dialog_runtime,
        &fence.dialog_baseline,
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

fn parse_select_result(result: &str, requested_value: &str) -> Result<(String, String), RubError> {
    match result {
        value if value.starts_with('{') => {
            let payload: serde_json::Value = serde_json::from_str(value)
                .map_err(|e| RubError::Internal(format!("Parse select payload failed: {e}")))?;
            Ok((
                payload["selected_value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                payload["selected_text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ))
        }
        "NOT_SELECT" => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is not a <select>",
        )),
        "NO_MATCH" => Err(RubError::domain(
            ErrorCode::NoMatchingOption,
            format!("No option matching '{requested_value}' found"),
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
    object_id: &RemoteObjectId,
) -> Result<EditableProjectionKind, RubError> {
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
            const tag = String(this.tagName || '').toLowerCase();
            if (this.isContentEditable) return 'TEXT';
            if (tag === 'textarea') return 'VALUE';
            const inputType = tag === 'input' ? String(this.getAttribute('type') || '').toLowerCase() : '';
            const textLikeInput =
                tag === 'input'
                && !['checkbox', 'radio', 'file', 'submit', 'button', 'reset', 'image', 'color', 'range', 'hidden'].includes(inputType);
            if (textLikeInput) return 'VALUE';
            return 'NOT_EDITABLE';
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
        "VALUE" => Ok(EditableProjectionKind::Value),
        "TEXT" => Ok(EditableProjectionKind::Text),
        _ => Err(RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element is not an editable text target",
        )),
    }
}

async fn input_text_with_resolved_target(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    element_verified: bool,
    text: &str,
    clear: bool,
    humanize: &HumanizeConfig,
    dialog_runtime: &SharedDialogRuntime,
) -> Result<InteractionOutcome, RubError> {
    let editable_projection = ensure_text_control_editable(page, object_id).await?;
    let expected_text_after_input = if clear {
        text.to_string()
    } else {
        crate::interaction::observe_element(page, object_id)
            .await
            .ok()
            .and_then(|observed| crate::interaction::observed_editable_content(&observed))
            .map(|before| format!("{before}{text}"))
            .unwrap_or_else(|| text.to_string())
    };
    let before_page = crate::interaction::capture_related_page_baseline(page, object_id).await;
    let expected_target_id = page.target_id().as_ref().to_string();
    let page_for_input = page.clone();
    let object_id_for_input = object_id.clone();
    let humanize_for_input = humanize.clone();
    let text_for_input = text.to_string();
    let fence = crate::interaction::await_actuation_or_dialog(
        async move {
            crate::interaction::prepare_text_input(&page_for_input, &object_id_for_input, clear)
                .await?;
            crate::keyboard::focus_pause(&humanize_for_input).await;
            ensure_text_target_focus_committed(
                &page_for_input,
                &object_id_for_input,
                editable_projection,
            )
            .await?;

            if clear && text_for_input.is_empty() {
                crate::interaction::clear_text_input(&page_for_input, &object_id_for_input).await?;
            } else {
                crate::keyboard::type_text(&page_for_input, &text_for_input, &humanize_for_input)
                    .await?;
            }

            Ok(())
        },
        dialog_runtime.clone(),
        "text_input",
        &expected_target_id,
    )
    .await?;

    if let Some(confirmation) = crate::interaction::dialog_confirmation(
        dialog_runtime,
        &expected_target_id,
        &fence.dialog_baseline,
    )
    .await
    {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::SetValue,
            element_verified,
            actuation: Some(InteractionActuation::Keyboard),
            confirmation: Some(confirmation),
        });
    }
    if matches!(
        fence.fence,
        crate::interaction::ActuationFence::DialogOpened
    ) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::SetValue,
            element_verified,
            actuation: Some(InteractionActuation::Keyboard),
            confirmation: Some(crate::interaction::unconfirmed_dialog_opening()),
        });
    }
    if matches!(
        fence.fence,
        crate::interaction::ActuationFence::Indeterminate
    ) {
        return Ok(InteractionOutcome {
            semantic_class: InteractionSemanticClass::SetValue,
            element_verified,
            actuation: Some(InteractionActuation::Keyboard),
            confirmation: Some(crate::interaction::indeterminate_actuation_confirmation(
                "text_input",
            )),
        });
    }

    let confirmation = crate::interaction::confirm_input(
        page,
        object_id,
        &expected_text_after_input,
        before_page,
        dialog_runtime,
        &fence.dialog_baseline,
    )
    .await;

    Ok(InteractionOutcome {
        semantic_class: InteractionSemanticClass::SetValue,
        element_verified,
        actuation: Some(InteractionActuation::Keyboard),
        confirmation: Some(confirmation),
    })
}

async fn ensure_text_target_focus_committed(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    editable_projection: EditableProjectionKind,
) -> Result<(), RubError> {
    let focused_target = crate::interaction::observe_element(page, object_id).await?;
    if focused_target.active && focused_target.editable_projection == Some(editable_projection) {
        return Ok(());
    }

    Err(RubError::domain(
        ErrorCode::ElementNotInteractable,
        "Element could not be focused as an editable typing target",
    ))
}

#[cfg(test)]
mod tests {
    use super::{js_string_literal, parse_select_result};
    use rub_core::error::ErrorCode;

    #[test]
    fn js_string_literal_preserves_js_unsafe_newlines_and_separators() {
        let literal = js_string_literal("line1\nline2\r\u{2028}\u{2029}'\\").unwrap();
        assert_eq!(literal, "\"line1\\nline2\\r\\u2028\\u2029'\\\\\"");
    }

    #[test]
    fn parse_select_result_preserves_selected_value_and_text() {
        let parsed = parse_select_result(
            r#"{"status":"OK","selected_value":"v2","selected_text":"Two"}"#,
            "Two",
        )
        .expect("valid select payload");
        assert_eq!(parsed, ("v2".to_string(), "Two".to_string()));
    }

    #[test]
    fn parse_select_result_keeps_non_select_as_interactability_error() {
        let error =
            parse_select_result("NOT_SELECT", "Two").expect_err("not select should fail closed");
        match error {
            rub_core::error::RubError::Domain(envelope) => {
                assert_eq!(envelope.code, ErrorCode::ElementNotInteractable);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
