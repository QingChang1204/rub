//! Browser runtime bootstrapping for managed launches and external attachment.

use chromiumoxide::Page;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::target::TargetInfo;
use rub_core::error::{ErrorCode, RubError};
#[cfg(test)]
use std::collections::BTreeSet;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{Duration, Instant, sleep};

use crate::browser::BrowserLaunchOptions;
use crate::identity_policy::IdentityPolicy;
use crate::managed_browser::{
    is_profile_in_use, prepare_managed_profile_ownership_prelaunch, resolve_managed_profile_dir,
    rollback_managed_profile_ownership_prelaunch, shutdown_managed_browser,
};

#[cfg(test)]
static FORCE_STARTUP_PAGE_AUTHORITY_FAILURE_ONCE: std::sync::OnceLock<
    std::sync::Mutex<BTreeSet<PathBuf>>,
> = std::sync::OnceLock::new();

pub(crate) async fn launch_managed_browser(
    options: &BrowserLaunchOptions,
    identity_policy: &IdentityPolicy,
) -> Result<(Arc<Browser>, Arc<Page>), RubError> {
    let profile = resolve_managed_profile_dir(
        options.user_data_dir.clone(),
        options.profile_directory.clone(),
        options.managed_profile_ephemeral,
    );
    let config = build_managed_config(options, identity_policy)?;
    let (mut browser, handler) = match Browser::launch(config).await {
        Ok(launched) => launched,
        Err(error) => {
            let launch_error = classify_managed_launch_error(options, &error.to_string());
            return Err(rollback_failed_managed_prelaunch(&profile, launch_error));
        }
    };
    spawn_handler_loop(handler);

    let page = match resolve_startup_page_authority(&mut browser, &profile.path).await {
        Ok(page) => page,
        Err(error) => {
            return Err(rollback_failed_managed_launch(browser, &profile, error).await);
        }
    };
    let browser = Arc::new(browser);

    Ok((browser, Arc::new(page)))
}

fn rollback_failed_managed_prelaunch(
    profile: &crate::managed_browser::ManagedProfileDir,
    launch_error: RubError,
) -> RubError {
    match rollback_managed_profile_ownership_prelaunch(profile) {
        Ok(()) => launch_error,
        Err(cleanup_error) => RubError::domain_with_context(
            ErrorCode::BrowserLaunchFailed,
            format!(
                "Managed browser launch failed before browser authority existed: {launch_error}"
            ),
            serde_json::json!({
                "user_data_dir": profile.path.display().to_string(),
                "managed_profile_ephemeral": profile.ephemeral,
                "prelaunch_cleanup_succeeded": false,
                "prelaunch_cleanup_error": cleanup_error.to_string(),
            }),
        ),
    }
}

pub(crate) async fn attach_external_browser(
    url: &str,
) -> Result<(Arc<Browser>, Arc<Page>, String), RubError> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let (mut browser, handler, connect_url) =
        crate::attachment::connect_external_browser_until(url, deadline).await?;
    spawn_handler_loop(handler);

    let targets = wait_for_external_targets_ready(&mut browser, url, deadline).await?;

    let pages_result = wait_for_external_pages_ready(&browser, url, deadline).await?;
    let page = select_external_active_page(pages_result, &targets, url).await?;
    let browser = Arc::new(browser);

    Ok((browser, Arc::new(page), connect_url))
}

fn remaining_attach_budget(deadline: Instant) -> Result<Duration, RubError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            "Timed out while attaching to external browser",
        ));
    }
    Ok(remaining)
}

