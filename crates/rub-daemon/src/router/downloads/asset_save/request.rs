use std::collections::BTreeMap;
use std::path::PathBuf;

use reqwest::header::{COOKIE, HeaderMap, HeaderValue, REFERER};
use rub_core::error::{ErrorCode, RubError};
use tokio::fs;

use crate::router::DaemonRouter;

use super::paths::planned_output_path;
use super::{DownloadSaveArgs, DownloadSaveRequest};

#[derive(Debug, Clone)]
pub(super) struct AssetSource {
    pub(super) index: u32,
    pub(super) url: String,
    pub(super) source_name: Option<String>,
    pub(super) output_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct PreparedAssetSource {
    pub(super) source: AssetSource,
    pub(super) headers: Option<HeaderMap>,
}

pub(super) struct AssetRequestAuthority<'a> {
    pub(super) cookie_lookup_url: &'a str,
    pub(super) referer_url: Option<&'a str>,
}

#[allow(dead_code)]
pub(super) fn parse_save_request(
    args: &serde_json::Value,
) -> Result<DownloadSaveRequest, RubError> {
    let args: DownloadSaveArgs = serde_json::from_value(args.clone()).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid download save payload: {error}"),
        )
    })?;
    DownloadSaveRequest::try_from(args)
}

pub(super) async fn load_asset_sources(
    request: &DownloadSaveRequest,
) -> Result<Vec<AssetSource>, RubError> {
    let raw = fs::read_to_string(&request.file)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => RubError::domain(
                ErrorCode::FileNotFound,
                format!("Asset source file not found: {}", request.file.display()),
            ),
            _ => RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Failed to read asset source file {}: {error}",
                    request.file.display()
                ),
            ),
        })?;

    let parsed_sources = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(json) => parse_json_asset_sources(request, &json)?,
        Err(_) => parse_text_asset_sources(&raw)?,
    };
    if parsed_sources.is_empty() {
        return Err(RubError::domain(
            ErrorCode::InvalidInput,
            "download save source did not contain any asset URLs",
        ));
    }

    let limited = if let Some(limit) = request.limit {
        parsed_sources
            .into_iter()
            .take(limit as usize)
            .collect::<Vec<_>>()
    } else {
        parsed_sources
    };
    let mut reserved_names = BTreeMap::new();
    limited
        .into_iter()
        .enumerate()
        .map(|(index, (url, source_name))| {
            let resolved_url = resolve_asset_url(&url, request.base_url.as_deref())?;
            Ok(AssetSource {
                index: index as u32,
                output_path: planned_output_path(
                    &request.output_dir,
                    &resolved_url,
                    source_name.as_deref(),
                    &mut reserved_names,
                ),
                url: resolved_url,
                source_name,
            })
        })
        .collect::<Result<Vec<_>, RubError>>()
}

pub(super) async fn prepare_asset_sources(
    router: &DaemonRouter,
    request: &DownloadSaveRequest,
) -> Result<Vec<PreparedAssetSource>, RubError> {
    let mut prepared = Vec::new();
    for source in load_asset_sources(request).await? {
        let headers =
            build_request_headers_for_asset(router, request.cookie_url.as_deref(), &source.url)
                .await?;
        prepared.push(PreparedAssetSource { source, headers });
    }
    Ok(prepared)
}

fn parse_json_asset_sources(
    request: &DownloadSaveRequest,
    root: &serde_json::Value,
) -> Result<Vec<(String, Option<String>)>, RubError> {
    let selected = resolve_json_asset_root(request, root)?;

    let rows = match selected {
        serde_json::Value::Array(values) => values,
        serde_json::Value::String(url) => {
            return Ok(vec![(url.clone(), None)]);
        }
        _ => {
            return Err(RubError::domain_with_context(
                ErrorCode::InvalidInput,
                "download save JSON input must resolve to an array, string URL, or canonical batch object; use --input-field to select a batch root like data.result",
                serde_json::json!({
                    "file": request.file.display().to_string(),
                    "input_field": request.input_field,
                }),
            ));
        }
    };

    rows.iter()
        .enumerate()
        .map(|(index, row)| parse_json_asset_row(request, row, index))
        .collect()
}

