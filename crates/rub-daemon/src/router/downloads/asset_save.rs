use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, REFERER};
use reqwest::{Client, StatusCode};
use rub_core::error::{ErrorCode, RubError};
use rub_core::fs::{commit_temporary_file, commit_temporary_file_no_clobber};
use rub_core::model::{BulkAssetSaveSummary, SavedAssetEntry, SavedAssetStatus};
use time::OffsetDateTime;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::DaemonRouter;

const DEFAULT_SAVE_CONCURRENCY: u32 = 6;
const MAX_SAVE_CONCURRENCY: u32 = 64;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DownloadSaveArgs {
    #[serde(rename = "sub")]
    _sub: String,
    file: String,
    output_dir: String,
    #[serde(default)]
    input_field: Option<String>,
    #[serde(default)]
    url_field: Option<String>,
    #[serde(default)]
    name_field: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    cookie_url: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default = "default_save_concurrency")]
    concurrency: u32,
    #[serde(default)]
    overwrite: bool,
    #[serde(default = "default_save_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug)]
struct DownloadSaveRequest {
    file: PathBuf,
    output_dir: PathBuf,
    input_field: Option<String>,
    url_field: Option<String>,
    name_field: Option<String>,
    base_url: Option<String>,
    cookie_url: Option<String>,
    limit: Option<u32>,
    concurrency: u32,
    overwrite: bool,
    timeout_ms: u64,
}

#[derive(Debug, Clone)]
struct AssetSource {
    index: u32,
    url: String,
    source_name: Option<String>,
    output_path: PathBuf,
}

#[derive(Debug, Clone)]
struct PreparedAssetSource {
    source: AssetSource,
    headers: Option<HeaderMap>,
}

struct AssetRequestAuthority<'a> {
    cookie_lookup_url: &'a str,
    referer_url: Option<&'a str>,
}

#[derive(Clone)]
struct SaveExecutionContext {
    client: Client,
    deadline: Instant,
    overwrite: bool,
    reserved_output_paths: Arc<Mutex<BTreeSet<PathBuf>>>,
}

struct DownloadSaveBatch {
    request: DownloadSaveRequest,
    deadline: Instant,
    client: Client,
}

fn default_save_concurrency() -> u32 {
    DEFAULT_SAVE_CONCURRENCY
}

fn default_save_timeout_ms() -> u64 {
    30_000
}

pub(super) async fn cmd_download_save(
    router: &DaemonRouter,
    args: DownloadSaveArgs,
) -> Result<serde_json::Value, RubError> {
    let batch = DownloadSaveBatch::new(DownloadSaveRequest::try_from(args)?)?;
    batch.run(router).await
}

impl TryFrom<DownloadSaveArgs> for DownloadSaveRequest {
    type Error = RubError;

    fn try_from(args: DownloadSaveArgs) -> Result<Self, Self::Error> {
        let concurrency = args.concurrency;
        if concurrency == 0 {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "download save --concurrency must be greater than 0",
            ));
        }
        if concurrency > MAX_SAVE_CONCURRENCY {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("download save --concurrency must be at most {MAX_SAVE_CONCURRENCY}"),
            ));
        }

        Ok(Self {
            file: PathBuf::from(args.file),
            output_dir: PathBuf::from(args.output_dir),
            input_field: args.input_field,
            url_field: args.url_field,
            name_field: args.name_field,
            base_url: args.base_url,
            cookie_url: args.cookie_url,
            limit: args.limit,
            concurrency,
            overwrite: args.overwrite,
            timeout_ms: args.timeout_ms,
        })
    }
}

