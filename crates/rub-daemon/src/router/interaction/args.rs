use super::super::request_args::LocatorRequestArgs;
use rub_core::error::{ErrorCode, RubError};

#[derive(Debug, Clone, Copy)]
pub(super) enum ClickGesture {
    Single,
    Double,
    Right,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClickArgs {
    #[serde(default)]
    pub(super) gesture: Option<String>,
    #[serde(default)]
    pub(super) xy: Option<[f64; 2]>,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    pub(super) _snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) _locator: LocatorRequestArgs,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KeysArgs {
    pub(super) keys: String,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TextEntryArgs {
    pub(super) text: String,
    #[serde(default)]
    pub(super) clear: bool,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    pub(super) _snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) locator: LocatorRequestArgs,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HoverArgs {
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    pub(super) _snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadArgs {
    pub(super) path: String,
    #[serde(default, rename = "path_state")]
    pub(super) _path_state: Option<serde_json::Value>,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    pub(super) _snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectArgs {
    pub(super) value: String,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "snapshot_id")]
    pub(super) _snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) _locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

pub(super) fn requested_click_gesture(gesture: Option<&str>) -> Result<ClickGesture, RubError> {
    let gesture = gesture.unwrap_or("single");
    match gesture {
        "single" => Ok(ClickGesture::Single),
        "double" => Ok(ClickGesture::Double),
        "right" => Ok(ClickGesture::Right),
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!("Unsupported click gesture: {other}"),
        )),
    }
}

pub(super) fn click_command_name(gesture: ClickGesture) -> &'static str {
    match gesture {
        ClickGesture::Single => "click",
        ClickGesture::Double | ClickGesture::Right => "click",
    }
}

pub(super) fn click_gesture_name(gesture: ClickGesture) -> &'static str {
    match gesture {
        ClickGesture::Single => "single",
        ClickGesture::Double => "double",
        ClickGesture::Right => "right",
    }
}