fn parse_json_asset_row(
    request: &DownloadSaveRequest,
    row: &serde_json::Value,
    index: usize,
) -> Result<(String, Option<String>), RubError> {
    match row {
        serde_json::Value::String(url) => Ok((url.clone(), None)),
        serde_json::Value::Object(_) => {
            let url_value = match request.url_field.as_deref() {
                Some(path) => lookup_json_path(row, path),
                None => row.get("url").or_else(|| row.get("href")),
            }
            .ok_or_else(|| {
                RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!(
                        "download save row {index} did not expose a URL; use --url-field or include a top-level 'url'/'href' field"
                    ),
                    serde_json::json!({
                        "row_index": index,
                        "row": row,
                        "url_field": request.url_field,
                    }),
                )
            })?;
            let url = url_value.as_str().ok_or_else(|| {
                RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("download save row {index} URL field must be a string"),
                    serde_json::json!({
                        "row_index": index,
                        "row": row,
                        "url_field": request.url_field,
                    }),
                )
            })?;
            let source_name = request
                .name_field
                .as_deref()
                .and_then(|path| lookup_json_path(row, path))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            Ok((url.to_string(), source_name))
        }
        _ => Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("download save row {index} must be a string URL or JSON object"),
            serde_json::json!({
                "row_index": index,
                "row": row,
            }),
        )),
    }
}

pub(super) fn resolve_json_asset_root<'a>(
    request: &DownloadSaveRequest,
    root: &'a serde_json::Value,
) -> Result<&'a serde_json::Value, RubError> {
    if let Some(path) = request.input_field.as_deref() {
        let selected = lookup_json_path(root, path).ok_or_else(|| {
            RubError::domain_with_context(
                ErrorCode::InvalidInput,
                format!("download save input_field '{path}' was not found in the JSON source"),
                serde_json::json!({
                    "file": request.file.display().to_string(),
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

pub(super) fn parse_text_asset_sources(
    raw: &str,
) -> Result<Vec<(String, Option<String>)>, RubError> {
    let rows = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| (line.to_string(), None))
        .collect::<Vec<_>>();
    Ok(rows)
}

pub(super) fn resolve_asset_url(url: &str, base_url: Option<&str>) -> Result<String, RubError> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(url.to_string());
    }
    let Some(base_url) = base_url else {
        return Err(RubError::domain_with_context(
            ErrorCode::InvalidInput,
            "download save encountered a relative URL; provide --base-url to resolve it",
            serde_json::json!({
                "url": url,
            }),
        ));
    };
    let base = reqwest::Url::parse(base_url).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("download save base URL is invalid: {error}"),
        )
    })?;
    let joined = base.join(url).map_err(|error| {
        RubError::domain_with_context(
            ErrorCode::InvalidInput,
            format!("download save could not resolve relative URL '{url}': {error}"),
            serde_json::json!({
                "base_url": base_url,
                "url": url,
            }),
        )
    })?;
    Ok(joined.to_string())
}

pub(super) async fn build_request_headers_for_asset(
    router: &DaemonRouter,
    cookie_url: Option<&str>,
    asset_url: &str,
) -> Result<Option<HeaderMap>, RubError> {
    let authority = asset_request_authority(cookie_url, asset_url);
    let Some(referer_url) = authority.referer_url else {
        return Ok(None);
    };
    let cookies = router
        .browser
        .get_cookies(Some(authority.cookie_lookup_url))
        .await?;
    let mut headers = HeaderMap::new();
    let cookie_header = cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ");
    if !cookie_header.is_empty() {
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&cookie_header).map_err(|error| {
                RubError::Internal(format!("cookie header serialization failed: {error}"))
            })?,
        );
    }
    headers.insert(
        REFERER,
        HeaderValue::from_str(referer_url).map_err(|error| {
            RubError::Internal(format!("referer header serialization failed: {error}"))
        })?,
    );
    Ok(Some(headers))
}

pub(super) fn asset_request_authority<'a>(
    cookie_url: Option<&'a str>,
    asset_url: &'a str,
) -> AssetRequestAuthority<'a> {
    AssetRequestAuthority {
        cookie_lookup_url: asset_url,
        referer_url: cookie_url,
    }
}

pub(super) fn lookup_json_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
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