impl DownloadSaveBatch {
    fn new(request: DownloadSaveRequest) -> Result<Self, RubError> {
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.max(1));
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|error| {
                RubError::Internal(format!("asset save client build failed: {error}"))
            })?;
        Ok(Self {
            request,
            deadline,
            client,
        })
    }

    async fn run(self, router: &DaemonRouter) -> Result<serde_json::Value, RubError> {
        self.ensure_output_dir().await?;
        let sources = self.prepare_sources(router).await?;
        let source_count = self.source_count(&sources);
        let (attempted_count, results) = self.execute_sources(sources).await;
        let summary = summarize_results(
            source_count,
            attempted_count,
            self.request.output_dir.as_path(),
            &results,
        );
        Ok(serde_json::json!({
            "subject": self.subject_projection(),
            "result": {
                "summary": summary,
                "entries": results,
            }
        }))
    }

    async fn ensure_output_dir(&self) -> Result<(), RubError> {
        fs::create_dir_all(&self.request.output_dir)
            .await
            .map_err(|error| {
                RubError::domain(
                    ErrorCode::InvalidInput,
                    format!(
                        "Failed to create asset output directory {}: {error}",
                        self.request.output_dir.display()
                    ),
                )
            })
    }

    async fn prepare_sources(
        &self,
        router: &DaemonRouter,
    ) -> Result<Vec<PreparedAssetSource>, RubError> {
        let Some(remaining) = self.deadline.checked_duration_since(Instant::now()) else {
            return Err(RubError::domain_with_context(
                ErrorCode::IpcTimeout,
                "download save timed out before asset preparation could begin",
                serde_json::json!({
                    "reason": "bulk_asset_save_deadline_exceeded_before_prepare",
                    "timeout_ms": self.request.timeout_ms,
                }),
            ));
        };
        match tokio::time::timeout(remaining, prepare_asset_sources(router, &self.request)).await {
            Ok(result) => result,
            Err(_) => Err(RubError::domain_with_context(
                ErrorCode::IpcTimeout,
                "download save timed out while preparing asset sources",
                serde_json::json!({
                    "reason": "bulk_asset_save_prepare_timed_out",
                    "timeout_ms": self.request.timeout_ms,
                }),
            )),
        }
    }

    async fn execute_sources(
        &self,
        sources: Vec<PreparedAssetSource>,
    ) -> (u32, Vec<SavedAssetEntry>) {
        let execution = self.execution_context(&sources);
        let mut pending = sources.into_iter();
        let mut results = Vec::new();
        let mut inflight = FuturesUnordered::new();
        let mut attempted_count = 0u32;
        let concurrency = self.request.concurrency.max(1) as usize;

        loop {
            while inflight.len() < concurrency {
                let Some(source) = pending.next() else {
                    break;
                };
                if execution
                    .deadline
                    .checked_duration_since(Instant::now())
                    .is_none()
                {
                    results.push(timeout_entry(
                        &source.source,
                        None,
                        "bulk_asset_save_deadline_exceeded",
                    ));
                    continue;
                }
                attempted_count = attempted_count.saturating_add(1);
                inflight.push(save_one(execution.clone(), source));
            }

            match inflight.next().await {
                Some(result) => results.push(result),
                None => break,
            }
        }

        results.sort_by_key(|entry| entry.index);
        (attempted_count, results)
    }

    fn execution_context(&self, sources: &[PreparedAssetSource]) -> SaveExecutionContext {
        SaveExecutionContext {
            client: self.client.clone(),
            deadline: self.deadline,
            overwrite: self.request.overwrite,
            reserved_output_paths: Arc::new(Mutex::new(
                sources
                    .iter()
                    .map(|source| source.source.output_path.clone())
                    .collect::<BTreeSet<_>>(),
            )),
        }
    }

    fn source_count(&self, sources: &[PreparedAssetSource]) -> u32 {
        self.request
            .limit
            .unwrap_or(u32::MAX)
            .min(sources.len().try_into().unwrap_or(u32::MAX))
    }

    fn subject_projection(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "bulk_asset_save",
            "output_dir": self.request.output_dir,
            "input_field": self.request.input_field,
            "url_field": self.request.url_field,
            "name_field": self.request.name_field,
            "base_url": self.request.base_url,
            "cookie_url": self.request.cookie_url,
            "limit": self.request.limit,
            "concurrency": self.request.concurrency,
            "overwrite": self.request.overwrite,
        })
    }
}

#[allow(dead_code)]
fn parse_save_request(args: &serde_json::Value) -> Result<DownloadSaveRequest, RubError> {
    let args: DownloadSaveArgs = serde_json::from_value(args.clone()).map_err(|error| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid download save payload: {error}"),
        )
    })?;
    DownloadSaveRequest::try_from(args)
}

