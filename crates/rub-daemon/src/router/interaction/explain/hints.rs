pub(super) fn interactability_next_command_hints(
    element: &rub_core::model::Element,
    assessment: &serde_json::Value,
    locator_request: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut hints = Vec::new();

    if let Some(command) =
        interactability_direct_click_command(element, assessment, locator_request)
    {
        hints.push(interactability_command_hint(
            &command,
            "try the direct activation path; this target may be the control that clears the current page-level blocker",
        ));
    }
    if interactability_has_target_waitable_blocker(assessment)
        && let Some(wait_command) = interactability_wait_command(locator_request)
    {
        hints.push(interactability_command_hint(
            &wait_command,
            "wait for this target to become interactable before retrying the action",
        ));
    }
    if interactability_has_page_level_blocker(assessment) {
        hints.push(interactability_command_hint(
            "rub explain blockers",
            "inspect the dominant page-level blocker in the current runtime",
        ));
    }
    hints.push(interactability_command_hint(
        "rub explain locator ...",
        "confirm the exact target if you still need to disambiguate candidates",
    ));
    hints
}

pub(super) fn interactability_has_target_waitable_blocker(assessment: &serde_json::Value) -> bool {
    assessment
        .get("blocker_details")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|detail| {
                matches!(
                    detail.get("code").and_then(serde_json::Value::as_str),
                    Some("disabled_element" | "aria_disabled")
                )
            })
        })
}

pub(super) fn interactability_has_page_level_blocker(assessment: &serde_json::Value) -> bool {
    assessment
        .get("blocker_details")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|detail| {
                matches!(
                    detail.get("surface").and_then(serde_json::Value::as_str),
                    Some("runtime.readiness" | "runtime.interference")
                )
            })
        })
}

pub(super) fn interactability_wait_command(locator_request: &serde_json::Value) -> Option<String> {
    let mut parts = vec!["rub wait".to_string()];
    parts.extend(interactability_locator_parts(locator_request)?);
    parts.push("--state".to_string());
    parts.push("interactable".to_string());
    Some(parts.join(" "))
}

pub(super) fn interactability_direct_click_command(
    element: &rub_core::model::Element,
    assessment: &serde_json::Value,
    locator_request: &serde_json::Value,
) -> Option<String> {
    if interactability_has_target_waitable_blocker(assessment)
        || !interactability_is_direct_recovery_candidate(element, assessment)
    {
        return None;
    }
    let mut parts = vec!["rub click".to_string()];
    parts.extend(interactability_locator_parts(locator_request)?);
    Some(parts.join(" "))
}

pub(super) fn interactability_is_direct_recovery_candidate(
    element: &rub_core::model::Element,
    assessment: &serde_json::Value,
) -> bool {
    if !matches!(
        element.tag,
        rub_core::model::ElementTag::Button
            | rub_core::model::ElementTag::Link
            | rub_core::model::ElementTag::Other
    ) {
        return false;
    }
    let Some(details) = assessment
        .get("blocker_details")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    !details.is_empty()
        && details.iter().all(|detail| {
            matches!(
                detail.get("code").and_then(serde_json::Value::as_str),
                Some(
                    "overlay_present"
                        | "route_transitioning"
                        | "loading_present"
                        | "skeleton_present"
                )
            )
        })
}

pub(super) fn interactability_locator_parts(
    locator_request: &serde_json::Value,
) -> Option<Vec<String>> {
    let object = locator_request.as_object()?;
    let mut parts = Vec::new();
    let mut has_locator = false;

    for (flag, key) in [
        ("--selector", "selector"),
        ("--target-text", "target_text"),
        ("--role", "role"),
        ("--label", "label"),
        ("--testid", "testid"),
    ] {
        if let Some(value) = object.get(key).and_then(serde_json::Value::as_str) {
            parts.push(flag.to_string());
            parts.push(shell_double_quoted(value));
            has_locator = true;
            break;
        }
    }
    if !has_locator {
        return None;
    }
    if object.get("first").and_then(serde_json::Value::as_bool) == Some(true) {
        parts.push("--first".to_string());
    }
    if object.get("last").and_then(serde_json::Value::as_bool) == Some(true) {
        parts.push("--last".to_string());
    }
    if let Some(nth) = object.get("nth").and_then(serde_json::Value::as_u64) {
        parts.push("--nth".to_string());
        parts.push(nth.to_string());
    }
    if object.get("topmost").and_then(serde_json::Value::as_bool) == Some(true) {
        parts.push("--topmost".to_string());
    }
    if object.get("visible").and_then(serde_json::Value::as_bool) == Some(true) {
        parts.push("--visible".to_string());
    }
    if object
        .get("prefer_enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        parts.push("--prefer-enabled".to_string());
    }
    Some(parts)
}

pub(super) fn interactability_command_hint(command: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "command": command,
        "reason": reason,
    })
}

fn shell_double_quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}
