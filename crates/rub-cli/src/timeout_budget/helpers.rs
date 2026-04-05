use crate::commands::{
    ElementAddressArgs, ObservationProjectionArgs, ObservationScopeArgs, WaitAfterArgs,
};
use crate::workflow_assets::resolve_named_workflow_path;
use crate::workflow_params::resolve_workflow_parameters;
use rub_core::error::{ErrorCode, RubError};
use rub_ipc::protocol::IpcRequest;
use std::path::{Path, PathBuf};

pub(crate) struct WaitProbeArgs<'a> {
    pub selector: Option<&'a str>,
    pub target_text: Option<&'a str>,
    pub role: Option<&'a str>,
    pub label: Option<&'a str>,
    pub testid: Option<&'a str>,
    pub text: Option<&'a str>,
    pub first: bool,
    pub last: bool,
    pub nth: Option<u32>,
}

pub(crate) fn mutating_request(
    command: &str,
    args: serde_json::Value,
    timeout_ms: u64,
) -> IpcRequest {
    IpcRequest::new(command, args, timeout_ms)
        .with_command_id(uuid::Uuid::now_v7().to_string())
        .expect("UUID command_id must be valid")
}

pub(crate) fn resolve_pipe_spec(
    inline_spec: Option<&str>,
    file: Option<&str>,
    workflow: Option<&str>,
    vars: &[String],
    rub_home: &Path,
) -> Result<(String, serde_json::Value), RubError> {
    match (inline_spec, file, workflow) {
        (Some(spec), None, None) => {
            let parameterized = resolve_workflow_parameters(spec, vars)?;
            Ok((
                parameterized.resolved_spec,
                serde_json::json!({
                    "kind": "inline",
                    "vars": parameterized.parameter_keys,
                }),
            ))
        }
        (None, Some(path), None) => {
            let contents = std::fs::read_to_string(path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => RubError::domain(
                    ErrorCode::FileNotFound,
                    format!("Workflow file not found: {path}"),
                ),
                _ => RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Failed to read workflow file {path}: {error}"),
                ),
            })?;
            let parameterized = resolve_workflow_parameters(&contents, vars)?;
            Ok((
                parameterized.resolved_spec,
                serde_json::json!({
                    "kind": "file",
                    "path": path,
                    "vars": parameterized.parameter_keys,
                }),
            ))
        }
        (None, None, Some(name)) => {
            let path = resolve_named_workflow_path(rub_home, name)?;
            let path_string = path.display().to_string();
            let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => RubError::domain(
                    ErrorCode::FileNotFound,
                    format!("Named workflow not found: {name} ({path_string})"),
                ),
                _ => RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Failed to read workflow asset {path_string}: {error}"),
                ),
            })?;
            let parameterized = resolve_workflow_parameters(&contents, vars)?;
            Ok((
                parameterized.resolved_spec,
                serde_json::json!({
                    "kind": "workflow",
                    "name": name,
                    "path": path_string,
                    "vars": parameterized.parameter_keys,
                }),
            ))
        }
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => {
            Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Use exactly one workflow source: inline pipe JSON, --file, or --workflow",
            ))
        }
        (None, None, None) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Provide an inline pipe spec, --file, or --workflow",
        )),
    }
}

pub(crate) fn resolve_json_spec_source(
    command: &str,
    inline_spec: Option<&str>,
    file: Option<&str>,
) -> Result<(String, serde_json::Value), RubError> {
    match (inline_spec, file) {
        (Some(spec), None) => Ok((
            spec.to_string(),
            serde_json::json!({
                "kind": "inline",
            }),
        )),
        (None, Some(path)) => {
            let path = resolve_cli_path(path);
            let path_string = path.display().to_string();
            let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => RubError::domain(
                    ErrorCode::FileNotFound,
                    format!("{command} spec file not found: {path_string}"),
                ),
                _ => RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("Failed to read {command} spec file {path_string}: {error}"),
                ),
            })?;
            Ok((
                contents,
                serde_json::json!({
                    "kind": "file",
                    "path": path_string,
                }),
            ))
        }
        (Some(_), Some(_)) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Use exactly one {command} spec source: inline JSON or --file"),
        )),
        (None, None) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Provide an inline {command} JSON spec or --file"),
        )),
    }
}

