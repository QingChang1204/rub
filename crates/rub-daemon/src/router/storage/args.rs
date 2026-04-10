use rub_core::error::{ErrorCode, RubError};
use rub_core::storage::StorageArea;

use crate::router::request_args::parse_json_args;

#[derive(Debug)]
pub(super) enum StorageCommand {
    Get(StorageGetArgs),
    Set(StorageSetArgs),
    Remove(StorageRemoveArgs),
    Clear(StorageClearArgs),
    Export(StorageExportArgs),
    Import(StorageImportArgs),
}

impl StorageCommand {
    pub(super) fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match args
            .get("sub")
            .and_then(|value| value.as_str())
            .unwrap_or("export")
        {
            "get" => Ok(Self::Get(parse_json_args(args, "storage get")?)),
            "set" => Ok(Self::Set(parse_json_args(args, "storage set")?)),
            "remove" => Ok(Self::Remove(parse_json_args(args, "storage remove")?)),
            "clear" => Ok(Self::Clear(parse_json_args(args, "storage clear")?)),
            "export" => Ok(Self::Export(parse_json_args(args, "storage export")?)),
            "import" => Ok(Self::Import(parse_json_args(args, "storage import")?)),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown storage subcommand '{other}'"),
            )),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InspectStorageArgs {
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default)]
    pub(super) key: Option<String>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageGetArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) key: String,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageSetArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) key: String,
    pub(super) value: String,
    pub(super) area: String,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageRemoveArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) key: String,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageClearArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    #[serde(default)]
    pub(super) area: Option<String>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageExportArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default, rename = "path_state")]
    pub(super) _path_state: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageImportArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) path: String,
    #[serde(default, rename = "path_state")]
    pub(super) _path_state: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

pub(super) fn parse_storage_area(value: Option<&str>) -> Result<Option<StorageArea>, RubError> {
    match value {
        None => Ok(None),
        Some("local") => Ok(Some(StorageArea::Local)),
        Some("session") => Ok(Some(StorageArea::Session)),
        Some(other) => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unknown storage area '{other}'. Valid: local, session"),
        )),
    }
}

pub(super) fn required_storage_area(value: Option<&str>) -> Result<StorageArea, RubError> {
    parse_storage_area(value)?.ok_or_else(|| {
        RubError::domain(
            ErrorCode::InvalidInput,
            "Storage mutation requires --area <local|session>",
        )
    })
}
