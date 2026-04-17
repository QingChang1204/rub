use serde::{Deserialize, Serialize};

/// Runtime status of the session-scoped JavaScript dialog surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogRuntimeStatus {
    #[default]
    Inactive,
    Active,
    Degraded,
}

/// JavaScript dialog type surfaced by the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    Beforeunload,
}

/// One currently pending JavaScript dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDialogInfo {
    pub kind: DialogKind,
    pub message: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
    pub has_browser_handler: bool,
    pub opened_at: String,
}

/// Most recent dialog resolution observed by the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogResolutionInfo {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_input: Option<String>,
    pub closed_at: String,
}

/// Session-scoped JavaScript dialog runtime projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogRuntimeInfo {
    pub status: DialogRuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_dialog: Option<PendingDialogInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_dialog: Option<PendingDialogInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<DialogResolutionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for DialogRuntimeInfo {
    fn default() -> Self {
        Self {
            status: DialogRuntimeStatus::Inactive,
            pending_dialog: None,
            last_dialog: None,
            last_result: None,
            degraded_reason: None,
        }
    }
}

/// A pre-registered one-shot dialog handling intent.
///
/// When set, the CDP `EventJavascriptDialogOpening` listener consumes this
/// policy and immediately calls `Page.handleJavaScriptDialog` — before Chrome's
/// built-in handler can auto-dismiss the dialog.
///
/// This is the correct fix for `has_browser_handler: true` race conditions in
/// headless Chrome, where the browser may auto-dismiss dialogs before an
/// IPC-routed `dialog accept/dismiss` command can arrive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogInterceptPolicy {
    /// Whether to accept (`true`) or dismiss (`false`) the intercepted dialog.
    pub accept: bool,
    /// Optional text to provide if the dialog is a `prompt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
    /// Restrict this intercept to a specific tab (CDP target ID).
    ///
    /// When `Some`, the listener only consumes this policy if its own
    /// `tab_target_id` matches. When `None`, any tab may consume it
    /// (single-tab sessions only — not safe in multi-tab contexts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_tab_id: Option<String>,
}
