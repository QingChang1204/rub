//! Tab projection, page hooks, and launch-policy projection helpers.

mod hooks;
mod tabs;
#[cfg(test)]
mod tests;

pub(crate) use self::hooks::{
    EpochCallback, PageHookInstallState, ProjectionContext, ensure_page_hooks,
    replay_runtime_state_for_committed_active_page, sync_tabs_projection_with,
};
pub(crate) use self::tabs::{
    CommittedTabProjection, LocalActiveTargetAuthority, projected_stealth_patch_names,
    resolve_active_page_index_from_browser_truth, resolve_active_target_authority,
    resolve_active_target_from_browser_truth, tab_info_for_page, tab_not_found,
    wait_for_startup_page,
};