pub(crate) fn resolve_inspect_list_spec_source(
    inline_spec: Option<&str>,
    file: Option<&str>,
    collection: Option<&str>,
    row_scope: Option<&str>,
    fields: &[String],
) -> Result<(String, serde_json::Value), RubError> {
    if inline_spec.is_some() || file.is_some() {
        return resolve_json_spec_source("inspect list", inline_spec, file);
    }

    let Some(collection) = collection else {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Provide inspect list JSON, --file, or --collection with one or more --field entries",
        ));
    };
    if fields.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect list --collection requires at least one --field entry",
        ));
    }

    let mut children = serde_json::Map::new();
    let mut field_names = Vec::new();
    for field in fields {
        let (name, spec) = parse_extract_builder_field(field, "inspect list")?;
        field_names.push(name.clone());
        children.insert(name, spec);
    }

    let spec = serde_json::json!({
        "items": {
            "collection": collection,
            "row_scope_selector": row_scope,
            "fields": children,
        }
    });
    Ok((
        serde_json::to_string(&spec).map_err(RubError::from)?,
        serde_json::json!({
            "kind": "builder",
            "collection": collection,
            "row_scope_selector": row_scope,
            "fields": field_names,
        }),
    ))
}

pub(crate) fn resolve_extract_builder_spec_source(
    command: &str,
    fields: &[String],
) -> Result<(String, serde_json::Value), RubError> {
    if fields.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} requires at least one --field entry"),
        ));
    }

    let mut extracted = serde_json::Map::new();
    let mut field_names = Vec::new();
    for field in fields {
        let (name, spec) = parse_extract_builder_field(field, command)?;
        field_names.push(name.clone());
        extracted.insert(name, spec);
    }

    let spec = serde_json::Value::Object(extracted);
    Ok((
        serde_json::to_string(&spec).map_err(RubError::from)?,
        serde_json::json!({
            "kind": "builder",
            "fields": field_names,
        }),
    ))
}

pub(crate) fn element_address_args(
    index: Option<u32>,
    target: &ElementAddressArgs,
) -> Result<serde_json::Value, RubError> {
    element_address_args_with_requirement(index, target, true)
}

pub(crate) fn optional_element_address_args(
    index: Option<u32>,
    target: &ElementAddressArgs,
) -> Result<serde_json::Value, RubError> {
    element_address_args_with_requirement(index, target, false)
}

fn element_address_args_with_requirement(
    index: Option<u32>,
    target: &ElementAddressArgs,
    require_target: bool,
) -> Result<serde_json::Value, RubError> {
    let element_ref = non_empty_arg(target.element_ref.as_deref());
    let selector = non_empty_arg(target.selector.as_deref());
    let target_text = non_empty_arg(target.target_text.as_deref());
    let role = non_empty_arg(target.role.as_deref());
    let label = non_empty_arg(target.label.as_deref());
    let testid = non_empty_arg(target.testid.as_deref());
    let snapshot = non_empty_arg(target.snapshot.as_deref());
    validate_selection_flags(
        target.first,
        target.last,
        target.nth,
        "Match selection is ambiguous: provide at most one of --first, --last, or --nth",
    )?;

    let configured = index.is_some() as u8
        + element_ref.is_some() as u8
        + selector.is_some() as u8
        + target_text.is_some() as u8
        + role.is_some() as u8
        + label.is_some() as u8
        + testid.is_some() as u8;
    if configured == 0 {
        if snapshot.is_some() {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Snapshot targeting requires <index>, --ref, --selector, --target-text, --role, --label, or --testid",
            ));
        }
        if selection_requested(target.first, target.last, target.nth) {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Match selection requires --selector, --target-text, --role, --label, or --testid",
            ));
        }
        if require_target {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Missing required target: provide <index>, --ref, --selector, --target-text, --role, --label, or --testid",
            ));
        }
    }
    if configured > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Target is ambiguous: provide exactly one of <index>, --ref, --selector, --target-text, --role, --label, or --testid",
        ));
    }
    if index.is_some() && selection_requested(target.first, target.last, target.nth) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Match selection cannot be combined with index addressing",
        ));
    }
    if element_ref.is_some() && selection_requested(target.first, target.last, target.nth) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Match selection cannot be combined with ref addressing",
        ));
    }

    Ok(serde_json::json!({
        "index": index,
        "snapshot_id": snapshot,
        "element_ref": element_ref,
        "selector": selector,
        "target_text": target_text,
        "role": role,
        "label": label,
        "testid": testid,
        "first": target.first,
        "last": target.last,
        "nth": target.nth,
    }))
}