async fn wait_for_external_targets_ready(
    browser: &mut Browser,
    url: &str,
    deadline: Instant,
) -> Result<Vec<TargetInfo>, RubError> {
    let mut last_error = format!("Failed to discover existing targets at {url}");
    loop {
        match tokio::time::timeout(remaining_attach_budget(deadline)?, browser.fetch_targets())
            .await
        {
            Ok(Ok(targets)) => {
                sleep(Duration::from_millis(100)).await;
                return Ok(targets);
            }
            Ok(Err(error)) => {
                last_error = format!("Failed to fetch external browser targets at {url}: {error}");
            }
            Err(_) => {
                last_error = format!("Timed out discovering existing targets at {url}");
            }
        }

        if Instant::now() >= deadline {
            return Err(RubError::domain(ErrorCode::CdpConnectionFailed, last_error));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_external_pages_ready(
    browser: &Browser,
    url: &str,
    deadline: Instant,
) -> Result<Vec<Page>, RubError> {
    let mut last_error = format!("Failed to enumerate pages from external browser at {url}");
    loop {
        match tokio::time::timeout(remaining_attach_budget(deadline)?, browser.pages()).await {
            Ok(Ok(pages)) if !pages.is_empty() => return Ok(pages),
            Ok(Ok(_)) => {
                last_error = format!(
                    "External browser at {url} did not expose any existing pages to attach"
                );
            }
            Ok(Err(error)) => {
                last_error =
                    format!("Failed to fetch pages from external browser at {url}: {error}");
            }
            Err(_) => {
                last_error = "Timed out enumerating pages from external browser".to_string();
            }
        }

        if Instant::now() >= deadline {
            return Err(RubError::domain(ErrorCode::CdpConnectionFailed, last_error));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn select_external_active_page(
    pages: Vec<Page>,
    _targets: &[TargetInfo],
    url: &str,
) -> Result<Page, RubError> {
    let page_target_count = pages.len();
    if page_target_count == 0 {
        return Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            format!("External browser at {url} did not expose any existing pages to attach"),
        ));
    }
    if pages.is_empty() {
        return Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            format!("External browser at {url} did not expose any existing pages to attach"),
        ));
    }
    if pages.len() == 1 {
        return pages.into_iter().next().ok_or_else(|| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!("External browser at {url} did not expose any existing pages to attach"),
            )
        });
    }
    if let Some(active_index) =
        crate::tab_projection::resolve_active_page_index_from_browser_truth(&pages).await
    {
        return pages.into_iter().nth(active_index).ok_or_else(|| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!("External browser at {url} did not expose any existing pages to attach"),
            )
        });
    }
    Err(RubError::domain(
        ErrorCode::CdpConnectionFailed,
        format!(
            "External browser at {url} did not expose a unique active-tab authority across existing pages"
        ),
    ))
}

fn classify_managed_launch_error(options: &BrowserLaunchOptions, error: &str) -> RubError {
    let profile = resolve_managed_profile_dir(
        options.user_data_dir.clone(),
        options.profile_directory.clone(),
        options.managed_profile_ephemeral,
    );
    let context = serde_json::json!({
        "user_data_dir": profile.path.display().to_string(),
    });
    if browser_launch_error_is_profile_in_use(error)
        || is_profile_in_use(&profile.path).unwrap_or(false)
    {
        return RubError::domain_with_context(
            ErrorCode::ProfileInUse,
            format!(
                "Browser profile {} is already in use by another browser process",
                profile.path.display()
            ),
            context,
        );
    }

    RubError::domain_with_context(
        ErrorCode::BrowserLaunchFailed,
        format!("Failed to launch browser: {error}"),
        context,
    )
}

fn browser_launch_error_is_profile_in_use(error: &str) -> bool {
    error.contains("Failed to create a ProcessSingleton")
        || error.contains("SingletonLock")
        || error.contains("profile appears to be in use")
}

fn build_managed_config(
    options: &BrowserLaunchOptions,
    identity_policy: &IdentityPolicy,
) -> Result<BrowserConfig, RubError> {
    build_managed_config_with_executable(options, identity_policy, None)
}

