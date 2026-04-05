use std::path::Path;
use std::time::{Duration, Instant};

use crate::commands::{Commands, EffectiveCli, InspectSubcommand};
use crate::daemon_ctl;
use crate::session_policy::{
    materialize_connection_request, parse_connection_request, requires_existing_session_validation,
    resolve_attachment_identity, validate_existing_session_connection_request,
};
use crate::timeout_budget::helpers::resolve_extract_builder_spec_source;
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use serde::Serialize;
use tokio::fs;
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct FollowPageExtractSummary {
    complete: bool,
    source_count: u32,
    attempted_count: u32,
    harvested_count: u32,
    failed_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractSubject {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FollowPageExtractStatus {
    Harvested,
    Failed,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractSourceInfo {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    row: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractPageInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractEntryResult {
    fields: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractEntry {
    index: u32,
    status: FollowPageExtractStatus,
    source: FollowPageExtractSourceInfo,
    page: FollowPageExtractPageInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<FollowPageExtractEntryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorEnvelope>,
}

#[derive(Debug)]
struct HarvestSource {
    index: u32,
    url: String,
    source_name: Option<String>,
    source_row: serde_json::Value,
}

struct HarvestDispatchContext {
    daemon_args: Vec<String>,
    connection_request: crate::session_policy::ConnectionRequest,
    attachment_identity: Option<String>,
}

pub(crate) async fn inspect_harvest(cli: &EffectiveCli) -> Result<serde_json::Value, RubError> {
    let Commands::Inspect(InspectSubcommand::Harvest {
        file,
        input_field,
        url_field,
        name_field,
        base_url,
        extract,
        extract_file,
        field,
        limit,
    }) = &cli.command
    else {
        return Err(RubError::Internal(
            "inspect_harvest called for a non-harvest command".to_string(),
        ));
    };
    let deadline = Instant::now() + Duration::from_millis(cli.timeout.max(1));

    let extract_spec =
        load_extract_spec(extract.as_deref(), extract_file.as_deref(), field).await?;
    let sources = load_harvest_sources(
        Path::new(file),
        input_field.as_deref(),
        url_field.as_deref(),
        name_field.as_deref(),
        base_url.as_deref(),
        *limit,
    )
    .await?;

    let source_count = sources.len() as u32;
    let subject = FollowPageExtractSubject {
        kind: "follow_page_extract",
        input_field: input_field.clone(),
        url_field: url_field.clone(),
        name_field: name_field.clone(),
        base_url: base_url.clone(),
        limit: *limit,
    };
    let dispatch = HarvestDispatchContext::new(cli).await?;
    let mut attempted_count = 0u32;
    let mut harvested_count = 0u32;
    let mut failed_count = 0u32;
    let mut entries = Vec::with_capacity(sources.len());

    for source in sources {
        attempted_count = attempted_count.saturating_add(1);
        let open_request = IpcRequest::new(
            "open",
            serde_json::json!({
                "url": source.url,
                "load_strategy": "load",
            }),
            cli.timeout,
        )
        .with_command_id(Uuid::now_v7().to_string())
        .expect("UUID command_id must be valid");
        let open_data = match dispatch.send_request(cli, &open_request, deadline).await {
            Ok(data) => data,
            Err(error) => {
                failed_count = failed_count.saturating_add(1);
                entries.push(FollowPageExtractEntry {
                    index: source.index,
                    status: FollowPageExtractStatus::Failed,
                    source: FollowPageExtractSourceInfo {
                        url: source.url,
                        name: source.source_name,
                        row: source.source_row,
                    },
                    page: FollowPageExtractPageInfo {
                        final_url: None,
                        title: None,
                    },
                    result: None,
                    error: Some(error),
                });
                if entries
                    .last()
                    .and_then(|entry| entry.error.as_ref())
                    .is_some_and(harvest_deadline_exhausted)
                {
                    break;
                }
                continue;
            }
        };

        let extract_request = IpcRequest::new(
            "extract",
            serde_json::json!({
                "spec": extract_spec,
            }),
            cli.timeout,
        )
        .with_command_id(Uuid::now_v7().to_string())
        .expect("UUID command_id must be valid");
        match dispatch.send_request(cli, &extract_request, deadline).await {
            Ok(extract_data) => {
                harvested_count = harvested_count.saturating_add(1);
                entries.push(FollowPageExtractEntry {
                    index: source.index,
                    status: FollowPageExtractStatus::Harvested,
                    source: FollowPageExtractSourceInfo {
                        url: source.url,
                        name: source.source_name,
                        row: source.source_row,
                    },
                    page: FollowPageExtractPageInfo {
                        final_url: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("final_url"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        title: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("title"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                    },
                    result: extract_data
                        .get("result")
                        .and_then(|value| value.get("fields"))
                        .cloned()
                        .map(|fields| FollowPageExtractEntryResult { fields }),
                    error: None,
                });
            }
            Err(error) => {
                failed_count = failed_count.saturating_add(1);
                entries.push(FollowPageExtractEntry {
                    index: source.index,
                    status: FollowPageExtractStatus::Failed,
                    source: FollowPageExtractSourceInfo {
                        url: source.url,
                        name: source.source_name,
                        row: source.source_row,
                    },
                    page: FollowPageExtractPageInfo {
                        final_url: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("final_url"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        title: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("title"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                    },
                    result: None,
                    error: Some(error),
                });
                if entries
                    .last()
                    .and_then(|entry| entry.error.as_ref())
                    .is_some_and(harvest_deadline_exhausted)
                {
                    break;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "subject": subject,
        "result": {
            "summary": FollowPageExtractSummary {
                complete: failed_count == 0,
                source_count,
                attempted_count,
                harvested_count,
                failed_count,
                base_url: base_url.clone(),
            },
            "entries": entries,
        }
    }))
}

impl HarvestDispatchContext {
    async fn new(cli: &EffectiveCli) -> Result<Self, RubError> {
        let connection_request =
            materialize_connection_request(&parse_connection_request(cli)?).await?;
        Ok(Self {
            daemon_args: crate::daemon_args(cli, &connection_request),
            attachment_identity: resolve_attachment_identity(cli, &connection_request, None)
                .await?,
            connection_request,
        })
    }

    async fn send_request(
        &self,
        cli: &EffectiveCli,
        request: &IpcRequest,
        deadline: Instant,
    ) -> Result<serde_json::Value, ErrorEnvelope> {
        if remaining_harvest_budget_ms(deadline).is_none() {
            return Err(harvest_timeout_envelope(cli.timeout));
        }
        let bootstrap = daemon_ctl::bootstrap_client(
            &cli.rub_home,
            &cli.session,
            deadline,
            &self.daemon_args,
            self.attachment_identity.as_deref(),
        )
        .await
        .map_err(|error| error.into_envelope())?;
        if requires_existing_session_validation(
            bootstrap.connected_to_existing_daemon,
            &self.connection_request,
            cli,
        ) {
            validate_existing_session_connection_request(cli, &self.connection_request)
                .await
                .map_err(|error| error.into_envelope())?;
        }
        let daemon_session_id = bootstrap.daemon_session_id;
        let mut client = bootstrap.client;
        let response = daemon_ctl::send_request_with_replay_recovery(
            &mut client,
            request,
            deadline,
            daemon_ctl::ReplayRecoveryContext {
                rub_home: &cli.rub_home,
                session: &cli.session,
                daemon_args: &self.daemon_args,
                attachment_identity: self.attachment_identity.as_deref(),
                original_daemon_session_id: daemon_session_id.as_deref(),
            },
        )
        .await
        .map_err(|error| error.into_envelope())?;
        match response.status {
            ResponseStatus::Success => response.data.ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "missing success payload in success response",
                )
            }),
            ResponseStatus::Error => Err(response.error.unwrap_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "missing error envelope in error response",
                )
            })),
        }
    }
}

fn remaining_harvest_budget_ms(deadline: Instant) -> Option<u64> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = remaining.as_millis() as u64;
    (millis > 0).then_some(millis)
}

fn harvest_timeout_envelope(timeout_ms: u64) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!("inspect harvest exceeded overall timeout of {timeout_ms}ms"),
    )
    .with_context(serde_json::json!({
        "reason": "inspect_harvest_timeout_budget_exhausted",
        "timeout_ms": timeout_ms,
    }))
}

fn harvest_deadline_exhausted(error: &ErrorEnvelope) -> bool {
    error.code == ErrorCode::IpcTimeout
        && error
            .context
            .as_ref()
            .and_then(|value| value.get("reason"))
            .and_then(|value| value.as_str())
            == Some("inspect_harvest_timeout_budget_exhausted")
}

async fn load_extract_spec(
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
                    std::io::ErrorKind::NotFound => RubError::domain(
                        ErrorCode::FileNotFound,
                        format!("inspect harvest extract spec file not found: {path}"),
                    ),
                    _ => RubError::domain(
                        ErrorCode::InvalidInput,
                        format!("Failed to read inspect harvest extract spec file {path}: {error}"),
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

async fn load_harvest_sources(
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
            std::io::ErrorKind::NotFound => RubError::domain(
                ErrorCode::FileNotFound,
                format!("inspect harvest source file not found: {}", file.display()),
            ),
            _ => RubError::domain(
                ErrorCode::InvalidInput,
                format!(
                    "Failed to read inspect harvest source file {}: {error}",
                    file.display()
                ),
            ),
        })?;

    let parsed_sources = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(json) => parse_json_harvest_sources(file, &json, input_field, url_field, name_field)?,
        Err(_) => parse_text_harvest_sources(&raw)?,
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

#[derive(Debug)]
struct ParsedHarvestSource {
    url: String,
    source_name: Option<String>,
    source_row: serde_json::Value,
}

fn parse_json_harvest_sources(
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

fn resolve_json_harvest_root<'a>(
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

fn parse_text_harvest_sources(raw: &str) -> Result<Vec<ParsedHarvestSource>, RubError> {
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| ParsedHarvestSource {
            url: line.to_string(),
            source_name: None,
            source_row: serde_json::Value::String(line.to_string()),
        })
        .collect())
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

#[cfg(test)]
mod tests {
    use super::{
        harvest_deadline_exhausted, harvest_timeout_envelope, remaining_harvest_budget_ms,
        resolve_json_harvest_root,
    };
    use serde_json::json;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn resolve_json_harvest_root_accepts_canonical_batch_object_root() {
        let value = json!({
            "items": [
                { "href": "/detail/a" }
            ],
            "item_count": 1
        });
        let resolved = resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, None)
            .expect("canonical batch root");
        assert_eq!(resolved, &value["items"]);
    }

    #[test]
    fn resolve_json_harvest_root_auto_detects_canonical_result_batch_root() {
        let value = json!({
            "data": {
                "result": {
                    "items": [
                        { "href": "/detail/a" }
                    ],
                    "item_count": 1
                }
            }
        });
        let resolved = resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, None)
            .expect("canonical result batch");
        assert_eq!(resolved, &value["data"]["result"]["items"]);
    }

    #[test]
    fn resolve_json_harvest_root_accepts_explicit_canonical_batch_path() {
        let value = json!({
            "data": {
                "result": {
                    "items": [
                        { "href": "/detail/a" }
                    ],
                    "item_count": 1
                }
            }
        });
        let resolved =
            resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, Some("data.result"))
                .expect("explicit canonical batch path");
        assert_eq!(resolved, &value["data"]["result"]["items"]);
    }

    #[test]
    fn remaining_harvest_budget_is_none_once_deadline_has_elapsed() {
        let deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(remaining_harvest_budget_ms(deadline), None);
    }

    #[test]
    fn harvest_timeout_envelope_is_marked_as_terminal_deadline_exhaustion() {
        let envelope = harvest_timeout_envelope(5_000);
        assert!(harvest_deadline_exhausted(&envelope));
        assert_eq!(envelope.code, rub_core::error::ErrorCode::IpcTimeout);
    }
}