pub(crate) fn observation_scope_args(
    scope: &ObservationScopeArgs,
) -> Result<serde_json::Value, RubError> {
    let scope_kind_count = scope.selector.is_some() as u8
        + scope.role.is_some() as u8
        + scope.label.is_some() as u8
        + scope.testid.is_some() as u8;
    if scope_kind_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Observation scope is ambiguous: provide at most one of --scope-selector, --scope-role, --scope-label, or --scope-testid",
        ));
    }

    let selection_count = scope.first as u8 + scope.last as u8 + scope.nth.is_some() as u8;
    if selection_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Observation scope selection is ambiguous: provide at most one of --scope-first, --scope-last, or --scope-nth",
        ));
    }

    let selection = if scope.first {
        Some(serde_json::json!("first"))
    } else if scope.last {
        Some(serde_json::json!("last"))
    } else {
        scope.nth.map(|nth| serde_json::json!({ "nth": nth }))
    };

    let scope_json = if let Some(selector) = &scope.selector {
        let mut object = serde_json::Map::new();
        object.insert("kind".to_string(), serde_json::json!("selector"));
        object.insert("css".to_string(), serde_json::json!(selector));
        if let Some(selection) = selection {
            object.insert("selection".to_string(), selection);
        }
        Some(serde_json::Value::Object(object))
    } else if let Some(role) = &scope.role {
        let mut object = serde_json::Map::new();
        object.insert("kind".to_string(), serde_json::json!("role"));
        object.insert("role".to_string(), serde_json::json!(role));
        if let Some(selection) = selection {
            object.insert("selection".to_string(), selection);
        }
        Some(serde_json::Value::Object(object))
    } else if let Some(label) = &scope.label {
        let mut object = serde_json::Map::new();
        object.insert("kind".to_string(), serde_json::json!("label"));
        object.insert("label".to_string(), serde_json::json!(label));
        if let Some(selection) = selection {
            object.insert("selection".to_string(), selection);
        }
        Some(serde_json::Value::Object(object))
    } else if let Some(testid) = &scope.testid {
        let mut object = serde_json::Map::new();
        object.insert("kind".to_string(), serde_json::json!("test_id"));
        object.insert("testid".to_string(), serde_json::json!(testid));
        if let Some(selection) = selection {
            object.insert("selection".to_string(), selection);
        }
        Some(serde_json::Value::Object(object))
    } else {
        None
    };

    if let Some(scope_json) = scope_json {
        Ok(serde_json::json!({ "scope": scope_json }))
    } else {
        Ok(serde_json::json!({}))
    }
}

