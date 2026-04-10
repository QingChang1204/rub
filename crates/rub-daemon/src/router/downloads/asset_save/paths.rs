use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use reqwest::header::HeaderValue;
use rub_core::model::{BulkAssetSaveOutputDirState, SavedAssetOutputPathState, SavedAssetStatus};
use time::OffsetDateTime;
use tokio::fs;

pub(super) fn saved_asset_output_path_state(
    status: SavedAssetStatus,
    durability_confirmed: Option<bool>,
) -> SavedAssetOutputPathState {
    let (path_kind, durability) = match status {
        SavedAssetStatus::Saved => (
            "saved_artifact",
            if matches!(durability_confirmed, Some(false)) {
                "published"
            } else {
                "durable"
            },
        ),
        SavedAssetStatus::SkippedExisting => (
            "existing_file_reference",
            "external_existing_file_reference",
        ),
        SavedAssetStatus::Failed | SavedAssetStatus::TimedOut => {
            ("planned_output_reference", "not_committed")
        }
    };

    SavedAssetOutputPathState {
        path_kind: path_kind.to_string(),
        path_authority: "router.download_save.output_path".to_string(),
        upstream_truth: "download_save_entry_result".to_string(),
        control_role: "display_only".to_string(),
        durability: durability.to_string(),
    }
}

pub(super) fn bulk_output_dir_state() -> BulkAssetSaveOutputDirState {
    BulkAssetSaveOutputDirState {
        path_kind: "batch_output_directory".to_string(),
        path_authority: "router.download_save.output_dir".to_string(),
        upstream_truth: "download_save_batch_request".to_string(),
        control_role: "display_only".to_string(),
    }
}

pub(super) fn bulk_output_dir_state_json() -> serde_json::Value {
    serde_json::to_value(bulk_output_dir_state()).unwrap_or(serde_json::Value::Null)
}

pub(super) fn planned_output_path(
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

pub(super) fn reconcile_output_path(path: &Path, content_type: Option<&HeaderValue>) -> PathBuf {
    let current_extension = path.extension().and_then(|value| value.to_str());
    if !matches!(current_extension, Some("bin")) {
        return path.to_path_buf();
    }
    let Some(inferred_extension) = content_type.and_then(content_type_extension) else {
        return path.to_path_buf();
    };
    path.with_extension(inferred_extension)
}

pub(super) fn reconcile_output_path_with_bytes(
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

pub(super) fn reserve_reconciled_output_path(
    planned_path: &Path,
    content_type: Option<&HeaderValue>,
    first_chunk: Option<&[u8]>,
    reserved_output_paths: &Mutex<BTreeSet<PathBuf>>,
) -> PathBuf {
    let reconciled = reconcile_output_path_with_bytes(planned_path, content_type, first_chunk);
    let mut reserved_paths = reserved_output_paths
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reserved_paths.remove(planned_path);
    if !reserved_paths.contains(&reconciled) {
        reserved_paths.insert(reconciled.clone());
        return reconciled;
    }
    let unique = reserve_unique_output_path(&reconciled, &reserved_paths);
    reserved_paths.insert(unique.clone());
    unique
}

pub(super) fn reserve_unique_output_path(
    path: &Path,
    reserved_paths: &BTreeSet<PathBuf>,
) -> PathBuf {
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

pub(super) fn build_base_filename(url: &str, source_name: Option<&str>) -> String {
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

pub(super) fn split_known_extension(value: String) -> (String, Option<String>) {
    let path = Path::new(&value);
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let extension = path.extension().and_then(|ext| ext.to_str());
    match (stem, extension.and_then(normalize_known_extension)) {
        (Some(stem), Some(extension)) if !stem.is_empty() => (stem.to_string(), Some(extension)),
        _ => (value, None),
    }
}

pub(super) fn normalize_known_extension(extension: &str) -> Option<String> {
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

pub(super) fn is_meaningful_filename_stem(value: &str) -> bool {
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

pub(super) fn content_type_extension(content_type: &HeaderValue) -> Option<&'static str> {
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

pub(super) fn sniff_content_extension(bytes: &[u8]) -> Option<&'static str> {
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

pub(super) fn with_numeric_suffix(file_name: &str, sequence: u32) -> String {
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

pub(super) fn sanitize_filename(raw: &str) -> String {
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

pub(super) fn temporary_path(path: &Path) -> PathBuf {
    let stamp = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    path.with_file_name(format!("{file_name}.part-{stamp}"))
}

pub(super) fn set_tracked_temp_path(slot: &Arc<Mutex<Option<PathBuf>>>, value: Option<PathBuf>) {
    if let Ok(mut guard) = slot.lock() {
        *guard = value;
    }
}

pub(super) async fn cleanup_tracked_temp_path(slot: &Arc<Mutex<Option<PathBuf>>>) {
    let path = {
        let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.take()
    };
    if let Some(path) = path {
        let _ = fs::remove_file(path).await;
    }
}

pub(super) fn set_tracked_output_path(slot: &Arc<Mutex<Option<PathBuf>>>, path: Option<PathBuf>) {
    if let Ok(mut guard) = slot.lock() {
        *guard = path;
    }
}

pub(super) fn tracked_output_path(slot: &Arc<Mutex<Option<PathBuf>>>) -> Option<PathBuf> {
    slot.lock().ok().and_then(|guard| guard.clone())
}