fn build_managed_config_with_executable(
    options: &BrowserLaunchOptions,
    identity_policy: &IdentityPolicy,
    executable_override: Option<&std::path::Path>,
) -> Result<BrowserConfig, RubError> {
    let mut config_builder = if options.headless {
        BrowserConfig::builder().new_headless_mode()
    } else {
        BrowserConfig::builder().with_head()
    };
    if let Some(executable) = executable_override {
        config_builder = config_builder.chrome_executable(executable);
    }
    if options.ignore_cert_errors {
        config_builder = config_builder.arg("--ignore-certificate-errors");
    }

    let profile = resolve_managed_profile_dir(
        options.user_data_dir.clone(),
        options.profile_directory.clone(),
        options.managed_profile_ephemeral,
    );
    config_builder = config_builder
        .user_data_dir(profile.path.clone())
        .arg("--rub-managed-browser=1")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-component-update")
        .arg("--disable-background-networking");
    if let Some(profile_directory) = &options.profile_directory {
        config_builder = config_builder.arg(format!("--profile-directory={profile_directory}"));
    }

    if options.stealth {
        config_builder = config_builder.arg("--disable-blink-features=AutomationControlled");
        if let Some(environment_profile) = identity_policy.environment_profile() {
            config_builder = config_builder
                .arg(environment_profile.launch_window_arg())
                .arg(environment_profile.launch_scale_arg());
        }
    } else {
        config_builder = config_builder.arg("--disable-extensions");
    }
    if options.hide_infobars && !options.headless {
        config_builder = config_builder.arg("--disable-infobars");
    }

    let config = config_builder.build().map_err(|e| {
        RubError::domain(
            ErrorCode::BrowserLaunchFailed,
            format!("Invalid browser config: {e}"),
        )
    })?;
    prepare_managed_profile_ownership_prelaunch(&profile)?;
    Ok(config)
}

async fn resolve_startup_page_authority(
    browser: &mut Browser,
    _profile_path: &Path,
) -> Result<Page, RubError> {
    #[cfg(test)]
    maybe_fail_startup_page_authority_for_test(_profile_path)?;
    crate::tab_projection::wait_for_startup_page(browser).await
}

async fn rollback_failed_managed_launch(
    browser: Browser,
    profile: &crate::managed_browser::ManagedProfileDir,
    launch_error: RubError,
) -> RubError {
    let cleanup_error = shutdown_managed_browser(&browser, profile).await.err();
    drop(browser);
    match cleanup_error {
        Some(cleanup_error) => RubError::domain_with_context(
            ErrorCode::BrowserLaunchFailed,
            format!(
                "Managed browser launch failed before startup authority committed: {launch_error}"
            ),
            serde_json::json!({
                "user_data_dir": profile.path.display().to_string(),
                "managed_profile_ephemeral": profile.ephemeral,
                "launch_cleanup_succeeded": false,
                "launch_cleanup_error": cleanup_error.to_string(),
            }),
        ),
        None => launch_error,
    }
}