pub(crate) fn observation_projection_args(
    projection: &ObservationProjectionArgs,
) -> serde_json::Value {
    serde_json::json!({
        "compact": projection.compact,
        "depth": projection.depth,
    })
}

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
    if text.is_some() && locator_count > 0 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe is ambiguous: provide either page text or a single locator, not both",
        ));
    }
    if locator_count > 1 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Wait probe is ambiguous: provide at most one of --selector, --target-text, --role, --label, or --testid",
        ));
    }
    if text.is_none() && locator_count == 0 {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Missing required wait probe: selector, target_text, role, label, testid, or text",
        ));
    }
    if text.is_some() && selection_requested(probe.first, probe.last, probe.nth) {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Match selection is not supported for page text waits",
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

fn validate_selection_flags(
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

fn selection_requested(first: bool, last: bool, nth: Option<u32>) -> bool {
    first || last || nth.is_some()
}

fn non_empty_arg(value: Option<&str>) -> Option<String> {
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

pub(crate) fn resolve_cli_path(path: &str) -> PathBuf {
    let raw = Path::new(path);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(raw)
    }
}

fn parse_extract_builder_field(
    raw: &str,
    command: &str,
) -> Result<(String, serde_json::Value), RubError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand cannot be empty"),
        ));
    }

    let (name, shorthand) = match raw.split_once('=') {
        Some((name, shorthand)) => (name.trim(), Some(shorthand.trim())),
        None => (raw, None),
    };
    if name.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand '{raw}' is missing a field name"),
        ));
    }

    let mut spec = serde_json::Map::new();
    let selection = shorthand
        .map(|value| parse_builder_field_selection(value, command, raw))
        .transpose()?
        .flatten();
    let shorthand = selection
        .as_ref()
        .map_or(shorthand, |selection| Some(selection.base.as_str()));
    match shorthand {
        None => {
            spec.insert("kind".to_string(), serde_json::json!("text"));
        }
        Some("") => {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("{command} field shorthand '{raw}' is missing a selector or kind"),
            ));
        }
        Some(shorthand) => {
            if let Some(selector) = shorthand.strip_prefix("text:") {
                insert_builder_kind(&mut spec, "text", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("html:") {
                insert_builder_kind(&mut spec, "html", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("value:") {
                insert_builder_kind(&mut spec, "value", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("bbox:") {
                insert_builder_kind(&mut spec, "bbox", selector, command, raw)?;
            } else if let Some(selector) = shorthand.strip_prefix("attributes:") {
                insert_builder_kind(&mut spec, "attributes", selector, command, raw)?;
            } else if let Some(rest) = shorthand.strip_prefix("attribute:") {
                let (attribute, selector) = match rest.split_once(':') {
                    Some((attribute, selector)) => (attribute.trim(), Some(selector.trim())),
                    None => (rest.trim(), None),
                };
                if attribute.is_empty() {
                    return Err(RubError::domain(
                        ErrorCode::InvalidInput,
                        format!("{command} field shorthand '{raw}' is missing an attribute name"),
                    ));
                }
                spec.insert("kind".to_string(), serde_json::json!("attribute"));
                spec.insert("attribute".to_string(), serde_json::json!(attribute));
                if let Some(selector) = selector {
                    insert_builder_locator(
                        &mut spec,
                        selector,
                        command,
                        raw,
                        "a selector or locator after the attribute name",
                    )?;
                }
            } else {
                insert_builder_kind(&mut spec, "text", shorthand, command, raw)?;
            }
        }
    }
    if let Some(selection) = selection {
        selection.apply(&mut spec);
    }

    Ok((name.to_string(), serde_json::Value::Object(spec)))
}

#[derive(Debug)]
struct BuilderFieldSelection {
    base: String,
    mode: BuilderFieldSelectionMode,
}

#[derive(Debug)]
enum BuilderFieldSelectionMode {
    First,
    Last,
    Many,
    Nth(u32),
}

impl BuilderFieldSelection {
    fn apply(self, spec: &mut serde_json::Map<String, serde_json::Value>) {
        match self.mode {
            BuilderFieldSelectionMode::First => {
                spec.insert("first".to_string(), serde_json::json!(true));
            }
            BuilderFieldSelectionMode::Last => {
                spec.insert("last".to_string(), serde_json::json!(true));
            }
            BuilderFieldSelectionMode::Many => {
                spec.insert("many".to_string(), serde_json::json!(true));
            }
            BuilderFieldSelectionMode::Nth(nth) => {
                spec.insert("nth".to_string(), serde_json::json!(nth));
            }
        }
    }
}

fn parse_builder_field_selection(
    shorthand: &str,
    command: &str,
    raw: &str,
) -> Result<Option<BuilderFieldSelection>, RubError> {
    let selection = if let Some(base) = shorthand.strip_suffix("@first") {
        Some((base, BuilderFieldSelectionMode::First))
    } else if let Some(base) = shorthand.strip_suffix("@last") {
        Some((base, BuilderFieldSelectionMode::Last))
    } else if let Some(base) = shorthand.strip_suffix("@many") {
        Some((base, BuilderFieldSelectionMode::Many))
    } else if let Some((base, suffix)) = shorthand.rsplit_once('@') {
        if let Some(argument) = suffix
            .strip_prefix("nth(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let nth = argument.parse::<u32>().map_err(|_| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!("{command} field shorthand '{raw}' has an invalid @nth(...) selection"),
                )
            })?;
            Some((base, BuilderFieldSelectionMode::Nth(nth)))
        } else {
            None
        }
    } else {
        None
    };

    let Some((base, mode)) = selection else {
        return Ok(None);
    };

    let base = base.trim();
    if base.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "{command} field shorthand '{raw}' is missing a selector or kind before the match selection"
            ),
        ));
    }

    Ok(Some(BuilderFieldSelection {
        base: base.to_string(),
        mode,
    }))
}

