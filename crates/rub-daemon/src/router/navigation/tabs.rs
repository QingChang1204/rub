use std::sync::Arc;

use rub_core::error::RubError;

use crate::router::request_args::parse_json_args;
use crate::router::{CommandDispatchOutcome, DaemonRouter, TransactionDeadline};
use crate::session::SessionState;

use super::args::{CloseTabArgs, SwitchArgs};
use super::projection::{attach_result, attach_subject, tab_entity, tab_subject};
use super::settle::settle_navigation_projection;

pub(crate) async fn cmd_tabs(router: &DaemonRouter) -> Result<serde_json::Value, RubError> {
    let tabs = router.browser.list_tabs().await?;
    let active_tab = tabs.iter().find(|tab| tab.active).map(tab_entity);
    Ok(serde_json::json!({
        "subject": {
            "kind": "tab_registry",
        },
        "result": {
            "items": tabs,
            "active_tab": active_tab,
        },
    }))
}

pub(crate) async fn cmd_switch(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let parsed: SwitchArgs = parse_json_args(args, "switch")?;
    let tab = router.browser.switch_tab(parsed.index).await?;
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "switch", deadline).await;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, tab_subject(parsed.index));
    attach_result(
        &mut data,
        serde_json::json!({
            "active_tab": tab_entity(&tab),
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

pub(crate) async fn cmd_close_tab(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<CommandDispatchOutcome, RubError> {
    let parsed: CloseTabArgs = parse_json_args(args, "close-tab")?;
    let index = parsed.index;
    let before_tabs = router.browser.list_tabs().await?;
    let closed_index = index.unwrap_or_else(|| {
        before_tabs
            .iter()
            .find(|tab| tab.active)
            .map(|tab| tab.index)
            .unwrap_or(0)
    });
    let tabs = router.browser.close_tab(index).await?;
    let pending_external_dom_commit =
        settle_navigation_projection(router, state, "close-tab", deadline).await;
    let active_tab = tabs.iter().find(|tab| tab.active).ok_or_else(|| {
        RubError::Internal("close-tab completed without an active tab".to_string())
    })?;
    let mut data = serde_json::json!({});
    attach_subject(&mut data, tab_subject(closed_index));
    attach_result(
        &mut data,
        serde_json::json!({
            "remaining_tabs": tabs.len(),
            "active_tab": tab_entity(active_tab),
        }),
    );
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}
