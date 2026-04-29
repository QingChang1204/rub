use std::sync::Arc;

use rub_core::error::RubError;

use crate::router::request_args::parse_json_args;
use crate::router::{CommandDispatchOutcome, DaemonRouter, TransactionDeadline};
use crate::session::SessionState;

use super::args::{CloseTabArgs, SwitchArgs};
use super::projection::{attach_result, attach_subject, tab_entity, tab_subject};
use super::settle::{active_tab_projection_from_tabs, settle_navigation_projection};

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
    let result = close_tab_result_projection(&tabs);
    let mut data = serde_json::json!({});
    attach_subject(&mut data, tab_subject(closed_index));
    attach_result(&mut data, result);
    Ok(CommandDispatchOutcome::new(data)
        .with_pending_external_dom_commit(pending_external_dom_commit))
}

fn close_tab_result_projection(tabs: &[rub_core::model::TabInfo]) -> serde_json::Value {
    let active_tab = active_tab_projection_from_tabs(tabs);
    serde_json::json!({
        "remaining_tabs": tabs.len(),
        "active_tab": active_tab.tab,
        "active_tab_degraded_reason": active_tab.degraded_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::close_tab_result_projection;
    use rub_core::model::TabInfo;

    fn tab(index: u32, active: bool) -> TabInfo {
        TabInfo {
            index,
            target_id: format!("tab-{index}"),
            url: format!("https://example.com/{index}"),
            title: format!("Tab {index}"),
            active,
            active_authority: None,
            degraded_reason: None,
        }
    }

    #[test]
    fn close_tab_result_projects_degraded_active_tab_without_erasing_commit() {
        let projection = close_tab_result_projection(&[tab(0, false)]);

        assert_eq!(projection["remaining_tabs"], 1);
        assert!(projection["active_tab"].is_null());
        assert_eq!(
            projection["active_tab_degraded_reason"],
            "active_tab_unavailable"
        );
    }

    #[test]
    fn close_tab_result_preserves_active_tab_when_only_title_degraded() {
        let mut active = tab(0, true);
        active.url = "about:blank".to_string();
        active.title = String::new();
        active.degraded_reason = Some("tab_title_probe_failed".to_string());

        let projection = close_tab_result_projection(&[active]);

        assert_eq!(projection["remaining_tabs"], 1);
        assert_eq!(projection["active_tab"]["url"], "about:blank");
        assert_eq!(
            projection["active_tab"]["degraded_reason"],
            "tab_title_probe_failed"
        );
        assert!(projection["active_tab_degraded_reason"].is_null());
    }
}
