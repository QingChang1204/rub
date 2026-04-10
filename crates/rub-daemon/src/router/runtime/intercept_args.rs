use rub_core::error::{ErrorCode, RubError};

use crate::router::request_args::subcommand_arg;

#[derive(Clone, Copy, Debug)]
pub(super) enum InterceptAction {
    List,
    Rewrite,
    Block,
    Allow,
    Header,
    Remove,
    Clear,
}

impl InterceptAction {
    pub(super) fn parse(args: &serde_json::Value) -> Result<Self, RubError> {
        match subcommand_arg(args, "list") {
            "list" => Ok(Self::List),
            "rewrite" => Ok(Self::Rewrite),
            "block" => Ok(Self::Block),
            "allow" => Ok(Self::Allow),
            "header" => Ok(Self::Header),
            "remove" => Ok(Self::Remove),
            "clear" => Ok(Self::Clear),
            other => Err(RubError::domain(
                ErrorCode::InvalidInput,
                format!("Unknown intercept subcommand: '{other}'"),
            )),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InterceptRewriteArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) source_pattern: String,
    pub(super) target_base: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InterceptUrlPatternArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) url_pattern: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InterceptHeaderArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) url_pattern: String,
    pub(super) headers: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InterceptRemoveArgs {
    #[serde(rename = "sub")]
    pub(super) _sub: String,
    pub(super) id: u32,
}
