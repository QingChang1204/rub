use crate::commands::{
    ElementAddressArgs, ObservationProjectionArgs, ObservationScopeArgs, WaitAfterArgs,
};
use crate::workflow_assets::{resolve_named_workflow_path, workflow_asset_path_state};
use crate::workflow_params::resolve_workflow_parameters;
use rub_core::error::{ErrorCode, RubError};
use rub_core::json_spec::NormalizedJsonSpec;
use rub_core::model::PathReferenceState;
use rub_ipc::protocol::IpcRequest;
use std::path::{Path, PathBuf};

mod builder;
mod waits;

use self::builder::parse_extract_builder_field;
pub(crate) use self::waits::{
    merge_json_objects, parse_indexed_operand, wait_after_is_configured, wait_command_args,
    with_wait_after,
};
use self::waits::{non_empty_arg, selection_requested, validate_selection_flags};

pub(crate) struct WaitProbeArgs<'a> {
    pub selector: Option<&'a str>,
    pub target_text: Option<&'a str>,
    pub role: Option<&'a str>,
    pub label: Option<&'a str>,
    pub testid: Option<&'a str>,
    pub text: Option<&'a str>,
    pub description_contains: Option<&'a str>,
    pub url_contains: Option<&'a str>,
    pub title_contains: Option<&'a str>,
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

pub(crate) fn input_path_reference_state(
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) -> PathReferenceState {
    PathReferenceState {
        truth_level: "input_path_reference".to_string(),
        path_authority: path_authority.to_string(),
        upstream_truth: upstream_truth.to_string(),
        path_kind: path_kind.to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(crate) fn resolve_pipe_spec(
    inline_spec: Option<&str>,
    file: Option<&str>,
    workflow: Option<&str>,
    vars: &[String],
    rub_home: &Path,
) -> Result<(NormalizedJsonSpec, serde_json::Value), RubError> {
    match (inline_spec, file, workflow) {
        (Some(spec), None, None) => {
            let parameterized = resolve_workflow_parameters(spec, vars)?;
            Ok((
                NormalizedJsonSpec::from_raw_str(&parameterized.resolved_spec, "pipe")?,
                serde_json::json!({
                    "kind": "inline",
                    "vars": parameterized.parameter_keys,
                }),
            ))
        }
        (None, Some(path), None) => {
            let contents = std::fs::read_to_string(path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => input_file_read_error(
                    ErrorCode::FileNotFound,
                    format!("Workflow file not found: {path}"),
                    path,
                    input_path_reference_state(
                        "cli.pipe.spec_source.path",
                        "cli_pipe_file_option",
                        "workflow_spec_file",
                    ),
                    "pipe_spec_file_not_found",
                ),
                _ => input_file_read_error(
                    ErrorCode::InvalidInput,
                    format!("Failed to read workflow file {path}: {error}"),
                    path,
                    input_path_reference_state(
                        "cli.pipe.spec_source.path",
                        "cli_pipe_file_option",
                        "workflow_spec_file",
                    ),
                    "pipe_spec_file_read_failed",
                ),
            })?;
            let parameterized = resolve_workflow_parameters(&contents, vars)?;
            Ok((
                NormalizedJsonSpec::from_raw_str(&parameterized.resolved_spec, "pipe")?,
                serde_json::json!({
                    "kind": "file",
                    "path": path,
                    "path_state": input_path_reference_state(
                        "cli.pipe.spec_source.path",
                        "cli_pipe_file_option",
                        "workflow_spec_file",
                    ),
                    "vars": parameterized.parameter_keys,
                }),
            ))
        }
        (None, None, Some(name)) => {
            let path = resolve_named_workflow_path(rub_home, name)?;
            let path_string = path.display().to_string();
            let path_state =
                workflow_asset_path_state("cli.pipe.spec_source.path", "cli_pipe_workflow_option");
            let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => input_file_read_error(
                    ErrorCode::FileNotFound,
                    format!("Named workflow not found: {name} ({path_string})"),
                    &path_string,
                    path_state.clone(),
                    "named_workflow_asset_not_found",
                ),
                _ => input_file_read_error(
                    ErrorCode::InvalidInput,
                    format!("Failed to read workflow asset {path_string}: {error}"),
                    &path_string,
                    path_state.clone(),
                    "named_workflow_asset_read_failed",
                ),
            })?;
            let parameterized = resolve_workflow_parameters(&contents, vars)?;
            Ok((
                NormalizedJsonSpec::from_raw_str(&parameterized.resolved_spec, "pipe")?,
                serde_json::json!({
                    "kind": "workflow",
                    "name": name,
                    "path": path_string,
                    "path_state": path_state,
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
) -> Result<(NormalizedJsonSpec, serde_json::Value), RubError> {
    match (inline_spec, file) {
        (Some(spec), None) => Ok((
            NormalizedJsonSpec::from_raw_str(spec, command)?,
            serde_json::json!({
                "kind": "inline",
            }),
        )),
        (None, Some(path)) => {
            let path = resolve_cli_path(path);
            let path_string = path.display().to_string();
            let path_state = json_spec_path_state(command);
            let contents = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => input_file_read_error(
                    ErrorCode::FileNotFound,
                    format!("{command} spec file not found: {path_string}"),
                    &path_string,
                    path_state.clone(),
                    "json_spec_file_not_found",
                ),
                _ => input_file_read_error(
                    ErrorCode::InvalidInput,
                    format!("Failed to read {command} spec file {path_string}: {error}"),
                    &path_string,
                    path_state.clone(),
                    "json_spec_file_read_failed",
                ),
            })?;
            Ok((
                NormalizedJsonSpec::from_raw_str(&contents, command)?,
                serde_json::json!({
                    "kind": "file",
                    "path": path_string,
                    "path_state": path_state,
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

fn json_spec_path_state(command: &str) -> PathReferenceState {
    match command {
        "fill" => input_path_reference_state(
            "cli.fill.spec_source.path",
            "cli_fill_file_option",
            "json_spec_file",
        ),
        "extract" => input_path_reference_state(
            "cli.extract.spec_source.path",
            "cli_extract_file_option",
            "json_spec_file",
        ),
        "inspect list" => input_path_reference_state(
            "cli.inspect_list.spec_source.path",
            "cli_inspect_list_file_option",
            "json_spec_file",
        ),
        _ => input_path_reference_state(
            "cli.unknown.spec_source.path",
            "cli_unknown_file_option",
            "json_spec_file",
        ),
    }
}

fn input_file_read_error(
    code: ErrorCode,
    message: String,
    path: &str,
    path_state: PathReferenceState,
    reason: &str,
) -> RubError {
    RubError::domain_with_context(
        code,
        message,
        serde_json::json!({
            "path": path,
            "path_state": path_state,
            "reason": reason,
        }),
    )
}

pub(crate) fn resolve_inspect_list_spec_source(
    inline_spec: Option<&str>,
    file: Option<&str>,
    collection: Option<&str>,
    row_scope: Option<&str>,
    fields: &[String],
) -> Result<(NormalizedJsonSpec, serde_json::Value), RubError> {
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
        NormalizedJsonSpec::from_value(spec),
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
) -> Result<(NormalizedJsonSpec, serde_json::Value), RubError> {
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
        NormalizedJsonSpec::from_value(spec),
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
        "visible": target.visible,
        "prefer_enabled": target.prefer_enabled,
        "topmost": target.topmost,
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
