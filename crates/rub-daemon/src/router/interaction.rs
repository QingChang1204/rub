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
use super::request_args::{locator_json, parse_json_args};
use super::*;
use crate::router::timeout_projection::record_mutating_possible_commit_timeout_projection;

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
        record_interaction_possible_commit_timeout_projection(
            click_command_name(gesture),
            raw_args,
        );
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
    record_interaction_possible_commit_timeout_projection(click_command_name(gesture), raw_args);
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

fn record_interaction_possible_commit_timeout_projection(command: &str, args: &serde_json::Value) {
    record_mutating_possible_commit_timeout_projection(
        command,
        interaction_possible_commit_recovery_contract(command, args),
    );
}

fn interaction_possible_commit_recovery_contract(
    command: &str,
    args: &serde_json::Value,
) -> serde_json::Value {
    let mut request = serde_json::Map::new();
    if let Some(locator) = redacted_interaction_locator(args) {
        request.insert("locator".to_string(), locator);
    }
    if let Some(xy) = args.get("xy").and_then(|value| value.as_array())
        && xy.len() == 2
    {
        request.insert("xy".to_string(), serde_json::Value::Array(xy.clone()));
    }
    if let Some(snapshot_id) = args.get("snapshot_id").and_then(|value| value.as_str()) {
        request.insert(
            "snapshot_id".to_string(),
            serde_json::Value::String(snapshot_id.to_string()),
        );
    }
    if args.get("wait_after").is_some() {
        request.insert(
            "wait_after_requested".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    request.insert(
        "arguments_redacted".to_string(),
        serde_json::Value::Bool(true),
    );

    serde_json::json!({
        "kind": "interaction_possible_commit",
        "command": command,
        "same_command_retry_requires_same_command_id": true,
        "request": request,
    })
}

fn redacted_interaction_locator(args: &serde_json::Value) -> Option<serde_json::Value> {
    let mut locator = serde_json::Map::new();
    for key in [
        "index",
        "element_ref",
        "ref",
        "selector",
        "role",
        "testid",
        "visible",
        "prefer_enabled",
        "topmost",
        "first",
        "last",
        "nth",
    ] {
        if let Some(value) = args.get(key) {
            let public_key = if key == "ref" { "element_ref" } else { key };
            locator.insert(public_key.to_string(), value.clone());
        }
    }
    if locator.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(locator))
    }
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
        super::frame_scope::effective_interaction_frame_id(router, args, state).await?;
    record_interaction_possible_commit_timeout_projection("keys", args);
    let outcome = router
        .browser
        .send_keys_in_frame(selected_frame_id.as_deref(), &combo)
        .await?;
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
        record_interaction_possible_commit_timeout_projection("type", raw_args);
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
            super::frame_scope::effective_interaction_frame_id(router, raw_args, state).await?;
        attach_subject(
            &mut data,
            focused_frame_subject(selected_frame_id.as_deref()),
        );
        record_interaction_possible_commit_timeout_projection("type", raw_args);
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
    record_interaction_possible_commit_timeout_projection("hover", args);
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
    record_interaction_possible_commit_timeout_projection("upload", args);
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
    record_interaction_possible_commit_timeout_projection("select", args);
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

pub(crate) fn semantic_replay_args(
    command: &str,
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    match command {
        "click" => {
            let parsed: ClickArgs = parse_json_args(args, "click").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert(
                "gesture".to_string(),
                serde_json::json!(click_gesture_name(
                    requested_click_gesture(parsed.gesture.as_deref()).ok()?
                )),
            );
            if let Some(xy) = parsed.xy {
                projected.insert("xy".to_string(), serde_json::json!(xy));
            } else {
                merge_locator_projection(&mut projected, locator_json(parsed._locator));
                if let Some(snapshot_id) = args.get("snapshot_id") {
                    projected.insert("snapshot_id".to_string(), snapshot_id.clone());
                }
                if let Some(orchestration) =
                    super::frame_scope::semantic_replay_orchestration_metadata(args)
                {
                    projected.insert("_orchestration".to_string(), orchestration);
                }
            }
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            Some(serde_json::Value::Object(projected))
        }
        "keys" => {
            let parsed: KeysArgs = parse_json_args(args, "keys").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("keys".to_string(), serde_json::json!(parsed.keys));
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        "type" => {
            let parsed: TextEntryArgs = parse_json_args(args, "type").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("text".to_string(), serde_json::json!(parsed.text));
            projected.insert("clear".to_string(), serde_json::json!(parsed.clear));
            merge_locator_projection(&mut projected, locator_json(parsed.locator));
            if let Some(snapshot_id) = args.get("snapshot_id") {
                projected.insert("snapshot_id".to_string(), snapshot_id.clone());
            }
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        "hover" => {
            let parsed: HoverArgs = parse_json_args(args, "hover").ok()?;
            let mut projected = serde_json::Map::new();
            merge_locator_projection(&mut projected, locator_json(parsed._locator));
            if let Some(snapshot_id) = args.get("snapshot_id") {
                projected.insert("snapshot_id".to_string(), snapshot_id.clone());
            }
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        "upload" => {
            let parsed: UploadArgs = parse_json_args(args, "upload").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("path".to_string(), serde_json::json!(parsed.path));
            merge_locator_projection(&mut projected, locator_json(parsed._locator));
            if let Some(snapshot_id) = args.get("snapshot_id") {
                projected.insert("snapshot_id".to_string(), snapshot_id.clone());
            }
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        "select" => {
            let parsed: SelectArgs = parse_json_args(args, "select").ok()?;
            let mut projected = serde_json::Map::new();
            projected.insert("value".to_string(), serde_json::json!(parsed.value));
            merge_locator_projection(&mut projected, locator_json(parsed._locator));
            if let Some(snapshot_id) = args.get("snapshot_id") {
                projected.insert("snapshot_id".to_string(), snapshot_id.clone());
            }
            if let Some(wait_after) = args.get("wait_after") {
                projected.insert("wait_after".to_string(), wait_after.clone());
            }
            if let Some(orchestration) =
                super::frame_scope::semantic_replay_orchestration_metadata(args)
            {
                projected.insert("_orchestration".to_string(), orchestration);
            }
            Some(serde_json::Value::Object(projected))
        }
        _ => None,
    }
}

fn merge_locator_projection(
    projected: &mut serde_json::Map<String, serde_json::Value>,
    locator: serde_json::Value,
) {
    let serde_json::Value::Object(locator) = locator else {
        return;
    };
    projected.extend(locator);
}

#[cfg(test)]
mod tests;
