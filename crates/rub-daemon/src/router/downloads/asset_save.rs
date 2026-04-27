use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::SavedAssetEntry;
use tokio::fs;

use super::DaemonRouter;
use crate::router::TransactionDeadline;

mod execute;
mod paths;
mod request;
mod summary;

use self::execute::{SaveExecutionContext, save_one, timeout_entry};
use self::paths::bulk_output_dir_state_json;
use self::request::{PreparedAssetSource, prepare_asset_sources};
use self::summary::summarize_results;

const DEFAULT_SAVE_CONCURRENCY: u32 = 6;
const MAX_SAVE_CONCURRENCY: u32 = 64;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DownloadSaveArgs {
    #[serde(rename = "sub")]
    _sub: String,
    file: String,
    #[serde(default, rename = "file_state")]
    _file_state: Option<serde_json::Value>,
    output_dir: String,
    #[serde(default, rename = "output_dir_state")]
    _output_dir_state: Option<serde_json::Value>,
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
    deadline: TransactionDeadline,
) -> Result<serde_json::Value, RubError> {
    let batch = DownloadSaveBatch::new(
        DownloadSaveRequest::try_from(args)?,
        deadline.remaining_ms(),
    )?;
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
    fn new(
        mut request: DownloadSaveRequest,
        authoritative_timeout_ms: u64,
    ) -> Result<Self, RubError> {
        let effective_timeout_ms = request.timeout_ms.min(authoritative_timeout_ms);
        let deadline = Instant::now() + Duration::from_millis(effective_timeout_ms.max(1));
        request.timeout_ms = effective_timeout_ms;
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
            "output_dir_state": bulk_output_dir_state_json(),
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

#[cfg(test)]
mod tests {
    use super::paths::{
        build_base_filename, content_type_extension, is_meaningful_filename_stem,
        normalize_known_extension, reconcile_output_path, reconcile_output_path_with_bytes,
        sanitize_filename, sniff_content_extension, split_known_extension, with_numeric_suffix,
    };
    use super::paths::{
        bulk_output_dir_state, cleanup_tracked_temp_path, reserve_reconciled_output_path,
        saved_asset_output_path_state, set_tracked_temp_path,
    };
    use super::request::{
        asset_request_authority, lookup_json_path, parse_save_request, parse_text_asset_sources,
        resolve_asset_url, resolve_json_asset_root,
    };
    use super::{DownloadSaveArgs, DownloadSaveBatch, DownloadSaveRequest, MAX_SAVE_CONCURRENCY};
    use reqwest::header::HeaderValue;
    use rub_core::error::ErrorCode;
    use rub_core::fs::commit_temporary_file;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use time::OffsetDateTime;
    use tokio::io::AsyncWriteExt;

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
    fn download_save_batch_uses_authoritative_timeout_budget() {
        let request = DownloadSaveRequest {
            file: PathBuf::from("/tmp/assets.json"),
            output_dir: PathBuf::from("/tmp/out"),
            input_field: None,
            url_field: None,
            name_field: None,
            base_url: None,
            cookie_url: None,
            limit: None,
            concurrency: 1,
            overwrite: false,
            timeout_ms: 30_000,
        };

        let batch = DownloadSaveBatch::new(request, 250).expect("batch should build");
        assert!(
            batch.deadline <= std::time::Instant::now() + std::time::Duration::from_millis(260),
            "batch deadline should clamp to the authoritative timeout budget"
        );
        assert_eq!(batch.request.timeout_ms, 250);
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

    #[tokio::test]
    async fn download_save_commit_accepts_readable_async_temp_handle() {
        let temp_root = std::env::temp_dir().join(format!(
            "rub-asset-save-commit-{}",
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();
        let tmp_path = temp_root.join("alpha.jpg.part");
        let final_path = temp_root.join("alpha.jpg");

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&tmp_path)
            .await
            .unwrap();
        file.write_all(b"AAA").await.unwrap();
        file.flush().await.unwrap();
        let file = file.into_std().await;

        let outcome = commit_temporary_file(&file, &tmp_path, &final_path).unwrap();
        assert!(outcome.durability_confirmed());
        assert_eq!(std::fs::read(&final_path).unwrap(), b"AAA");

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

    #[test]
    fn saved_asset_output_path_state_distinguishes_saved_skipped_and_planned_paths() {
        let saved = saved_asset_output_path_state(rub_core::model::SavedAssetStatus::Saved, None);
        assert_eq!(saved.path_kind, "saved_artifact");
        assert_eq!(saved.path_authority, "router.download_save.output_path");
        assert_eq!(saved.upstream_truth, "download_save_entry_result");
        assert_eq!(saved.control_role, "display_only");
        assert_eq!(saved.durability, "durable");

        let published =
            saved_asset_output_path_state(rub_core::model::SavedAssetStatus::Saved, Some(false));
        assert_eq!(published.path_kind, "saved_artifact");
        assert_eq!(published.durability, "published");

        let skipped =
            saved_asset_output_path_state(rub_core::model::SavedAssetStatus::SkippedExisting, None);
        assert_eq!(skipped.path_kind, "existing_file_reference");
        assert_eq!(skipped.durability, "external_existing_file_reference");

        let failed = saved_asset_output_path_state(rub_core::model::SavedAssetStatus::Failed, None);
        assert_eq!(failed.path_kind, "planned_output_reference");
        assert_eq!(failed.durability, "not_committed");

        let timed_out =
            saved_asset_output_path_state(rub_core::model::SavedAssetStatus::TimedOut, None);
        assert_eq!(timed_out.path_kind, "planned_output_reference");
        assert_eq!(timed_out.durability, "not_committed");
    }

    #[test]
    fn bulk_output_dir_state_marks_request_directory_reference() {
        let state = bulk_output_dir_state();
        assert_eq!(state.path_kind, "batch_output_directory");
        assert_eq!(state.path_authority, "router.download_save.output_dir");
        assert_eq!(state.upstream_truth, "download_save_batch_request");
        assert_eq!(state.control_role, "display_only");
    }

    #[test]
    fn download_save_args_accept_request_path_metadata() {
        let parsed: DownloadSaveArgs = serde_json::from_value(serde_json::json!({
            "sub": "save",
            "file": "/tmp/assets.json",
            "file_state": {
                "path_authority": "cli.download.save.file"
            },
            "output_dir": "/tmp/output",
            "output_dir_state": {
                "path_authority": "cli.download.save.output_dir"
            }
        }))
        .expect("download save payload should accept display-only path metadata");
        assert_eq!(parsed.file, "/tmp/assets.json");
        assert_eq!(parsed.output_dir, "/tmp/output");
    }
}
