use std::path::PathBuf;

use crate::commands::{Commands, EffectiveCli, RequestedLaunchPolicy};

pub(crate) fn effective_cli(command: Commands, rub_home: PathBuf) -> EffectiveCli {
    EffectiveCli {
        session: "default".to_string(),
        session_id: None,
        rub_home,
        timeout: 30_000,
        headed: false,
        ignore_cert_errors: false,
        user_data_dir: None,
        hide_infobars: true,
        json_pretty: false,
        verbose: false,
        trace: false,
        command,
        cdp_url: None,
        connect: false,
        profile: None,
        profile_resolved_path: None,
        use_alias: None,
        no_stealth: false,
        humanize: false,
        humanize_speed: "normal".to_string(),
        requested_launch_policy: RequestedLaunchPolicy::default(),
        effective_launch_policy: RequestedLaunchPolicy::default(),
    }
}

pub(crate) fn effective_cli_with_default_home(command: Commands) -> EffectiveCli {
    effective_cli(command, PathBuf::from("/tmp/rub-test"))
}
