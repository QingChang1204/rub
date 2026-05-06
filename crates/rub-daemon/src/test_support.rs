use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::router::DaemonRouter;

pub(crate) fn daemon_router() -> DaemonRouter {
    let manager = Arc::new(rub_cdp::browser::BrowserManager::new(
        rub_cdp::browser::BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        },
    ));
    let adapter = Arc::new(rub_cdp::adapter::ChromiumAdapter::new(
        manager,
        Arc::new(AtomicU64::new(0)),
        rub_cdp::humanize::HumanizeConfig {
            enabled: false,
            speed: rub_cdp::humanize::HumanizeSpeed::Normal,
        },
    ));
    DaemonRouter::new(adapter)
}

pub(crate) fn daemon_router_arc() -> Arc<DaemonRouter> {
    Arc::new(daemon_router())
}