#[cfg(test)]
fn maybe_fail_startup_page_authority_for_test(profile_path: &Path) -> Result<(), RubError> {
    if consume_startup_page_authority_failure_once(profile_path) {
        return Err(RubError::domain(
            ErrorCode::BrowserLaunchFailed,
            "forced startup page authority failure",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn consume_startup_page_authority_failure_once(profile_path: &Path) -> bool {
    FORCE_STARTUP_PAGE_AUTHORITY_FAILURE_ONCE
        .get_or_init(|| std::sync::Mutex::new(BTreeSet::new()))
        .lock()
        .expect("startup-page failure registry")
        .remove(profile_path)
}

#[cfg(test)]
fn force_startup_page_authority_failure_once_for(profile_path: &Path) {
    FORCE_STARTUP_PAGE_AUTHORITY_FAILURE_ONCE
        .get_or_init(|| std::sync::Mutex::new(BTreeSet::new()))
        .lock()
        .expect("startup-page failure registry")
        .insert(profile_path.to_path_buf());
}

fn spawn_handler_loop(mut handler: chromiumoxide::handler::Handler) {
    use futures::StreamExt;

    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            let _ = event;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        browser_launch_error_is_profile_in_use, build_managed_config_with_executable,
        force_startup_page_authority_failure_once_for, launch_managed_browser,
        select_external_active_page,
    };
    use crate::browser::BrowserLaunchOptions;
    use crate::identity_policy::IdentityPolicy;
    use crate::managed_browser::is_profile_in_use;
    use chromiumoxide::cdp::browser_protocol::target::TargetInfo;
    use rub_core::error::ErrorCode;
    use rub_core::managed_profile::has_temp_owned_managed_profile_marker;

    fn page_target(target_id: &str) -> TargetInfo {
        TargetInfo {
            target_id: target_id.to_string().into(),
            r#type: "page".to_string(),
            title: String::new(),
            url: "https://example.test".to_string(),
            attached: false,
            opener_id: None,
            can_access_opener: false,
            opener_frame_id: None,
            parent_frame_id: None,
            browser_context_id: None,
            subtype: None,
        }
    }

    fn options(headless: bool) -> BrowserLaunchOptions {
        BrowserLaunchOptions {
            headless,
            ignore_cert_errors: false,
            user_data_dir: None,
            managed_profile_ephemeral: false,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        }
    }

    #[test]
    fn headed_launch_config_explicitly_disables_headless_mode() {
        let options = options(false);
        let identity_policy = IdentityPolicy::from_options(&options);
        let executable = std::env::current_exe().expect("current test binary should exist");
        let config = build_managed_config_with_executable(
            &options,
            &identity_policy,
            Some(executable.as_path()),
        )
        .expect("config should build");
        let debug = format!("{config:?}");

        assert!(debug.contains("headless: False"), "{debug}");
    }

    #[test]
    fn headless_launch_config_uses_new_headless_mode() {
        let options = options(true);
        let identity_policy = IdentityPolicy::from_options(&options);
        let executable = std::env::current_exe().expect("current test binary should exist");
        let config = build_managed_config_with_executable(
            &options,
            &identity_policy,
            Some(executable.as_path()),
        )
        .expect("config should build");
        let debug = format!("{config:?}");

        assert!(debug.contains("headless: New"), "{debug}");
    }

    #[test]
    fn managed_config_build_failure_does_not_publish_ephemeral_cleanup_marker() {
        let profile_path = std::env::temp_dir().join(format!(
            "rub-chrome-config-failure-{}",
            uuid::Uuid::now_v7()
        ));
        let options = BrowserLaunchOptions {
            user_data_dir: Some(profile_path.clone()),
            managed_profile_ephemeral: true,
            ..options(true)
        };
        let identity_policy = IdentityPolicy::from_options(&options);
        let missing_executable = profile_path.join("missing-chrome");

        let result = build_managed_config_with_executable(
            &options,
            &identity_policy,
            Some(missing_executable.as_path()),
        );

        if result.is_err() {
            assert!(
                !has_temp_owned_managed_profile_marker(&profile_path),
                "config-build failure happens before prelaunch ownership is published"
            );
        }
        let _ = std::fs::remove_dir_all(profile_path);
    }

    #[test]
    fn process_singleton_launch_error_is_classified_as_profile_in_use() {
        assert!(browser_launch_error_is_profile_in_use(
            "Failed to create a ProcessSingleton for your profile directory",
        ));
        assert!(browser_launch_error_is_profile_in_use(
            "Failed to create /tmp/profile/SingletonLock: File exists",
        ));
        assert!(!browser_launch_error_is_profile_in_use(
            "Could not auto detect a chrome executable",
        ));
    }

    #[tokio::test]
    async fn external_attach_fails_closed_when_browser_targets_exist_but_no_pages_are_attachable() {
        let error = select_external_active_page(
            Vec::new(),
            &[page_target("page-1"), page_target("page-2")],
            "http://127.0.0.1:9222",
        )
        .await
        .expect_err("mismatched target inventory must fail closed");

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::CdpConnectionFailed);
        assert!(
            envelope
                .message
                .contains("did not expose any existing pages"),
            "{envelope:?}"
        );
    }

    #[tokio::test]
    async fn managed_launch_failure_before_startup_commit_cleans_ephemeral_profile_authority() {
        let profile_dir = std::env::temp_dir().join(format!(
            "rub-managed-launch-cleanup-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&profile_dir);
        let options = BrowserLaunchOptions {
            headless: true,
            ignore_cert_errors: false,
            user_data_dir: Some(profile_dir.clone()),
            managed_profile_ephemeral: true,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        };
        let identity_policy = IdentityPolicy::from_options(&options);
        force_startup_page_authority_failure_once_for(&profile_dir);

        let envelope = launch_managed_browser(&options, &identity_policy)
            .await
            .expect_err("startup-page failure should fail managed launch")
            .into_envelope();

        assert_eq!(envelope.code, ErrorCode::BrowserLaunchFailed);
        assert!(
            !profile_dir.exists(),
            "pre-commit managed launch failure must not leave ephemeral profile residue"
        );
        assert!(
            !is_profile_in_use(&profile_dir).unwrap_or(false),
            "pre-commit managed launch failure must not leave live browser authority behind"
        );
    }
}