fn insert_builder_kind(
    spec: &mut serde_json::Map<String, serde_json::Value>,
    kind: &str,
    locator: &str,
    command: &str,
    raw: &str,
) -> Result<(), RubError> {
    spec.insert("kind".to_string(), serde_json::json!(kind));
    insert_builder_locator(spec, locator, command, raw, "a selector or locator")?;
    Ok(())
}

fn insert_builder_locator(
    spec: &mut serde_json::Map<String, serde_json::Value>,
    locator: &str,
    command: &str,
    raw: &str,
    missing_description: &str,
) -> Result<(), RubError> {
    let locator = locator.trim();
    if locator.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand '{raw}' is missing {missing_description}"),
        ));
    }

    if let Some(selector) = locator.strip_prefix("selector:") {
        return insert_named_builder_locator(
            spec,
            "selector",
            selector,
            command,
            raw,
            "a selector after 'selector:'",
        );
    }
    if let Some(target_text) = locator.strip_prefix("target_text:") {
        return insert_named_builder_locator(
            spec,
            "target_text",
            target_text,
            command,
            raw,
            "target text after 'target_text:'",
        );
    }
    if let Some(role) = locator.strip_prefix("role:") {
        return insert_named_builder_locator(
            spec,
            "role",
            role,
            command,
            raw,
            "a role after 'role:'",
        );
    }
    if let Some(label) = locator.strip_prefix("label:") {
        return insert_named_builder_locator(
            spec,
            "label",
            label,
            command,
            raw,
            "a label after 'label:'",
        );
    }
    if let Some(testid) = locator.strip_prefix("testid:") {
        return insert_named_builder_locator(
            spec,
            "testid",
            testid,
            command,
            raw,
            "a test id after 'testid:'",
        );
    }

    spec.insert("selector".to_string(), serde_json::json!(locator));
    Ok(())
}

fn insert_named_builder_locator(
    spec: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &str,
    command: &str,
    raw: &str,
    missing_description: &str,
) -> Result<(), RubError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("{command} field shorthand '{raw}' is missing {missing_description}"),
        ));
    }
    spec.insert(key.to_string(), serde_json::json!(value));
    Ok(())
}
