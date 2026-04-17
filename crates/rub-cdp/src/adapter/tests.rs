use super::ChromiumAdapter;
use crate::browser::{BrowserLaunchOptions, BrowserManager};
use crate::humanize::{HumanizeConfig, HumanizeSpeed};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

#[test]
fn projected_launch_policy_reports_l2_when_humanize_enabled() {
    let manager = Arc::new(BrowserManager::new(BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: None,
        managed_profile_ephemeral: false,
        download_dir: None,
        profile_directory: None,
        hide_infobars: true,
        stealth: true,
    }));
    let adapter = ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        HumanizeConfig {
            enabled: true,
            speed: HumanizeSpeed::Slow,
        },
    );

    let launch_policy = adapter.projected_launch_policy();
    assert_eq!(launch_policy.stealth_level.as_deref(), Some("L2"));
    assert_eq!(launch_policy.humanize_enabled, Some(true));
    assert_eq!(launch_policy.humanize_speed.as_deref(), Some("slow"));
}
