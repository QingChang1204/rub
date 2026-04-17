//! ChromiumAdapter — bridges the BrowserPort boundary to chromiumoxide.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use rub_core::model::LaunchPolicyInfo;

use crate::browser::BrowserManager;

mod browser_port;

#[cfg(test)]
mod tests;

/// Adapter connecting BrowserPort to chromiumoxide.
pub struct ChromiumAdapter {
    manager: Arc<BrowserManager>,
    dom_epoch: Arc<AtomicU64>,
    humanize: crate::humanize::HumanizeConfig,
}

impl ChromiumAdapter {
    pub fn new(
        manager: Arc<BrowserManager>,
        dom_epoch: Arc<AtomicU64>,
        humanize: crate::humanize::HumanizeConfig,
    ) -> Self {
        Self {
            manager,
            dom_epoch,
            humanize,
        }
    }

    fn projected_launch_policy(&self) -> LaunchPolicyInfo {
        let mut launch_policy = self.manager.launch_policy_info();
        launch_policy.humanize_enabled = Some(self.humanize.enabled);
        launch_policy.humanize_speed = Some(
            match self.humanize.speed {
                crate::humanize::HumanizeSpeed::Fast => "fast",
                crate::humanize::HumanizeSpeed::Normal => "normal",
                crate::humanize::HumanizeSpeed::Slow => "slow",
            }
            .to_string(),
        );
        if self.humanize.enabled {
            launch_policy.stealth_level = Some("L2".to_string());
        }
        launch_policy
    }
}