async fn load_asset_sources(request: &DownloadSaveRequest) -> Result<Vec<AssetSource>, RubError> {
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

async fn prepare_asset_sources(
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

fn resolve_json_asset_root<'a>(
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

fn parse_text_asset_sources(raw: &str) -> Result<Vec<(String, Option<String>)>, RubError> {
    let rows = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| (line.to_string(), None))
        .collect::<Vec<_>>();
    Ok(rows)
}

fn resolve_asset_url(url: &str, base_url: Option<&str>) -> Result<String, RubError> {
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

async fn build_request_headers_for_asset(
    router: &DaemonRouter,
    cookie_url: Option<&str>,
    asset_url: &str,
) -> Result<Option<HeaderMap>, RubError> {
    let authority = asset_request_authority(cookie_url, asset_url);
    let Some(referer_url) = authority.referer_url else {
        return Ok(None);
    };
    // `--cookie-url` is request context / referer authority. The actual cookie
    // lookup must stay per-asset so browser jar policy continues to decide
    // whether that asset request is credentialed.
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

fn asset_request_authority<'a>(
    cookie_url: Option<&'a str>,
    asset_url: &'a str,
) -> AssetRequestAuthority<'a> {
    AssetRequestAuthority {
        cookie_lookup_url: asset_url,
        referer_url: cookie_url,
    }
}

async fn save_one(context: SaveExecutionContext, source: PreparedAssetSource) -> SavedAssetEntry {
    let PreparedAssetSource { source, headers } = source;
    let source_index = source.index;
    let source_url = source.url.clone();
    let source_name = source.source_name.clone();
    let planned_output_path = source.output_path.clone();
    let timeout_source = AssetSource {
        index: source_index,
        url: source_url.clone(),
        source_name: source_name.clone(),
        output_path: planned_output_path.clone(),
    };
    let temp_path_slot = Arc::new(Mutex::new(None::<PathBuf>));
    let output_path_slot = Arc::new(Mutex::new(None::<PathBuf>));
    let Some(timeout) = context.deadline.checked_duration_since(Instant::now()) else {
        return timeout_entry(
            &timeout_source,
            None,
            "asset_request_timed_out_before_download_started",
        );
    };
    let future = async {
        let mut request = context.client.get(&source_url);
        if let Some(headers) = headers.clone() {
            request = request.headers(headers);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                return Err(error.to_string());
            }
        };
        if response.status() != StatusCode::OK {
            return Err(format!("http_status:{}", response.status()));
        }

        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let mut stream = response.bytes_stream();
        let first_chunk = stream
            .next()
            .await
            .transpose()
            .map_err(|error| error.to_string())?;
        let output_path = reserve_reconciled_output_path(
            &planned_output_path,
            content_type.as_ref(),
            first_chunk.as_deref(),
            context.reserved_output_paths.as_ref(),
        );
        set_tracked_output_path(&output_path_slot, Some(output_path.clone()));
        if !context.overwrite && output_path.exists() {
            return Ok(SaveOneOutcome::SkippedExisting { output_path });
        }
        let tmp_path = temporary_path(&output_path);
        set_tracked_temp_path(&temp_path_slot, Some(tmp_path.clone()));
        if let Some(parent) = tmp_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| error.to_string())?;
        }
        let mut file = fs::File::create(&tmp_path)
            .await
            .map_err(|error| error.to_string())?;
        let mut bytes_written = 0u64;
        if let Some(chunk) = first_chunk {
            file.write_all(&chunk)
                .await
                .map_err(|error| error.to_string())?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| error.to_string())?;
            file.write_all(&chunk)
                .await
                .map_err(|error| error.to_string())?;
            bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        }
        file.flush().await.map_err(|error| error.to_string())?;
        drop(file);
        Ok(SaveOneOutcome::Prepared {
            output_path,
            tmp_path,
            bytes_written,
        })
    };

    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(SaveOneOutcome::Prepared {
            output_path,
            tmp_path,
            bytes_written,
        })) => {
            if context
                .deadline
                .checked_duration_since(Instant::now())
                .is_none()
            {
                let _ = fs::remove_file(&tmp_path).await;
                set_tracked_temp_path(&temp_path_slot, None);
                return timeout_entry(
                    &timeout_source,
                    Some(output_path),
                    "asset_commit_deadline_exceeded_before_commit",
                );
            }
            match tokio::task::spawn_blocking({
                let tmp_path = tmp_path.clone();
                let output_path = output_path.clone();
                move || {
                    if context.overwrite {
                        commit_temporary_file(&tmp_path, &output_path)
                    } else {
                        commit_temporary_file_no_clobber(&tmp_path, &output_path)
                    }
                }
            })
            .await
            {
                Ok(Ok(commit_outcome)) => {
                    set_tracked_temp_path(&temp_path_slot, None);
                    SavedAssetEntry {
                        index: source_index,
                        url: source_url.clone(),
                        status: SavedAssetStatus::Saved,
                        output_path: output_path.display().to_string(),
                        source_name: source_name.clone(),
                        bytes_written: Some(bytes_written),
                        durability_confirmed: Some(commit_outcome.durability_confirmed())
                            .filter(|confirmed| !confirmed),
                        error: None,
                    }
                }
                Ok(Err(error)) => {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        let _ = fs::remove_file(&tmp_path).await;
                        set_tracked_temp_path(&temp_path_slot, None);
                        return SavedAssetEntry {
                            index: source_index,
                            url: source_url.clone(),
                            status: SavedAssetStatus::SkippedExisting,
                            output_path: output_path.display().to_string(),
                            source_name: source_name.clone(),
                            bytes_written: None,
                            durability_confirmed: None,
                            error: None,
                        };
                    }
                    let _ = fs::remove_file(&tmp_path).await;
                    set_tracked_temp_path(&temp_path_slot, None);
                    SavedAssetEntry {
                        index: source_index,
                        url: source_url.clone(),
                        status: SavedAssetStatus::Failed,
                        output_path: output_path.display().to_string(),
                        source_name: source_name.clone(),
                        bytes_written: None,
                        durability_confirmed: None,
                        error: Some(error.to_string()),
                    }
                }
                Err(join_error) => {
                    let _ = fs::remove_file(&tmp_path).await;
                    set_tracked_temp_path(&temp_path_slot, None);
                    SavedAssetEntry {
                        index: source_index,
                        url: source_url.clone(),
                        status: SavedAssetStatus::Failed,
                        output_path: output_path.display().to_string(),
                        source_name: source_name.clone(),
                        bytes_written: None,
                        durability_confirmed: None,
                        error: Some(join_error.to_string()),
                    }
                }
            }
        }
        Ok(Ok(SaveOneOutcome::SkippedExisting { output_path })) => SavedAssetEntry {
            index: source_index,
            url: source_url.clone(),
            status: SavedAssetStatus::SkippedExisting,
            output_path: output_path.display().to_string(),
            source_name: source_name.clone(),
            bytes_written: None,
            durability_confirmed: None,
            error: None,
        },
        Ok(Err(error)) => {
            cleanup_tracked_temp_path(&temp_path_slot).await;
            let output_path = tracked_output_path(&output_path_slot)
                .unwrap_or_else(|| planned_output_path.clone());
            SavedAssetEntry {
                index: source_index,
                url: source_url.clone(),
                status: SavedAssetStatus::Failed,
                output_path: output_path.display().to_string(),
                source_name: source_name.clone(),
                bytes_written: None,
                durability_confirmed: None,
                error: Some(error),
            }
        }
        Err(_) => {
            cleanup_tracked_temp_path(&temp_path_slot).await;
            timeout_entry(
                &timeout_source,
                tracked_output_path(&output_path_slot),
                "asset_request_timed_out",
            )
        }
    }
}

