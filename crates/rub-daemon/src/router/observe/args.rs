use rub_core::error::{ErrorCode, RubError};

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObserveArgs {
    #[serde(default)]
    pub(super) full: bool,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default, rename = "path_state")]
    pub(super) _path_state: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) limit: Option<u64>,
    #[serde(default, rename = "compact")]
    pub(super) _compact: bool,
    #[serde(default, rename = "depth")]
    pub(super) _depth: Option<u64>,
    #[serde(default, rename = "scope")]
    pub(super) _scope: Option<serde_json::Value>,
    #[serde(default, rename = "scope_selector")]
    pub(super) _scope_selector: Option<String>,
    #[serde(default, rename = "scope_role")]
    pub(super) _scope_role: Option<String>,
    #[serde(default, rename = "scope_label")]
    pub(super) _scope_label: Option<String>,
    #[serde(default, rename = "scope_testid")]
    pub(super) _scope_testid: Option<String>,
    #[serde(default, rename = "scope_first")]
    pub(super) _scope_first: bool,
    #[serde(default, rename = "scope_last")]
    pub(super) _scope_last: bool,
    #[serde(default, rename = "scope_nth")]
    pub(super) _scope_nth: Option<u64>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

pub(super) fn parse_observe_limit(limit: Option<u64>) -> Result<Option<u32>, RubError> {
    let Some(raw_limit) = limit else {
        return Ok(None);
    };
    let limit = u32::try_from(raw_limit).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "observe limit {raw_limit} exceeds the supported maximum {}",
                u32::MAX
            ),
        )
    })?;
    Ok(Some(limit))
}
