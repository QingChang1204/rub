use super::*;
use crate::timeout_budget::helpers::resolve_extract_builder_spec_source;
use rub_core::model::PathReferenceState;
use tokio::fs;

pub(super) async fn load_extract_spec(
    inline_spec: Option<&str>,
    file: Option<&str>,
    fields: &[String],
) -> Result<String, RubError> {
    match (inline_spec, file, fields.is_empty()) {
        (Some(spec), None, true) => Ok(spec.to_string()),
        (None, Some(path), true) => {
            fs::read_to_string(path)
                .await
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => RubError::domain_with_context(
                        ErrorCode::FileNotFound,
                        format!("inspect harvest extract spec file not found: {path}"),
                        serde_json::json!({
                            "path": path,
                            "path_state": harvest_extract_spec_file_state(),
                            "reason": "inspect_harvest_extract_spec_file_not_found",
                        }),
                    ),
                    _ => RubError::domain_with_context(
                        ErrorCode::InvalidInput,
                        format!("Failed to read inspect harvest extract spec file {path}: {error}"),
                        serde_json::json!({
                            "path": path,
                            "path_state": harvest_extract_spec_file_state(),
                            "reason": "inspect_harvest_extract_spec_file_read_failed",
                        }),
                    ),
                })
        }
        (None, None, false) => {
            resolve_extract_builder_spec_source("inspect harvest", fields).map(|(spec, _)| spec)
        }
        (Some(_), Some(_), _) | (Some(_), _, false) | (_, Some(_), false) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Use exactly one follow-page extract source: --extract, --extract-file, or one or more --field entries",
        )),
        (None, None, true) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            "Provide --extract, --extract-file, or one or more --field entries for inspect harvest",
        )),
    }
}

pub(super) async fn load_harvest_sources(
    file: &Path,
    input_field: Option<&str>,
    url_field: Option<&str>,
    name_field: Option<&str>,
    base_url: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<HarvestSource>, RubError> {
    let raw = fs::read_to_string(file)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => RubError::domain_with_context(
                ErrorCode::FileNotFound,
                format!("inspect harvest source file not found: {}", file.display()),
                serde_json::json!({
                    "file": file.display().to_string(),
                    "file_state": harvest_source_file_state(),
                    "reason": "inspect_harvest_source_file_not_found",
                }),
            ),
            _ => RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!(
                    "Failed to read inspect harvest source file {}: {error}",
                    file.display()
                ),
                serde_json::json!({
                    "file": file.display().to_string(),
                    "file_state": harvest_source_file_state(),
                    "reason": "inspect_harvest_source_file_read_failed",
                }),
            ),
        })?;

    let parsed_sources = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(json) => parse_json_harvest_sources(file, &json, input_field, url_field, name_field)?,
        Err(_) => parse_text_harvest_sources(&raw),
    };
    if parsed_sources.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "inspect harvest source did not contain any follow-page URLs",
        ));
    }

    let mut resolved = Vec::new();
    for (index, source) in parsed_sources.into_iter().enumerate() {
        if let Some(limit) = limit
            && index as u32 >= limit
        {
            break;
        }
        resolved.push(HarvestSource {
            index: index as u32,
            url: resolve_follow_url(&source.url, base_url)?,
            source_name: source.source_name,
            source_row: source.source_row,
        });
    }
    Ok(resolved)
}

