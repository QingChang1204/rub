use super::super::request_args::LocatorRequestArgs;

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "sub", rename_all = "lowercase")]
pub(super) enum GetCommand {
    Title,
    Html(GetHtmlArgs),
    Text(QueryReadArgs),
    Value(QueryReadArgs),
    Attributes(QueryReadArgs),
    Bbox(QueryReadArgs),
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecArgs {
    pub(super) code: String,
    #[serde(default, rename = "raw")]
    pub(super) _raw: bool,
    #[serde(default, rename = "wait_after")]
    pub(super) _wait_after: Option<serde_json::Value>,
    #[serde(default, rename = "_trigger")]
    pub(super) _trigger: Option<serde_json::Value>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GetHtmlArgs {
    #[serde(default)]
    pub(super) selector: Option<String>,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueryReadArgs {
    #[serde(default)]
    pub(super) snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InspectReadArgs {
    #[serde(default)]
    pub(super) many: bool,
    #[serde(default)]
    pub(super) snapshot_id: Option<String>,
    #[serde(flatten)]
    pub(super) locator: LocatorRequestArgs,
    #[serde(default, rename = "_orchestration")]
    pub(super) _orchestration: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum GetReadKind {
    Text,
    Value,
    Attributes,
    Bbox,
}

impl GetReadKind {
    pub(super) fn command_name(self) -> &'static str {
        match self {
            Self::Text => "get text",
            Self::Value => "get value",
            Self::Attributes => "get attributes",
            Self::Bbox => "get bbox",
        }
    }

    pub(super) fn response_field(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Value => "value",
            Self::Attributes => "attributes",
            Self::Bbox => "bbox",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum InspectReadKind {
    Text,
    Value,
    Attributes,
    Bbox,
}

impl InspectReadKind {
    pub(super) fn kind_name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Value => "value",
            Self::Attributes => "attributes",
            Self::Bbox => "bbox",
        }
    }
}
