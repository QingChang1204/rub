use chromiumoxide::Page;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::TargetId;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{ConnectionTarget, TabInfo};
use serde::Deserialize;
use std::sync::Arc;
use tokio::time::{Duration, sleep};

use crate::browser::BrowserLaunchOptions;

const ACTIVE_TAB_PROBE_TIMEOUT: Duration = Duration::from_millis(150);
const TAB_INFO_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const TAB_URL_PROBE_UNAVAILABLE: &str = "about:rub-probe-unavailable";
const TAB_TITLE_PROBE_UNAVAILABLE: &str = "[probe unavailable]";
const LOCAL_ACTIVE_TARGET_AUTHORITY_MAX_AMBIGUOUS_SYNCS: u8 = 2;

#[derive(Clone, Default)]
pub(crate) struct CommittedTabProjection {
    pub(crate) pages: Vec<Arc<Page>>,
    pub(crate) current_page: Option<Arc<Page>>,
    pub(crate) continuity_page: Option<Arc<Page>>,
    pub(crate) active_target_id: Option<TargetId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
struct ActiveTabProbe {
    visible: bool,
    focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalActiveTargetAuthority {
    pub(crate) target_id: TargetId,
    remaining_ambiguous_syncs: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveTargetAuthorityResolution {
    pub(crate) active_target: Option<TargetId>,
    pub(crate) next_local_active_target_authority: Option<LocalActiveTargetAuthority>,
}

impl LocalActiveTargetAuthority {
    pub(crate) fn new(target_id: TargetId) -> Self {
        Self {
            target_id,
            remaining_ambiguous_syncs: LOCAL_ACTIVE_TARGET_AUTHORITY_MAX_AMBIGUOUS_SYNCS,
        }
    }
}

impl CommittedTabProjection {
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn single(page: Arc<Page>) -> Self {
        let active_target_id = Some(page.target_id().clone());
        Self {
            pages: vec![page.clone()],
            continuity_page: Some(page.clone()),
            current_page: Some(page),
            active_target_id,
        }
    }

    pub(crate) fn from_projected_pages(
        pages: Vec<Arc<Page>>,
        active_target_id: Option<TargetId>,
        previous_continuity_target_id: Option<&TargetId>,
    ) -> Self {
        let current_page = active_target_id.as_ref().and_then(|active_target_id| {
            pages
                .iter()
                .find(|page| page.target_id() == active_target_id)
                .cloned()
        });
        let continuity_page = previous_continuity_target_id
            .and_then(|previous_continuity_target_id| {
                pages
                    .iter()
                    .find(|page| page.target_id() == previous_continuity_target_id)
                    .cloned()
            })
            .or_else(|| current_page.clone())
            .or_else(|| (pages.len() == 1).then(|| pages[0].clone()));
        Self {
            pages,
            current_page,
            continuity_page,
            active_target_id,
        }
    }

    pub(crate) fn with_local_active_page(mut self, page: Arc<Page>) -> Self {
        self.active_target_id = Some(page.target_id().clone());
        self.current_page = Some(page.clone());
        self.continuity_page = Some(page);
        self
    }

    pub(crate) fn continuity_target_id(&self) -> Option<TargetId> {
        self.continuity_page
            .as_ref()
            .map(|page| page.target_id().clone())
    }
}

pub(crate) fn projected_stealth_patch_names(
    options: &BrowserLaunchOptions,
    connection_target: Option<&ConnectionTarget>,
    config: &crate::stealth::StealthConfig,
) -> Vec<String> {
    let mut patches = crate::stealth::applied_patch_names(config);
    if options.stealth && is_managed_launch_projection(connection_target) {
        patches.push("clean_chrome_args".to_string());
    }
    patches
}

pub(crate) async fn wait_for_startup_page(browser: &mut Browser) -> Result<Page, RubError> {
    const STARTUP_PAGE_POLL_ATTEMPTS: usize = 20;
    const STARTUP_PAGE_POLL_INTERVAL_MS: u64 = 50;
    let mut last_error =
        "Browser did not expose an authoritative startup page before startup commit".to_string();

    for attempt in 0..STARTUP_PAGE_POLL_ATTEMPTS {
        let pages = browser.pages().await.map_err(|e| {
            RubError::domain(
                ErrorCode::BrowserLaunchFailed,
                format!("Failed to enumerate startup pages: {e}"),
            )
        })?;

        if pages.is_empty() {
            last_error =
                "Browser did not expose any startup pages before startup commit".to_string();
        } else if pages.len() == 1 {
            return Ok(pages.into_iter().next().expect("single startup page"));
        } else {
            if let Some(index) = resolve_active_page_index_from_browser_truth(&pages).await {
                return Ok(pages
                    .into_iter()
                    .nth(index)
                    .expect("browser-truth startup page index should be valid"));
            }
            last_error =
                "Browser did not expose a unique authoritative startup page before startup commit"
                    .to_string();
        }

        if attempt + 1 < STARTUP_PAGE_POLL_ATTEMPTS {
            sleep(Duration::from_millis(STARTUP_PAGE_POLL_INTERVAL_MS)).await;
        }
    }

    Err(RubError::domain(ErrorCode::BrowserLaunchFailed, last_error))
}

pub(crate) async fn tab_info_for_page(
    index: u32,
    page: &Arc<Page>,
    active: Option<&TargetId>,
) -> TabInfo {
    let url = match tokio::time::timeout(TAB_INFO_PROBE_TIMEOUT, page.url()).await {
        Ok(Ok(Some(url))) => projected_tab_url(Some(url.to_string())),
        _ => projected_tab_url(None),
    };
    let title = match tokio::time::timeout(TAB_INFO_PROBE_TIMEOUT, page.get_title()).await {
        Ok(Ok(Some(title))) => projected_tab_title(Some(title)),
        _ => projected_tab_title(None),
    };

    TabInfo {
        index,
        target_id: page.target_id().as_ref().to_string(),
        url: normalize_tab_url(url),
        title,
        active: active
            .map(|target| target == page.target_id())
            .unwrap_or(false),
    }
}

pub(crate) fn tab_not_found(index: u32, total: usize) -> RubError {
    RubError::domain(
        ErrorCode::TabNotFound,
        format!(
            "Tab index {} out of range (0..{})",
            index,
            total.saturating_sub(1)
        ),
    )
}

pub(crate) async fn resolve_active_target_from_browser_truth(
    pages: &[Arc<Page>],
) -> Option<TargetId> {
    if pages.is_empty() {
        return None;
    }
    if pages.len() == 1 {
        return Some(pages[0].target_id().clone());
    }

    let mut probe_states = Vec::with_capacity(pages.len());
    for page in pages {
        probe_states.push(probe_active_tab_state(page).await);
    }

    choose_active_probe_index(probe_states.iter()).map(|index| pages[index].target_id().clone())
}

pub(crate) async fn resolve_active_page_index_from_browser_truth(pages: &[Page]) -> Option<usize> {
    if pages.is_empty() {
        return None;
    }
    if pages.len() == 1 {
        return Some(0);
    }

    let mut probe_states = Vec::with_capacity(pages.len());
    for page in pages {
        probe_states.push(probe_active_tab_state(page).await);
    }

    choose_active_probe_index(probe_states.iter())
}

pub(crate) fn resolve_active_target_authority<'a, I>(
    live_target_ids: I,
    browser_truth: Option<&'a TargetId>,
    local_active_target_authority: Option<&'a LocalActiveTargetAuthority>,
) -> ActiveTargetAuthorityResolution
where
    I: IntoIterator<Item = &'a TargetId>,
{
    let live_target_ids = live_target_ids.into_iter().collect::<Vec<_>>();
    if let Some(browser_truth) = browser_truth {
        return ActiveTargetAuthorityResolution {
            active_target: Some(browser_truth.clone()),
            next_local_active_target_authority: None,
        };
    }

    let Some(local_active_target_authority) = local_active_target_authority else {
        return ActiveTargetAuthorityResolution {
            active_target: None,
            next_local_active_target_authority: None,
        };
    };

    if !live_target_ids
        .iter()
        .any(|target_id| **target_id == local_active_target_authority.target_id)
    {
        return ActiveTargetAuthorityResolution {
            active_target: None,
            next_local_active_target_authority: None,
        };
    }

    let next_local_active_target_authority =
        (local_active_target_authority.remaining_ambiguous_syncs > 1).then(|| {
            let mut next = local_active_target_authority.clone();
            next.remaining_ambiguous_syncs -= 1;
            next
        });

    ActiveTargetAuthorityResolution {
        active_target: Some(local_active_target_authority.target_id.clone()),
        next_local_active_target_authority,
    }
}

fn is_managed_launch_projection(connection_target: Option<&ConnectionTarget>) -> bool {
    !matches!(
        connection_target,
        Some(ConnectionTarget::CdpUrl { .. } | ConnectionTarget::AutoDiscovered { .. })
    )
}

fn normalize_tab_url(url: String) -> String {
    if url.starts_with("chrome://new-tab-page") || url.starts_with("chrome-search://local-ntp") {
        "about:blank".to_string()
    } else {
        url
    }
}

pub(super) fn projected_tab_url(url: Option<String>) -> String {
    url.map(normalize_tab_url)
        .unwrap_or_else(|| TAB_URL_PROBE_UNAVAILABLE.to_string())
}

pub(super) fn projected_tab_title(title: Option<String>) -> String {
    title.unwrap_or_else(|| TAB_TITLE_PROBE_UNAVAILABLE.to_string())
}

async fn probe_active_tab_state(page: &Page) -> Option<ActiveTabProbe> {
    let probe = tokio::time::timeout(
        ACTIVE_TAB_PROBE_TIMEOUT,
        page.evaluate(
            r#"(() => ({
                visible: document.visibilityState === 'visible',
                focused: document.hasFocus(),
            }))()"#,
        ),
    )
    .await
    .ok()?
    .ok()?;

    probe.into_value::<ActiveTabProbe>().ok()
}

