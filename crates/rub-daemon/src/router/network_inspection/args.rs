use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{NetworkRequestLifecycle, NetworkRequestRecord};

use crate::router::request_args::parse_json_args;

#[derive(Clone, Copy)]
pub(super) struct NetworkWaitErrorContext<'a> {
    pub(super) request_id: Option<&'a str>,
    pub(super) url_match: Option<&'a str>,
    pub(super) method: Option<&'a str>,
    pub(super) status: Option<u16>,
    pub(super) desired_state: NetworkRequestWaitState,
    pub(super) started: std::time::Instant,
}

#[derive(Debug)]
pub(super) enum InspectNetworkCommand {
    Timeline(NetworkTimelineArgs),
    Curl(NetworkCurlArgs),
}

impl InspectNetworkCommand {
    pub(super) fn parse(args: &serde_json::Value, sub: &str) -> Result<Self, RubError> {
        let mut normalized = args.clone();
        if let Some(object) = normalized.as_object_mut() {
            // Use the sub provided explicitly by cmd_inspect dispatch (already matched
            // from the routing key before it was stripped from forwarded args).
            object.insert("sub".to_string(), serde_json::json!(sub));
        }
        #[derive(Debug, serde::Deserialize)]
        #[serde(tag = "sub", rename_all = "lowercase")]
        enum TaggedInspectNetworkCommand {
            Network(NetworkTimelineArgs),
            Curl(NetworkCurlArgs),
        }

        match parse_json_args::<TaggedInspectNetworkCommand>(&normalized, "inspect network")? {
            TaggedInspectNetworkCommand::Network(args) => Ok(Self::Timeline(args)),
            TaggedInspectNetworkCommand::Curl(args) => Ok(Self::Curl(args)),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NetworkTimelineArgs {
    #[serde(default)]
    pub(super) wait: bool,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) last: Option<u64>,
    #[serde(default)]
    pub(super) url_match: Option<String>,
    #[serde(default)]
    pub(super) method: Option<String>,
    #[serde(default)]
    pub(super) status: Option<u64>,
    #[serde(default)]
    pub(super) lifecycle: Option<String>,
    #[serde(default)]
    pub(super) timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NetworkCurlArgs {
    pub(super) id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NetworkRequestWaitState {
    Pending,
    Responded,
    Completed,
    Failed,
    Terminal,
}

impl NetworkRequestWaitState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Responded => "responded",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Terminal => "terminal",
        }
    }

    pub(super) fn actual_filter(self) -> Option<NetworkRequestLifecycle> {
        match self {
            Self::Pending => Some(NetworkRequestLifecycle::Pending),
            Self::Responded => Some(NetworkRequestLifecycle::Responded),
            Self::Completed => Some(NetworkRequestLifecycle::Completed),
            Self::Failed => Some(NetworkRequestLifecycle::Failed),
            Self::Terminal => None,
        }
    }

    pub(super) fn matches(self, lifecycle: NetworkRequestLifecycle) -> bool {
        match self {
            Self::Pending => lifecycle == NetworkRequestLifecycle::Pending,
            Self::Responded => lifecycle == NetworkRequestLifecycle::Responded,
            Self::Completed => lifecycle == NetworkRequestLifecycle::Completed,
            Self::Failed => lifecycle == NetworkRequestLifecycle::Failed,
            Self::Terminal => matches!(
                lifecycle,
                NetworkRequestLifecycle::Completed | NetworkRequestLifecycle::Failed
            ),
        }
    }
}

pub(super) fn parse_lifecycle_filter(
    value: Option<&str>,
) -> Result<Option<NetworkRequestWaitState>, RubError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        "pending" => Ok(Some(NetworkRequestWaitState::Pending)),
        "responded" => Ok(Some(NetworkRequestWaitState::Responded)),
        "completed" => Ok(Some(NetworkRequestWaitState::Completed)),
        "failed" => Ok(Some(NetworkRequestWaitState::Failed)),
        "terminal" => Ok(Some(NetworkRequestWaitState::Terminal)),
        other => Err(RubError::domain(
            ErrorCode::InvalidInput,
            format!(
                "Unknown network lifecycle '{other}'. Valid: pending, responded, completed, failed, terminal"
            ),
        )),
    }
}

pub(super) fn filter_requests_by_wait_state(
    requests: Vec<NetworkRequestRecord>,
    state: Option<NetworkRequestWaitState>,
) -> Vec<NetworkRequestRecord> {
    let Some(state) = state else {
        return requests;
    };
    requests
        .into_iter()
        .filter(|record| state.matches(record.lifecycle))
        .collect()
}
