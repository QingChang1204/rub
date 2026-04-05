//! Browser runtime bootstrapping for managed launches and external attachment.

use chromiumoxide::Page;
use chromiumoxide::browser::{Browser, BrowserConfig};
use rub_core::error::{ErrorCode, RubError};
use std::sync::Arc;
use tokio::time::{Duration, Instant, sleep};

use crate::browser::BrowserLaunchOptions;
use crate::identity_policy::IdentityPolicy;
use crate::managed_browser::resolve_managed_profile_dir;

pub(crate) async fn launch_managed_browser(
    options: &BrowserLaunchOptions,
    identity_policy: &IdentityPolicy,
) -> Result<(Arc<Browser>, Arc<Page>), RubError> {
    let config = build_managed_config(options, identity_policy)?;
    let (mut browser, handler) = Browser::launch(config).await.map_err(|e| {
        RubError::domain(
            ErrorCode::BrowserLaunchFailed,
            format!("Failed to launch browser: {e}"),
        )
    })?;
    spawn_handler_loop(handler);

    let page = crate::tab_projection::wait_for_startup_page(&mut browser).await?;
    let browser = Arc::new(browser);

    Ok((browser, Arc::new(page)))
}

pub(crate) async fn attach_external_browser(
    url: &str,
) -> Result<(Arc<Browser>, Arc<Page>, String), RubError> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let (mut browser, handler, connect_url) =
        crate::attachment::connect_external_browser_until(url, deadline).await?;
    spawn_handler_loop(handler);

    wait_for_external_targets_ready(&mut browser, url, deadline).await?;

    let pages_result = wait_for_external_pages_ready(&browser, url, deadline).await?;
    let page = select_external_active_page(&mut browser, pages_result, url, deadline).await?;
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
) -> Result<(), RubError> {
    let mut last_error = format!("Failed to discover existing targets at {url}");
    loop {
        match tokio::time::timeout(remaining_attach_budget(deadline)?, browser.fetch_targets())
            .await
        {
            Ok(Ok(_targets)) => {
                sleep(Duration::from_millis(100)).await;
                return Ok(());
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
    browser: &mut Browser,
    pages: Vec<Page>,
    url: &str,
    deadline: Instant,
) -> Result<Page, RubError> {
    if pages.len() == 1 {
        return pages.into_iter().next().ok_or_else(|| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!("External browser at {url} did not expose any existing pages to attach"),
            )
        });
    }

    let targets = tokio::time::timeout(remaining_attach_budget(deadline)?, browser.fetch_targets())
        .await
        .map_err(|_| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!("Timed out resolving active page authority for external browser at {url}"),
            )
        })?
        .map_err(|error| {
            RubError::domain(
                ErrorCode::CdpConnectionFailed,
                format!(
                    "Failed to resolve active page authority for external browser at {url}: {error}"
                ),
            )
        })?;

    let attached_target_ids = targets
        .into_iter()
        .filter(|target| {
            target.r#type == "page"
                && target.attached
                && target.subtype.as_deref() != Some("prerender")
        })
        .map(|target| target.target_id.as_ref().to_string())
        .collect::<Vec<_>>();

    let attached_page_index = select_attached_external_page_index(
        &pages
            .iter()
            .map(|page| page.target_id().as_ref().to_string())
            .collect::<Vec<_>>(),
        &attached_target_ids,
    );

    let Some(index) = attached_page_index else {
        return Err(RubError::domain(
            ErrorCode::CdpConnectionFailed,
            format!("External browser at {url} did not expose any attachable page authority"),
        ));
    };

    pages.into_iter().nth(index).ok_or_else(|| {
        RubError::domain(
            ErrorCode::CdpConnectionFailed,
            format!("External browser at {url} lost the selected page authority during attach"),
        )
    })
}

fn select_attached_external_page_index(
    page_target_ids: &[String],
    attached_target_ids: &[String],
) -> Option<usize> {
    if page_target_ids.is_empty() {
        return None;
    }

    let attached_matches = page_target_ids
        .iter()
        .enumerate()
        .filter_map(|(index, target_id)| {
            attached_target_ids
                .iter()
                .any(|attached| attached == target_id)
                .then_some(index)
        })
        .collect::<Vec<_>>();

    match attached_matches.as_slice() {
        [index] => Some(*index),
        [index, ..] => Some(*index),
        [] => Some(0),
    }
}

pub(crate) fn select_attached_page_index(
    page_target_ids: &[String],
    attached_target_ids: &[String],
) -> Option<usize> {
    select_attached_external_page_index(page_target_ids, attached_target_ids)
}

fn build_managed_config(
    options: &BrowserLaunchOptions,
    identity_policy: &IdentityPolicy,
) -> Result<BrowserConfig, RubError> {
    let mut config_builder = if options.headless {
        BrowserConfig::builder().new_headless_mode()
    } else {
        BrowserConfig::builder().with_head()
    };
    if options.ignore_cert_errors {
        config_builder = config_builder.arg("--ignore-certificate-errors");
    }

    let profile = resolve_managed_profile_dir(options.user_data_dir.clone());
    config_builder = config_builder
        .user_data_dir(profile.path)
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

    config_builder.build().map_err(|e| {
        RubError::domain(
            ErrorCode::BrowserLaunchFailed,
            format!("Invalid browser config: {e}"),
        )
    })
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
        build_managed_config, select_attached_external_page_index, select_attached_page_index,
    };
    use crate::browser::BrowserLaunchOptions;
    use crate::identity_policy::IdentityPolicy;

    fn options(headless: bool) -> BrowserLaunchOptions {
        BrowserLaunchOptions {
            headless,
            ignore_cert_errors: false,
            user_data_dir: None,
            download_dir: None,
            profile_directory: None,
            hide_infobars: true,
            stealth: true,
        }
    }

    #[test]
    fn attached_external_page_selection_prefers_unique_attached_target() {
        let pages = vec![
            "tab-1".to_string(),
            "tab-2".to_string(),
            "tab-3".to_string(),
        ];
        let attached = vec!["tab-2".to_string()];
        assert_eq!(
            select_attached_external_page_index(&pages, &attached),
            Some(1)
        );
        assert_eq!(select_attached_page_index(&pages, &attached), Some(1));
    }

    #[test]
    fn attached_external_page_selection_falls_back_to_first_attached_page_when_ambiguous() {
        let pages = vec![
            "tab-1".to_string(),
            "tab-2".to_string(),
            "tab-3".to_string(),
        ];
        let attached = vec!["tab-1".to_string(), "tab-3".to_string()];
        assert_eq!(
            select_attached_external_page_index(&pages, &attached),
            Some(0)
        );
    }

    #[test]
    fn attached_external_page_selection_falls_back_to_first_page_without_attached_targets() {
        let pages = vec!["tab-1".to_string(), "tab-2".to_string()];
        assert_eq!(select_attached_external_page_index(&pages, &[]), Some(0));
    }

    #[test]
    fn headed_launch_config_explicitly_disables_headless_mode() {
        let options = options(false);
        let identity_policy = IdentityPolicy::from_options(&options);
        let config = build_managed_config(&options, &identity_policy).expect("config should build");
        let debug = format!("{config:?}");

        assert!(debug.contains("headless: False"), "{debug}");
    }

    #[test]
    fn headless_launch_config_uses_new_headless_mode() {
        let options = options(true);
        let identity_policy = IdentityPolicy::from_options(&options);
        let config = build_managed_config(&options, &identity_policy).expect("config should build");
        let debug = format!("{config:?}");

        assert!(debug.contains("headless: New"), "{debug}");
    }
}