enum SaveOneOutcome {
    Prepared {
        output_path: PathBuf,
        tmp_path: PathBuf,
        bytes_written: u64,
    },
    SkippedExisting {
        output_path: PathBuf,
    },
}

fn summarize_results(
    source_count: u32,
    attempted_count: u32,
    output_dir: &Path,
    results: &[SavedAssetEntry],
) -> BulkAssetSaveSummary {
    let mut saved_count = 0u32;
    let mut skipped_existing_count = 0u32;
    let mut failed_count = 0u32;
    let mut timed_out_count = 0u32;

    for result in results {
        match result.status {
            SavedAssetStatus::Saved => saved_count = saved_count.saturating_add(1),
            SavedAssetStatus::SkippedExisting => {
                skipped_existing_count = skipped_existing_count.saturating_add(1)
            }
            SavedAssetStatus::Failed => failed_count = failed_count.saturating_add(1),
            SavedAssetStatus::TimedOut => timed_out_count = timed_out_count.saturating_add(1),
        }
    }

    BulkAssetSaveSummary {
        complete: timed_out_count == 0,
        source_count,
        attempted_count,
        saved_count,
        skipped_existing_count,
        failed_count,
        timed_out_count,
        output_dir: output_dir.display().to_string(),
    }
}

fn timeout_entry(
    source: &AssetSource,
    output_path: Option<PathBuf>,
    reason: &str,
) -> SavedAssetEntry {
    SavedAssetEntry {
        index: source.index,
        url: source.url.clone(),
        status: SavedAssetStatus::TimedOut,
        output_path: output_path
            .unwrap_or_else(|| source.output_path.clone())
            .display()
            .to_string(),
        source_name: source.source_name.clone(),
        bytes_written: None,
        durability_confirmed: None,
        error: Some(reason.to_string()),
    }
}

