//! DaemonRouter — FIFO command queue + dispatch (AUTH.DaemonRouter).
//! Owns command transaction lifecycle, epoch management, and replay cache.

mod addressing;
mod artifacts;
pub(crate) mod automation_fence;
mod diagnostics;
mod dialogs;
mod dispatch;
mod downloads;
mod element_semantics;
mod extract;
mod extract_postprocess;
mod find;
mod frame_scope;
mod frames;
mod history;
mod inspect;
mod interaction;
mod interference;
mod navigation;
mod network_inspection;
mod observation_filter;
mod observation_scope;
mod observe;
mod orchestration;
mod policy;
mod projection;
mod query;
mod queue;
pub(crate) mod request_args;
mod runtime;
pub(crate) mod secret_resolution;
mod snapshot;
mod state_format;
mod storage;
mod timeout;
mod transaction;
mod transaction_context;
mod triggers;
mod url_normalization;
mod wait_after;
mod workflow;

use std::path::Path;
use std::sync::Arc;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{NetworkRule, NetworkRuleSpec, NetworkRuleStatus};
use rub_core::port::BrowserPort;
use rub_ipc::protocol::IPC_PROTOCOL_VERSION;

use crate::session::SessionState;

use diagnostics::agent_capabilities;
use diagnostics::detection_risks;
use projection::{attach_interaction_projection, attach_select_projection};
pub(super) use transaction::attach_response_metadata;
pub(crate) use transaction_context::{
    CommandDispatchOutcome, OwnedRouterTransactionGuard, PendingExternalDomCommit,
    RouterTransactionGuard, TransactionDeadline,
};
use wait_after::apply_post_wait_if_requested;
/// The central command router. Owns the FIFO dispatch queue.
pub struct DaemonRouter {
    browser: Arc<dyn BrowserPort>,
    /// Serializes command execution (FIFO).
    exec_semaphore: Arc<tokio::sync::Semaphore>,
}

impl DaemonRouter {
    pub fn new(browser: Arc<dyn BrowserPort>) -> Self {
        Self {
            browser,
            exec_semaphore: Arc::new(tokio::sync::Semaphore::new(1)), // FIFO: one at a time
        }
    }

    pub(crate) fn browser_port(&self) -> Arc<dyn BrowserPort> {
        self.browser.clone()
    }

    /// Shutdown the browser (called on daemon exit).
    pub async fn shutdown(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.browser.close().await.map_err(|e| Box::new(e) as _)
    }
}

pub(crate) fn explain_extract_spec_contract(
    raw: &str,
    rub_home: &Path,
) -> Result<serde_json::Value, RubError> {
    extract::explain_extract_spec_contract(raw, rub_home)
}

#[cfg(test)]
mod tests;