pub(super) fn harvest_source_file_state() -> PathReferenceState {
    PathReferenceState {
        truth_level: "input_path_reference".to_string(),
        path_authority: "cli.inspect_harvest.source_file".to_string(),
        upstream_truth: "cli_inspect_harvest_file_option".to_string(),
        path_kind: "harvest_source_file".to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(super) fn harvest_extract_spec_file_state() -> PathReferenceState {
    PathReferenceState {
        truth_level: "input_path_reference".to_string(),
        path_authority: "cli.inspect_harvest.extract_spec_file".to_string(),
        upstream_truth: "cli_inspect_harvest_extract_file_option".to_string(),
        path_kind: "harvest_extract_spec_file".to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(super) fn parse_json_harvest_sources(
    file: &Path,
    root: &serde_json::Value,
    input_field: Option<&str>,
    url_field: Option<&str>,
    name_field: Option<&str>,
) -> Result<Vec<ParsedHarvestSource>, RubError> {
    let selected = resolve_json_harvest_root(file, root, input_field)?;

    let rows = match selected {
        serde_json::Value::Array(values) => values,
        serde_json::Value::String(url) => {
            return Ok(vec![ParsedHarvestSource {
                url: url.clone(),
                source_name: None,
                source_row: serde_json::Value::String(url.clone()),
            }]);
        }
        _ => {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "inspect harvest JSON input must resolve to an array, string URL, or canonical batch object; use --input-field to select a batch root like data.result",
                serde_json::json!({
                    "file": file.display().to_string(),
                    "file_state": harvest_source_file_state(),
                    "input_field": input_field,
                }),
            ));
        }
    };

    rows.iter()
        .enumerate()
        .map(|(index, row)| parse_json_harvest_row(row, index, url_field, name_field))
        .collect()
}

pub(super) fn resolve_json_harvest_root<'a>(
    file: &Path,
    root: &'a serde_json::Value,
    input_field: Option<&str>,
) -> Result<&'a serde_json::Value, RubError> {
    if let Some(path) = input_field {
        let selected = lookup_json_path(root, path).ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("inspect harvest input_field '{path}' was not found in the JSON source"),
                serde_json::json!({
                    "file": file.display().to_string(),
                    "file_state": harvest_source_file_state(),
                    "input_field": path,
                }),
            )
        })?;
        return Ok(canonical_batch_root(selected).unwrap_or(selected));
    }

    if matches!(
        root,
        serde_json::Value::Array(_) | serde_json::Value::String(_)
    ) {
        return Ok(root);
    }

    if let Some(items) = canonical_batch_root(root) {
        return Ok(items);
    }

    for candidate in ["data.result", "result", "data"] {
        if let Some(value) = lookup_json_path(root, candidate)
            && let Some(selected) = canonical_batch_root(value).or(array_or_string_root(value))
        {
            return Ok(selected);
        }
    }

    Ok(root)
}

fn parse_json_harvest_row(
    row: &serde_json::Value,
    index: usize,
    url_field: Option<&str>,
    name_field: Option<&str>,
) -> Result<ParsedHarvestSource, RubError> {
    match row {
        serde_json::Value::String(url) => Ok(ParsedHarvestSource {
            url: url.clone(),
            source_name: None,
            source_row: row.clone(),
        }),
        serde_json::Value::Object(_) => {
            let url_value = match url_field {
                Some(path) => lookup_json_path(row, path),
                None => row.get("url").or_else(|| row.get("href")),
            }
            .ok_or_else(|| {
                RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!(
                        "inspect harvest row {index} did not expose a URL; use --url-field or include a top-level 'url'/'href' field"
                    ),
                    serde_json::json!({
                        "row_index": index,
                        "row": row,
                        "url_field": url_field,
                    }),
                )
            })?;
            let url = url_value.as_str().ok_or_else(|| {
                RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("inspect harvest row {index} URL field must be a string"),
                    serde_json::json!({
                        "row_index": index,
                        "row": row,
                        "url_field": url_field,
                    }),
                )
            })?;
            let source_name = name_field
                .and_then(|path| lookup_json_path(row, path))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Ok(ParsedHarvestSource {
                url: url.to_string(),
                source_name,
                source_row: row.clone(),
            })
        }
        _ => Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("inspect harvest row {index} must be a string URL or JSON object"),
            serde_json::json!({
                "row_index": index,
                "row": row,
            }),
        )),
    }
}

fn parse_text_harvest_sources(raw: &str) -> Vec<ParsedHarvestSource> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| ParsedHarvestSource {
            url: line.to_string(),
            source_name: None,
            source_row: serde_json::Value::String(line.to_string()),
        })
        .collect()
}

fn resolve_follow_url(url: &str, base_url: Option<&str>) -> Result<String, RubError> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(url.to_string());
    }
    let Some(base_url) = base_url else {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "inspect harvest encountered a relative URL; provide --base-url to resolve it",
            serde_json::json!({
                "url": url,
            }),
        ));
    };
    let base = reqwest::Url::parse(base_url).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("inspect harvest base URL is invalid: {error}"),
        )
    })?;
    let joined = base.join(url).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("inspect harvest could not resolve relative URL '{url}': {error}"),
            serde_json::json!({
                "base_url": base_url,
                "url": url,
            }),
        )
    })?;
    Ok(joined.to_string())
}

fn lookup_json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = current.get(segment)?;
    }
    Some(current)
}

fn canonical_batch_root(value: &serde_json::Value) -> Option<&serde_json::Value> {
    let items = value.get("items")?;
    match items {
        serde_json::Value::Array(_) | serde_json::Value::String(_) => Some(items),
        _ => None,
    }
}

fn array_or_string_root(value: &serde_json::Value) -> Option<&serde_json::Value> {
    match value {
        serde_json::Value::Array(_) | serde_json::Value::String(_) => Some(value),
        _ => None,
    }
}