fn planned_output_path(
    output_dir: &Path,
    url: &str,
    source_name: Option<&str>,
    reserved_names: &mut BTreeMap<String, u32>,
) -> PathBuf {
    let mut file_name = build_base_filename(url, source_name);
    if let Some(existing) = reserved_names.get_mut(&file_name) {
        *existing += 1;
        file_name = with_numeric_suffix(&file_name, *existing);
    } else {
        reserved_names.insert(file_name.clone(), 1);
    }
    output_dir.join(file_name)
}

fn reconcile_output_path(path: &Path, content_type: Option<&HeaderValue>) -> PathBuf {
    let current_extension = path.extension().and_then(|value| value.to_str());
    if !matches!(current_extension, Some("bin")) {
        return path.to_path_buf();
    }
    let Some(inferred_extension) = content_type.and_then(content_type_extension) else {
        return path.to_path_buf();
    };
    path.with_extension(inferred_extension)
}

fn reconcile_output_path_with_bytes(
    path: &Path,
    content_type: Option<&HeaderValue>,
    first_chunk: Option<&[u8]>,
) -> PathBuf {
    let reconciled = reconcile_output_path(path, content_type);
    let current_extension = reconciled.extension().and_then(|value| value.to_str());
    if !matches!(current_extension, Some("bin")) {
        return reconciled;
    }
    let Some(inferred_extension) = first_chunk.and_then(sniff_content_extension) else {
        return reconciled;
    };
    reconciled.with_extension(inferred_extension)
}

fn reserve_reconciled_output_path(
    planned_path: &Path,
    content_type: Option<&HeaderValue>,
    first_chunk: Option<&[u8]>,
    reserved_output_paths: &Mutex<BTreeSet<PathBuf>>,
) -> PathBuf {
    let reconciled = reconcile_output_path_with_bytes(planned_path, content_type, first_chunk);
    let mut reserved_paths = reserved_output_paths
        .lock()
        .expect("reserved output paths mutex should not be poisoned");
    reserved_paths.remove(planned_path);
    if !reserved_paths.contains(&reconciled) {
        reserved_paths.insert(reconciled.clone());
        return reconciled;
    }
    let unique = reserve_unique_output_path(&reconciled, &reserved_paths);
    reserved_paths.insert(unique.clone());
    unique
}

fn reserve_unique_output_path(path: &Path, reserved_paths: &BTreeSet<PathBuf>) -> PathBuf {
    if !reserved_paths.contains(path) {
        return path.to_path_buf();
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    let mut sequence = 2u32;
    loop {
        let candidate = path.with_file_name(with_numeric_suffix(file_name, sequence));
        if !reserved_paths.contains(&candidate) {
            return candidate;
        }
        sequence = sequence.saturating_add(1);
    }
}

fn build_base_filename(url: &str, source_name: Option<&str>) -> String {
    let parsed = reqwest::Url::parse(url).ok();
    let url_segment = parsed
        .as_ref()
        .and_then(|url| url.path_segments())
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .unwrap_or("asset");
    let url_segment = sanitize_filename(url_segment);
    let url_extension = Path::new(&url_segment)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .map(str::to_string);
    let url_stem = Path::new(&url_segment)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(sanitize_filename)
        .filter(|stem| !stem.is_empty());

    let (source_stem, source_extension) = source_name
        .map(sanitize_filename)
        .filter(|value| !value.is_empty())
        .map(split_known_extension)
        .unwrap_or_else(|| ("".to_string(), None));

    let base = if is_meaningful_filename_stem(&source_stem) {
        source_stem
    } else {
        url_stem.unwrap_or_else(|| "asset".to_string())
    };
    let extension = url_extension
        .or(source_extension)
        .unwrap_or_else(|| "bin".to_string());
    format!("{base}.{extension}")
}

fn split_known_extension(value: String) -> (String, Option<String>) {
    let path = Path::new(&value);
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let extension = path.extension().and_then(|ext| ext.to_str());
    match (stem, extension.and_then(normalize_known_extension)) {
        (Some(stem), Some(extension)) if !stem.is_empty() => (stem.to_string(), Some(extension)),
        _ => (value, None),
    }
}

fn normalize_known_extension(extension: &str) -> Option<String> {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("jpg".to_string()),
        "png" => Some("png".to_string()),
        "gif" => Some("gif".to_string()),
        "webp" => Some("webp".to_string()),
        "avif" => Some("avif".to_string()),
        "heic" => Some("heic".to_string()),
        "heif" => Some("heif".to_string()),
        "svg" => Some("svg".to_string()),
        "json" => Some("json".to_string()),
        "txt" => Some("txt".to_string()),
        "html" | "htm" => Some("html".to_string()),
        "bin" => Some("bin".to_string()),
        _ => None,
    }
}

