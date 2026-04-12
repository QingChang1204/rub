use super::*;

pub(crate) fn parse_indexed_operand(
    operands: &[String],
    command: &str,
    value_name: &str,
) -> Result<(Option<u32>, String), RubError> {
    match operands {
        [value] => Ok((None, value.clone())),
        [index, value] => {
            let index = index.parse::<u32>().map_err(|_| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("{command} expects `<{value_name}>` or `<index> <{value_name}>`"),
                )
            })?;
            Ok((Some(index), value.clone()))
        }
        _ => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} expects `<{value_name}>` or `<index> <{value_name}>`"),
        )),
    }
}

pub(crate) fn with_wait_after(
    mut args: serde_json::Value,
    wait_after: &WaitAfterArgs,
) -> Result<serde_json::Value, RubError> {
    let Some(object) = args.as_object_mut() else {
        return Ok(args);
    };
    if let Some(wait) = wait_after_args(wait_after)? {
        object.insert("wait_after".to_string(), serde_json::Value::Object(wait));
    }
    Ok(args)
}

pub(crate) fn wait_command_args(
    probe: WaitProbeArgs<'_>,
    timeout_ms: u64,
    state: &str,
) -> Result<serde_json::Value, RubError> {
    let mut args = serde_json::json!({
        "timeout_ms": timeout_ms,
        "state": state,
    });
    let Some(object) = args.as_object_mut() else {
        return Ok(args);
    };
    let wait = build_wait_probe_object(&probe)?;
    for (key, value) in wait {
        object.insert(key, value);
    }
    Ok(args)
}

fn wait_after_args(
    wait_after: &WaitAfterArgs,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, RubError> {
    if !wait_after_is_configured(wait_after) {
        return Ok(None);
    }

    let mut wait = build_wait_probe_object(&WaitProbeArgs {
        selector: wait_after.selector.as_deref(),
        target_text: wait_after.target_text.as_deref(),
        role: wait_after.role.as_deref(),
        label: wait_after.label.as_deref(),
        testid: wait_after.testid.as_deref(),
        text: wait_after.text.as_deref(),
        description_contains: wait_after.description_contains.as_deref(),
        url_contains: wait_after.url_contains.as_deref(),
        title_contains: wait_after.title_contains.as_deref(),
        first: wait_after.first,
        last: wait_after.last,
        nth: wait_after.nth,
    })?;
    if let Some(timeout_ms) = wait_after.timeout_ms {
        wait.insert("timeout_ms".to_string(), serde_json::json!(timeout_ms));
    }
    if let Some(state) = &wait_after.state {
        wait.insert("state".to_string(), serde_json::json!(state));
    }
    Ok(Some(wait))
}

pub(crate) fn wait_after_is_configured(wait_after: &WaitAfterArgs) -> bool {
    wait_after.selector.is_some()
        || wait_after.target_text.is_some()
        || wait_after.role.is_some()
        || wait_after.label.is_some()
        || wait_after.testid.is_some()
        || wait_after.text.is_some()
        || wait_after.description_contains.is_some()
        || wait_after.url_contains.is_some()
        || wait_after.title_contains.is_some()
        || wait_after.first
        || wait_after.last
        || wait_after.nth.is_some()
        || wait_after.timeout_ms.is_some()
        || wait_after.state.is_some()
}

fn build_wait_probe_object(
    probe: &WaitProbeArgs<'_>,
) -> Result<serde_json::Map<String, serde_json::Value>, RubError> {
    let selector = non_empty_arg(probe.selector);
    let target_text = non_empty_arg(probe.target_text);
    let role = non_empty_arg(probe.role);
    let label = non_empty_arg(probe.label);
    let testid = non_empty_arg(probe.testid);
    let text = non_empty_arg(probe.text);
    let description_contains = non_empty_arg(probe.description_contains);
    let url_contains = non_empty_arg(probe.url_contains);
    let title_contains = non_empty_arg(probe.title_contains);
    validate_selection_flags(
        probe.first,
        probe.last,
        probe.nth,
        "Wait probe selection is ambiguous: provide at most one of --first, --last, or --nth",
    )?;

    let locator_count = selector.is_some() as u8
        + target_text.is_some() as u8
        + role.is_some() as u8
        + label.is_some() as u8
        + testid.is_some() as u8;
    let page_probe_count =
        text.is_some() as u8 + url_contains.is_some() as u8 + title_contains.is_some() as u8;
    let locator_match_probe_count = description_contains.is_some() as u8;
    if page_probe_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe is ambiguous: provide only one of --text, --url-contains, or --title-contains",
        ));
    }
    if page_probe_count > 0 && (locator_count > 0 || locator_match_probe_count > 0) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe is ambiguous: provide either a page-level probe or a single locator, not both",
        ));
    }
    if locator_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe is ambiguous: provide at most one of --selector, --target-text, --role, --label, or --testid",
        ));
    }
    if locator_match_probe_count > 0 && locator_count == 0 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe `--description-contains` requires exactly one locator such as --label or --selector",
        ));
    }
    if page_probe_count == 0 && locator_count == 0 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Missing required wait probe: selector, target_text, role, label, testid, text, description_contains, url_contains, or title_contains",
        ));
    }
    if page_probe_count > 0 && selection_requested(probe.first, probe.last, probe.nth) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Match selection is not supported for page-level waits",
        ));
    }

    let mut wait = serde_json::Map::new();
    if let Some(selector) = selector {
        wait.insert("selector".to_string(), serde_json::json!(selector));
    }
    if let Some(target_text) = target_text {
        wait.insert("target_text".to_string(), serde_json::json!(target_text));
    }
    if let Some(role) = role {
        wait.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(label) = label {
        wait.insert("label".to_string(), serde_json::json!(label));
    }
    if let Some(testid) = testid {
        wait.insert("testid".to_string(), serde_json::json!(testid));
    }
    if let Some(text) = text {
        wait.insert("text".to_string(), serde_json::json!(text));
    }
    if let Some(description_contains) = description_contains {
        wait.insert(
            "description_contains".to_string(),
            serde_json::json!(description_contains),
        );
    }
    if let Some(url_contains) = url_contains {
        wait.insert("url_contains".to_string(), serde_json::json!(url_contains));
    }
    if let Some(title_contains) = title_contains {
        wait.insert(
            "title_contains".to_string(),
            serde_json::json!(title_contains),
        );
    }
    if probe.first {
        wait.insert("first".to_string(), serde_json::json!(true));
    }
    if probe.last {
        wait.insert("last".to_string(), serde_json::json!(true));
    }
    if let Some(nth) = probe.nth {
        wait.insert("nth".to_string(), serde_json::json!(nth));
    }
    Ok(wait)
}

pub(super) fn validate_selection_flags(
    first: bool,
    last: bool,
    nth: Option<u32>,
    message: &str,
) -> Result<(), RubError> {
    let selection_count = first as u8 + last as u8 + nth.is_some() as u8;
    if selection_count > 1 {
        return Err(RubError::domain(ErrorCode::InvalidInput, message));
    }
    Ok(())
}

pub(super) fn selection_requested(first: bool, last: bool, nth: Option<u32>) -> bool {
    first || last || nth.is_some()
}

pub(super) fn non_empty_arg(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn merge_json_objects(
    mut left: serde_json::Value,
    right: serde_json::Value,
) -> serde_json::Value {
    let Some(left_object) = left.as_object_mut() else {
        return left;
    };
    if let Some(right_object) = right.as_object() {
        for (key, value) in right_object {
            left_object.insert(key.clone(), value.clone());
        }
    }
    left
}
