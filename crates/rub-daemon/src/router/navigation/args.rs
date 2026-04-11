use rub_core::error::{ErrorCode, RubError};

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenArgs {
    pub(super) url: String,
    #[serde(default)]
    pub(super) load_strategy: Option<String>,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StateArgs {
    #[serde(default)]
    pub(super) limit: Option<u64>,
    #[serde(default)]
    pub(super) format: Option<String>,
    #[serde(default)]
    pub(super) a11y: bool,
    #[serde(default)]
    pub(super) viewport: bool,
    #[serde(default)]
    pub(super) diff: Option<String>,
    #[serde(default)]
    pub(super) listeners: bool,
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

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScrollArgs {
    #[serde(default)]
    pub(super) direction: Option<String>,
    #[serde(default)]
    pub(super) amount: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReloadArgs {
    #[serde(default)]
    pub(super) load_strategy: Option<String>,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScreenshotArgs {
    #[serde(default)]
    pub(super) full: bool,
    #[serde(default)]
    pub(super) highlight: bool,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default, rename = "path_state")]
    pub(super) _path_state: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SwitchArgs {
    pub(super) index: u32,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CloseTabArgs {
    #[serde(default)]
    pub(super) index: Option<u32>,
}

pub(super) fn parse_optional_load_strategy(
    value: Option<&str>,
    name: &str,
) -> Result<rub_core::model::LoadStrategy, RubError> {
    let Some(value) = value else {
        return Ok(rub_core::model::LoadStrategy::default());
    };
    serde_json::from_value(serde_json::Value::String(value.to_string())).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Invalid {name} '{}'; expected one of: load, domcontentloaded, networkidle",
                value
            ),
        )
    })
}

pub(super) fn parse_optional_scroll_direction(
    value: Option<&str>,
    name: &str,
) -> Result<rub_core::model::ScrollDirection, RubError> {
    let Some(value) = value else {
        return Ok(rub_core::model::ScrollDirection::Down);
    };
    serde_json::from_value(serde_json::Value::String(value.to_string())).map_err(|_| {
        RubError::domain(
            ErrorCode::InvalidInput,
            format!("Invalid {name} '{}'; expected one of: up, down", value),
        )
    })
}