fn choose_active_probe_index<'a, I>(probe_states: I) -> Option<usize>
where
    I: IntoIterator<Item = &'a Option<ActiveTabProbe>>,
{
    let entries = probe_states
        .into_iter()
        .enumerate()
        .collect::<Vec<(usize, &Option<ActiveTabProbe>)>>();

    let focused_visible = entries
        .iter()
        .filter_map(|(index, probe)| {
            probe
                .as_ref()
                .filter(|probe| probe.focused && probe.visible)
                .map(|_| *index)
        })
        .collect::<Vec<_>>();
    if focused_visible.len() == 1 {
        return focused_visible.into_iter().next();
    }

    let visible = entries
        .iter()
        .filter_map(|(index, probe)| probe.as_ref().filter(|probe| probe.visible).map(|_| *index))
        .collect::<Vec<_>>();
    if visible.len() == 1 {
        return visible.into_iter().next();
    }

    let focused = entries
        .iter()
        .filter_map(|(index, probe)| probe.as_ref().filter(|probe| probe.focused).map(|_| *index))
        .collect::<Vec<_>>();
    if focused.len() == 1 {
        return focused.into_iter().next();
    }

    None
}

#[cfg(test)]
fn choose_active_target_from_probe_states<'a, I, J>(
    target_ids: I,
    probe_states: J,
) -> Option<&'a TargetId>
where
    I: IntoIterator<Item = &'a TargetId>,
    J: IntoIterator<Item = &'a Option<ActiveTabProbe>>,
{
    let entries = target_ids
        .into_iter()
        .zip(probe_states)
        .collect::<Vec<(&TargetId, &Option<ActiveTabProbe>)>>();
    choose_active_probe_index(entries.iter().map(|(_, probe)| *probe)).map(|index| entries[index].0)
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveTabProbe, CommittedTabProjection, LocalActiveTargetAuthority,
        choose_active_probe_index, choose_active_target_from_probe_states,
        resolve_active_target_authority,
    };
    use chromiumoxide::cdp::browser_protocol::target::TargetId;

    fn target(id: &str) -> TargetId {
        TargetId::from(id.to_string())
    }

    #[test]
    fn active_target_prefers_unique_focused_visible_probe() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");
        let target = choose_active_target_from_probe_states(
            [&tab_a, &tab_b],
            [
                &Some(ActiveTabProbe {
                    visible: false,
                    focused: false,
                }),
                &Some(ActiveTabProbe {
                    visible: true,
                    focused: true,
                }),
            ],
        )
        .expect("focused+visible browser truth should win");
        assert_eq!(target, &tab_b);
    }

    #[test]
    fn active_target_prefers_unique_visible_probe_when_focus_is_unavailable() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");
        let target = choose_active_target_from_probe_states(
            [&tab_a, &tab_b],
            [
                &Some(ActiveTabProbe {
                    visible: false,
                    focused: false,
                }),
                &Some(ActiveTabProbe {
                    visible: true,
                    focused: false,
                }),
            ],
        )
        .expect("unique visible browser truth should win");
        assert_eq!(target, &tab_b);
    }

    #[test]
    fn active_target_degrades_when_probe_is_ambiguous() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");
        let target = choose_active_target_from_probe_states(
            [&tab_a, &tab_b],
            [
                &Some(ActiveTabProbe {
                    visible: false,
                    focused: false,
                }),
                &Some(ActiveTabProbe {
                    visible: false,
                    focused: false,
                }),
            ],
        );
        assert!(
            target.is_none(),
            "ambiguous browser-side probes must fail closed instead of reusing a stale hint"
        );
    }

    #[test]
    fn active_page_index_prefers_unique_focused_visible_probe() {
        let index = choose_active_probe_index([
            &Some(ActiveTabProbe {
                visible: false,
                focused: false,
            }),
            &Some(ActiveTabProbe {
                visible: true,
                focused: true,
            }),
        ]);
        assert_eq!(index, Some(1));
    }

    #[test]
    fn active_page_index_degrades_when_browser_truth_is_ambiguous() {
        let index = choose_active_probe_index([
            &Some(ActiveTabProbe {
                visible: true,
                focused: false,
            }),
            &Some(ActiveTabProbe {
                visible: true,
                focused: false,
            }),
        ]);
        assert!(
            index.is_none(),
            "startup/external page selection must fail closed when browser-truth probes are ambiguous"
        );
    }

    #[test]
    fn local_active_target_authority_bridges_ambiguous_browser_probe() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");

        let resolution = resolve_active_target_authority(
            [&tab_a, &tab_b],
            None,
            Some(&LocalActiveTargetAuthority::new(tab_b.clone())),
        );
        assert_eq!(resolution.active_target, Some(tab_b));
        assert!(
            resolution.next_local_active_target_authority.is_some(),
            "local actuation authority should bridge browser-side ambiguity for a bounded handoff window"
        );
    }

    #[test]
    fn browser_truth_clears_local_active_target_authority_once_it_converges() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");

        let resolution = resolve_active_target_authority(
            [&tab_a, &tab_b],
            Some(&tab_a),
            Some(&LocalActiveTargetAuthority::new(tab_b.clone())),
        );
        assert!(
            resolution.next_local_active_target_authority.is_none(),
            "local actuation authority must clear once browser truth is authoritative"
        );
        assert_eq!(resolution.active_target, Some(tab_a));
    }

    #[test]
    fn stale_local_active_target_authority_clears_when_target_is_gone() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");

        let resolution = resolve_active_target_authority(
            [&tab_a],
            None,
            Some(&LocalActiveTargetAuthority::new(tab_b)),
        );
        assert!(
            resolution.active_target.is_none(),
            "missing local target cannot stay authoritative after it leaves the live tab set"
        );
        assert!(
            resolution.next_local_active_target_authority.is_none(),
            "stale local actuation authority must clear once its target is no longer live"
        );
    }

    #[test]
    fn local_active_target_authority_expires_after_bounded_ambiguous_syncs() {
        let tab_a = target("tab-a");
        let tab_b = target("tab-b");

        let first = resolve_active_target_authority(
            [&tab_a, &tab_b],
            None,
            Some(&LocalActiveTargetAuthority::new(tab_b.clone())),
        );
        let second = resolve_active_target_authority(
            [&tab_a, &tab_b],
            None,
            first.next_local_active_target_authority.as_ref(),
        );

        assert_eq!(second.active_target, Some(tab_b));
        assert!(
            second.next_local_active_target_authority.is_none(),
            "local actuation authority must expire after the bounded ambiguity bridge is spent"
        );
    }

    #[test]
    fn empty_committed_tab_projection_has_no_active_authority() {
        let projection = CommittedTabProjection::empty();
        assert!(projection.pages.is_empty());
        assert!(projection.current_page.is_none());
        assert!(projection.continuity_page.is_none());
        assert!(projection.active_target_id.is_none());
    }
}