fn is_meaningful_filename_stem(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let mut alphabetic_count = 0usize;
    let mut digit_count = 0usize;
    for ch in value.chars() {
        if ch.is_alphabetic() {
            alphabetic_count += 1;
        } else if ch.is_ascii_digit() {
            digit_count += 1;
        }
    }
    alphabetic_count > 0 || digit_count >= 3
}

fn content_type_extension(content_type: &HeaderValue) -> Option<&'static str> {
    let value = content_type.to_str().ok()?;
    let media_type = value
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/avif" => Some("avif"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/svg+xml" => Some("svg"),
        "application/json" => Some("json"),
        "text/plain" => Some("txt"),
        "text/html" => Some("html"),
        _ => None,
    }
}

fn sniff_content_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    if bytes.len() >= 12 && &bytes[4..12] == b"ftypavif" {
        return Some("avif");
    }
    if bytes.len() >= 12 && &bytes[4..12] == b"ftypheic" {
        return Some("heic");
    }
    if bytes.len() >= 12 && &bytes[4..12] == b"ftypheif" {
        return Some("heif");
    }
    None
}

fn with_numeric_suffix(file_name: &str, sequence: u32) -> String {
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("asset");
    let ext = path.extension().and_then(|ext| ext.to_str());
    match ext {
        Some(ext) => format!("{stem}-{sequence}.{ext}"),
        None => format!("{stem}-{sequence}"),
    }
}

fn sanitize_filename(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            _ if ch.is_alphanumeric() => ch,
            '.' | '_' | '-' => ch,
            ' ' => '_',
            _ => '_',
        })
        .collect::<String>()
        .trim_matches(|ch| ch == '_' || ch == '.')
        .to_string()
}

fn temporary_path(path: &Path) -> PathBuf {
    let stamp = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    path.with_file_name(format!("{file_name}.part-{stamp}"))
}

fn set_tracked_temp_path(slot: &Arc<Mutex<Option<PathBuf>>>, value: Option<PathBuf>) {
    *slot.lock().expect("temp path slot poisoned") = value;
}

async fn cleanup_tracked_temp_path(slot: &Arc<Mutex<Option<PathBuf>>>) {
    let path = {
        let mut guard = slot.lock().expect("temp path slot poisoned");
        guard.take()
    };
    if let Some(path) = path {
        let _ = fs::remove_file(path).await;
    }
}

fn set_tracked_output_path(slot: &Arc<Mutex<Option<PathBuf>>>, path: Option<PathBuf>) {
    if let Ok(mut guard) = slot.lock() {
        *guard = path;
    }
}

