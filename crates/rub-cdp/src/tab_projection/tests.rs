use super::hooks::{
    ALL_PAGE_HOOKS_MASK, CRITICAL_RUNTIME_HOOKS_MASK, PageHookFlag, PageHookInstallState,
    PageHookResult, required_runtime_hooks_commit_ready, user_agent_protocol_override_succeeded,
};
use super::tabs::{projected_stealth_patch_names, projected_tab_title, projected_tab_url};
use crate::browser::BrowserLaunchOptions;
use crate::identity_policy::{IdentityPolicy, UserAgentOverrideProfile};
use rub_core::model::ConnectionTarget;

#[test]
fn user_agent_override_script_escapes_single_quotes() {
    let script = IdentityPolicy::from_options(&BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: None,
        managed_profile_ephemeral: false,
        download_dir: None,
        profile_directory: None,
        hide_infobars: true,
        stealth: true,
    })
    .user_agent_override("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/146.0.0.0 'Test' Safari/537.36")
    .map(|UserAgentOverrideProfile { script, .. }| script)
    .unwrap();
    assert!(script.contains("\\'Test\\'"));
    assert!(script.contains("navigator"));
}

#[test]
fn projected_stealth_patch_names_include_clean_args_for_managed_sessions() {
    let options = BrowserLaunchOptions {
        headless: true,
        ignore_cert_errors: false,
        user_data_dir: None,
        managed_profile_ephemeral: false,
        download_dir: None,
        profile_directory: None,
        hide_infobars: true,
        stealth: true,
    };
    let config = crate::stealth::StealthConfig::default();
    let managed =
        projected_stealth_patch_names(&options, Some(&ConnectionTarget::Managed), &config);
    assert!(managed.iter().any(|patch| patch == "clean_chrome_args"));

    let external = projected_stealth_patch_names(
        &options,
        Some(&ConnectionTarget::CdpUrl {
            url: "http://127.0.0.1:9222".to_string(),
        }),
        &config,
    );
    assert!(!external.iter().any(|patch| patch == "clean_chrome_args"));
}

#[test]
fn user_agent_protocol_override_only_counts_completed_success() {
    assert!(user_agent_protocol_override_succeeded(&PageHookResult::<
        (),
        (),
    >::Completed(
        Ok(())
    )));
    assert!(!user_agent_protocol_override_succeeded(&PageHookResult::<
        (),
        (),
    >::Completed(
        Err(())
    )));
    assert!(!user_agent_protocol_override_succeeded(
        &PageHookResult::<(), ()>::TimedOut
    ));
}

#[test]
fn invalidating_runtime_callback_hooks_preserves_non_callback_installation_state() {
    let mut state = PageHookInstallState::default();
    state.mark_all(ALL_PAGE_HOOKS_MASK);
    state.installation_recorded = true;

    state.invalidate_runtime_callback_hooks();

    assert!(state.contains(PageHookFlag::EnvironmentMetrics));
    assert!(state.contains(PageHookFlag::TouchEmulation));
    assert!(state.contains(PageHookFlag::StealthNewDocument));
    assert!(state.contains(PageHookFlag::StealthLive));
    assert!(state.contains(PageHookFlag::UserAgent));
    assert!(state.contains(PageHookFlag::SelfProbe));
    assert!(state.contains(PageHookFlag::DomEnable));
    assert!(!state.contains(PageHookFlag::NetworkRules));
    assert!(state.installation_recorded);
    assert!(!state.contains(PageHookFlag::RuntimeProbe));
    assert!(!state.contains(PageHookFlag::FrameListener));
    assert!(!state.contains(PageHookFlag::DocumentListener));
    assert!(!state.contains(PageHookFlag::Observatory));
    assert!(!state.contains(PageHookFlag::Dialogs));
    assert!(!state.complete());
}

#[test]
fn tab_probe_failures_project_empty_strings_and_publish_degraded_metadata_elsewhere() {
    assert_eq!(projected_tab_url(None), "");
    assert_eq!(projected_tab_title(None), "");
    assert_eq!(
        projected_tab_url(Some("about:blank".to_string())),
        "about:blank"
    );
    assert_eq!(projected_tab_title(Some(String::new())), "");
}

#[test]
fn required_runtime_commit_ready_ignores_auxiliary_identity_hooks() {
    let mut state = PageHookInstallState::default();
    state.mark_all(CRITICAL_RUNTIME_HOOKS_MASK);

    assert!(required_runtime_hooks_commit_ready(
        &state,
        CRITICAL_RUNTIME_HOOKS_MASK,
        0,
    ));
    assert!(!state.complete());
}
