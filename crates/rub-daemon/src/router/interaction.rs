mod args;
mod explain;
mod projection;

use self::args::{
    ClickArgs, ClickGesture, HoverArgs, KeysArgs, SelectArgs, TextEntryArgs, UploadArgs,
    click_command_name, click_gesture_name, requested_click_gesture,
};
use self::explain::enrich_interactability_error_if_needed;
use self::projection::{
    capture_interaction_baseline, finalize_interaction_projection, finalize_select_projection,
};
use super::addressing::resolve_element;
use super::artifacts::annotate_path_reference_state;
use super::projection::{
    attach_result, attach_subject, coordinates_subject, element_subject, focused_frame_subject,
};
use super::request_args::parse_json_args;
use super::*;

pub(super) async fn cmd_click(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: ClickArgs = parse_json_args(args, "click")?;
    cmd_click_with_gesture(router, args, parsed, deadline, state).await
}

async fn cmd_click_with_gesture(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    args: ClickArgs,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let gesture = requested_click_gesture(args.gesture.as_deref())?;
    if let Some([x, y]) = args.xy {
        let baseline = capture_interaction_baseline(router, state).await;
        let outcome = match gesture {
            ClickGesture::Single => router.browser.click_xy(x, y).await?,
            ClickGesture::Double => router.browser.dblclick_xy(x, y).await?,
            ClickGesture::Right => router.browser.rightclick_xy(x, y).await?,
        };
        let mut data = serde_json::json!({});
        attach_subject(&mut data, coordinates_subject(x, y));
        attach_result(
            &mut data,
            serde_json::json!({
                "gesture": click_gesture_name(gesture),
                "dialog_dismissed": null,
            }),
        );
        finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
        return Ok(data);
    }

    let resolved = resolve_element(
        router,
        raw_args,
        state,
        deadline,
        click_command_name(gesture),
    )
    .await?;
    let element = resolved.element;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match match gesture {
        ClickGesture::Single => router.browser.click(&element).await,
        ClickGesture::Double => router.browser.dblclick(&element).await,
        ClickGesture::Right => router.browser.rightclick(&element).await,
    } {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "click",
                &element,
                &resolved.snapshot_id,
                raw_args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "gesture": click_gesture_name(gesture),
            "dialog_dismissed": null,
        }),
    );
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_keys(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: KeysArgs = parse_json_args(args, "keys")?;
    let combo = rub_core::model::KeyCombo::parse(&parsed.keys)?;
    let baseline = capture_interaction_baseline(router, state).await;
    let selected_frame_id =
        super::frame_scope::effective_request_frame_id(router, args, state).await?;
    let outcome = router.browser.send_keys(&combo).await?;
    let mut data = serde_json::json!({});
    attach_subject(
        &mut data,
        focused_frame_subject(selected_frame_id.as_deref()),
    );
    attach_result(
        &mut data,
        serde_json::json!({
            "keys": parsed.keys,
        }),
    );
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_type(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    cmd_text_entry(router, args, deadline, state).await
}

async fn cmd_text_entry(
    router: &DaemonRouter,
    raw_args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let args: TextEntryArgs = parse_json_args(raw_args, "type")?;
    let text = args.text;
    let clear = args.clear;
    let baseline = capture_interaction_baseline(router, state).await;
    let mut data = serde_json::json!({});
    attach_result(
        &mut data,
        serde_json::json!({
            "text": text,
            "clear": clear,
        }),
    );
    let outcome = if args.locator.is_requested() {
        let resolved = resolve_element(router, raw_args, state, deadline, "type").await?;
        attach_subject(
            &mut data,
            element_subject(&resolved.element, &resolved.snapshot_id),
        );
        match router
            .browser
            .type_into(&resolved.element, &text, clear)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(enrich_interactability_error_if_needed(
                    router,
                    state,
                    "type",
                    &resolved.element,
                    &resolved.snapshot_id,
                    raw_args,
                    error,
                )
                .await);
            }
        }
    } else if clear {
        return Err(RubError::domain(
            rub_core::error::ErrorCode::InvalidInput,
            "`type --clear` requires a target locator or index in the current baseline",
        ));
    } else {
        let selected_frame_id =
            super::frame_scope::effective_request_frame_id(router, raw_args, state).await?;
        attach_subject(
            &mut data,
            focused_frame_subject(selected_frame_id.as_deref()),
        );
        router
            .browser
            .type_text_in_frame(selected_frame_id.as_deref(), &text)
            .await?
    };
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_hover(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let _: HoverArgs = parse_json_args(args, "hover")?;
    let resolved = resolve_element(router, args, state, deadline, "hover").await?;
    let element = resolved.element;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match router.browser.hover(&element).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "hover",
                &element,
                &resolved.snapshot_id,
                args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(&mut data, serde_json::json!({}));
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_upload(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: UploadArgs = parse_json_args(args, "upload")?;
    let resolved = resolve_element(router, args, state, deadline, "upload").await?;
    let element = resolved.element;
    let path = parsed.path;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match router.browser.upload_file(&element, &path).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "upload",
                &element,
                &resolved.snapshot_id,
                args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "path": path,
        }),
    );
    if let Some(result) = data.get_mut("result") {
        annotate_path_reference_state(
            result,
            "router.upload.input_path",
            "upload_command_request",
            "external_input_file",
        );
    }
    finalize_interaction_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_select(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let parsed: SelectArgs = parse_json_args(args, "select")?;
    let resolved = resolve_element(router, args, state, deadline, "select").await?;
    let element = resolved.element;
    let value = parsed.value;
    let baseline = capture_interaction_baseline(router, state).await;
    let outcome = match router.browser.select_option(&element, &value).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(enrich_interactability_error_if_needed(
                router,
                state,
                "select",
                &element,
                &resolved.snapshot_id,
                args,
                error,
            )
            .await);
        }
    };
    let mut data = serde_json::json!({});
    attach_subject(&mut data, element_subject(&element, &resolved.snapshot_id));
    attach_result(
        &mut data,
        serde_json::json!({
            "value": outcome.selected_value,
            "text": outcome.selected_text,
        }),
    );
    finalize_select_projection(router, state, &mut data, &outcome, &baseline).await;
    Ok(data)
}

pub(super) async fn cmd_interactability_probe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    explain::cmd_interactability_probe(router, args, deadline, state).await
}

#[cfg(test)]
mod tests;