fn tracked_output_path(slot: &Arc<Mutex<Option<PathBuf>>>) -> Option<PathBuf> {
    slot.lock().ok().and_then(|guard| guard.clone())
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
        DownloadSaveRequest, MAX_SAVE_CONCURRENCY, asset_request_authority, build_base_filename,
        cleanup_tracked_temp_path, content_type_extension, is_meaningful_filename_stem,
        lookup_json_path, normalize_known_extension, parse_save_request, parse_text_asset_sources,
        reconcile_output_path, reconcile_output_path_with_bytes, reserve_reconciled_output_path,
        resolve_asset_url, resolve_json_asset_root, sanitize_filename, set_tracked_temp_path,
        sniff_content_extension, split_known_extension, with_numeric_suffix,
    };
    use reqwest::header::HeaderValue;
    use rub_core::error::ErrorCode;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use time::OffsetDateTime;

    #[test]
    fn build_base_filename_uses_source_name_and_url_extension() {
        assert_eq!(
            build_base_filename("https://example.test/assets/photo.jpg", Some("card-1")),
            "card-1.jpg"
        );
        assert_eq!(
            build_base_filename("https://example.test/assets/photo", Some("card 1")),
            "card_1.bin"
        );
        assert_eq!(
            build_base_filename(
                "https://example.test/assets/photo.webp",
                Some("sample-content-title-v2")
            ),
            "sample-content-title-v2.webp"
        );
        assert_eq!(
            build_base_filename("https://example.test/assets/photo.webp", Some("2.0")),
            "photo.webp"
        );
        assert_eq!(
            build_base_filename("https://example.test/assets/photo", Some("cover.jpg")),
            "cover.jpg"
        );
    }

    #[test]
    fn reconcile_output_path_uses_content_type_when_name_is_bin() {
        let path = PathBuf::from("/tmp/card_1.bin");
        assert_eq!(
            reconcile_output_path(&path, Some(&HeaderValue::from_static("image/webp"))),
            PathBuf::from("/tmp/card_1.webp")
        );
        assert_eq!(
            reconcile_output_path(
                &PathBuf::from("/tmp/card_1.jpg"),
                Some(&HeaderValue::from_static("image/webp"))
            ),
            PathBuf::from("/tmp/card_1.jpg")
        );
        assert_eq!(
            content_type_extension(&HeaderValue::from_static("image/jpeg; charset=binary")),
            Some("jpg")
        );
        assert_eq!(
            sniff_content_extension(b"RIFF\x96d\x01\x00WEBPVP8 "),
            Some("webp")
        );
        assert_eq!(
            reconcile_output_path_with_bytes(&path, None, Some(b"RIFF\x96d\x01\x00WEBPVP8 ")),
            PathBuf::from("/tmp/card_1.webp")
        );
    }

    #[test]
    fn parse_text_asset_sources_ignores_blank_lines() {
        let rows = parse_text_asset_sources("https://a\n\nhttps://b\n").expect("plain text");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "https://a");
        assert_eq!(rows[1].0, "https://b");
    }

    #[test]
    fn json_path_lookup_supports_nested_fields() {
        let value = json!({ "fields": { "items": [{ "url": "https://example.test/a.png" }] } });
        assert_eq!(
            lookup_json_path(&value, "fields.items")
                .and_then(|value| value.as_array())
                .map(|items| items.len()),
            Some(1)
        );
    }

    #[test]
    fn resolve_json_asset_root_accepts_canonical_batch_object_root() {
        let value = json!({
            "items": [
                { "url": "https://example.test/a.png" }
            ],
            "item_count": 1
        });
        let request = test_download_save_request(None);
        let resolved = resolve_json_asset_root(&request, &value).expect("canonical batch root");
        assert_eq!(resolved, &value["items"]);
    }

    #[test]
    fn resolve_json_asset_root_auto_detects_canonical_result_batch_root() {
        let value = json!({
            "data": {
                "result": {
                    "items": [
                        { "url": "https://example.test/a.png" }
                    ],
                    "item_count": 1
                }
            }
        });
        let request = test_download_save_request(None);
        let resolved =
            resolve_json_asset_root(&request, &value).expect("canonical result batch root");
        assert_eq!(resolved, &value["data"]["result"]["items"]);
    }

    #[test]
    fn resolve_json_asset_root_accepts_explicit_canonical_batch_path() {
        let value = json!({
            "data": {
                "result": {
                    "items": [
                        { "url": "https://example.test/a.png" }
                    ],
                    "item_count": 1
                }
            }
        });
        let request = test_download_save_request(Some("data.result"));
        let resolved =
            resolve_json_asset_root(&request, &value).expect("explicit canonical batch root");
        assert_eq!(resolved, &value["data"]["result"]["items"]);
    }

    #[test]
    fn parse_save_request_rejects_concurrency_beyond_backpressure_cap() {
        let error = parse_save_request(&json!({
            "file": "downloads.json",
            "concurrency": u64::from(MAX_SAVE_CONCURRENCY) + 1,
        }))
        .expect_err("oversized concurrency must fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_asset_url_requires_base_url_for_relative_sources() {
        let error = resolve_asset_url("media/cache/cover.jpg", None).expect_err("relative URL");
        assert!(matches!(
            error,
            rub_core::error::RubError::Domain(ref envelope) if envelope.code == ErrorCode::InvalidInput
        ));
        assert!(error.to_string().contains("provide --base-url"), "{error}");
        assert_eq!(
            resolve_asset_url("media/cache/cover.jpg", Some("https://books.toscrape.com/"))
                .expect("resolved relative URL"),
            "https://books.toscrape.com/media/cache/cover.jpg"
        );
    }

    #[test]
    fn filename_helpers_sanitize_and_suffix() {
        assert_eq!(sanitize_filename("hello world!.png"), "hello_world_.png");
        assert_eq!(sanitize_filename("article-title_2.0"), "article-title_2.0");
        assert_eq!(with_numeric_suffix("asset.png", 2), "asset-2.png");
        assert_eq!(normalize_known_extension("jpeg"), Some("jpg".to_string()));
        assert_eq!(
            split_known_extension("cover.jpg".to_string()),
            ("cover".to_string(), Some("jpg".to_string()))
        );
        assert!(is_meaningful_filename_stem("article_123"));
        assert!(!is_meaningful_filename_stem("2.0"));
    }

    #[test]
    fn reconciled_output_path_reserves_unique_name_when_extension_inference_collides() {
        let planned = Mutex::new(BTreeSet::from([
            PathBuf::from("/tmp/alpha.bin"),
            PathBuf::from("/tmp/alpha.webp"),
        ]));
        assert_eq!(
            reserve_reconciled_output_path(
                &PathBuf::from("/tmp/alpha.bin"),
                Some(&HeaderValue::from_static("image/webp")),
                None,
                &planned,
            ),
            PathBuf::from("/tmp/alpha-2.webp")
        );
        assert_eq!(
            reserve_reconciled_output_path(
                &PathBuf::from("/tmp/alpha.webp"),
                Some(&HeaderValue::from_static("image/webp")),
                None,
                &planned,
            ),
            PathBuf::from("/tmp/alpha.webp")
        );
    }

    fn test_download_save_request(input_field: Option<&str>) -> DownloadSaveRequest {
        DownloadSaveRequest {
            file: PathBuf::from("/tmp/assets.json"),
            output_dir: PathBuf::from("/tmp/output"),
            input_field: input_field.map(str::to_string),
            url_field: None,
            name_field: None,
            base_url: None,
            cookie_url: None,
            limit: None,
            concurrency: 1,
            overwrite: false,
            timeout_ms: 1_000,
        }
    }

    #[test]
    fn reconciled_output_path_handles_cascading_extension_collisions() {
        let reserved = Mutex::new(BTreeSet::from([
            PathBuf::from("/tmp/card.bin"),
            PathBuf::from("/tmp/card-2.bin"),
            PathBuf::from("/tmp/card.webp"),
        ]));
        let first = reserve_reconciled_output_path(
            &PathBuf::from("/tmp/card.bin"),
            Some(&HeaderValue::from_static("image/webp")),
            None,
            &reserved,
        );
        let second = reserve_reconciled_output_path(
            &PathBuf::from("/tmp/card-2.bin"),
            Some(&HeaderValue::from_static("image/webp")),
            None,
            &reserved,
        );
        assert_eq!(first, PathBuf::from("/tmp/card-2.webp"));
        assert_eq!(second, PathBuf::from("/tmp/card-2-2.webp"));
    }

    #[tokio::test]
    async fn tracked_temp_cleanup_removes_actual_tmp_path() {
        let temp_root = std::env::temp_dir().join(format!(
            "rub-asset-save-test-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();
        let temp_path = temp_root.join("alpha.webp.part-1");
        std::fs::write(&temp_path, "partial").unwrap();
        let slot = Arc::new(Mutex::new(None::<PathBuf>));
        set_tracked_temp_path(&slot, Some(temp_path.clone()));
        cleanup_tracked_temp_path(&slot).await;
        assert!(!temp_path.exists());
        assert!(slot.lock().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn asset_request_authority_keeps_cookie_lookup_on_asset_url_contract() {
        let authority = asset_request_authority(
            Some("https://app.example.test/feed"),
            "https://cdn.example.test/image.png",
        );
        assert_eq!(
            authority.cookie_lookup_url,
            "https://cdn.example.test/image.png"
        );
        assert_eq!(authority.referer_url, Some("https://app.example.test/feed"));
    }
}
