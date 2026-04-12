//! Browser launch, tab projection, and health checking.

mod runtime_authority;
mod runtime_callbacks;
#[cfg(test)]
mod tests;

use chromiumoxide::Page;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, EventTargetCreated, EventTargetDestroyed, EventTargetInfoChanged, TargetId,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::Mutex;
use tracing::info;

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    ConnectionTarget, LaunchPolicyInfo, LoadStrategy, NetworkRule, StealthCoverageInfo, TabInfo,
};

use crate::tab_projection::{EpochCallback, PageHookInstallState, ProjectionContext};
use crate::{
    dialogs::{
        DialogCallbacks, SharedDialogIntercept, SharedDialogRuntime, new_shared_dialog_runtime,
    },
    downloads::DownloadCallbacks,
    identity_coverage::IdentityCoverageRegistry,
    identity_policy::IdentityPolicy,
    listener_generation::{
        ListenerGeneration, ListenerGenerationRx, ListenerGenerationTx,
        new_listener_generation_channel,
    },
    managed_browser::{resolve_managed_profile_dir, shutdown_managed_browser},
    network_rules::NetworkRuleRuntime,
    request_correlation::RequestCorrelationRegistry,
    runtime_observatory::ObservatoryCallbacks,
    runtime_state::RuntimeStateCallbacks,
};

pub use crate::attachment::{CdpCandidate, discover_local_cdp};

#[derive(Debug, Clone)]
pub struct BrowserLaunchOptions {
    pub headless: bool,
    pub ignore_cert_errors: bool,
    pub user_data_dir: Option<PathBuf>,
    pub download_dir: Option<PathBuf>,
    pub profile_directory: Option<String>,
    pub hide_infobars: bool,
    /// L1 stealth baseline enabled (default: true). DOM snapshot hygiene
    /// remains enabled even when this is false because it is part of snapshot
    /// correctness, not only anti-detection behavior.
    pub stealth: bool,
}

/// Manages browser lifecycle and the tab projection used by the daemon.
pub struct BrowserManager {
    browser: Arc<Mutex<Option<Arc<Browser>>>>,
    launch_lock: Arc<Mutex<()>>,
    page: Arc<Mutex<Option<Arc<Page>>>>,
    pages: Arc<Mutex<Vec<Arc<Page>>>>,
    active_target_id: Arc<Mutex<Option<TargetId>>>,
    page_hook_states: Arc<Mutex<HashMap<String, PageHookInstallState>>>,
    options: BrowserLaunchOptions,
    headless_mode: StdRwLock<bool>,
    identity_seed: u64,
    identity_coverage: Arc<Mutex<IdentityCoverageRegistry>>,
    epoch_callback: Arc<Mutex<Option<EpochCallback>>>,
    observatory_callbacks: Arc<Mutex<ObservatoryCallbacks>>,
    runtime_state_callbacks: Arc<Mutex<RuntimeStateCallbacks>>,
    dialog_callbacks: Arc<Mutex<DialogCallbacks>>,
    dialog_runtime: SharedDialogRuntime,
    /// One-shot pre-registered intercept policy for the next JavaScript dialog.
    dialog_intercept: SharedDialogIntercept,
    download_callbacks: Arc<Mutex<DownloadCallbacks>>,
    network_rule_runtime: Arc<tokio::sync::RwLock<NetworkRuleRuntime>>,
    request_correlation: Arc<Mutex<RequestCorrelationRegistry>>,
    observatory_pending_registries:
        Arc<Mutex<HashMap<String, crate::runtime_observatory::SharedPendingRequestRegistry>>>,
    listener_generation_tx: ListenerGenerationTx,
    /// True when connected to an externally-managed browser (not launched by us).
    is_external: Arc<Mutex<bool>>,
    /// Connection target metadata for diagnostics.
    connection_target: Arc<Mutex<Option<ConnectionTarget>>>,
    launch_policy_projection: Arc<StdRwLock<LaunchPolicyProjection>>,
    #[cfg(test)]
    force_reconcile_runtime_callbacks_failure: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone, Default)]
struct LaunchPolicyProjection {
    connection_target: Option<ConnectionTarget>,
    stealth_coverage: Option<StealthCoverageInfo>,
}

impl BrowserManager {
    pub fn new(options: BrowserLaunchOptions) -> Self {
        let identity_policy = IdentityPolicy::from_options(&options);
        let initial_identity_coverage = IdentityCoverageRegistry::new(&identity_policy);
        let initial_stealth_coverage = initial_identity_coverage.project();
        let identity_coverage = Arc::new(Mutex::new(initial_identity_coverage));
        let initial_headless = options.headless;
        let identity_seed = crate::fingerprint_profile::generate_session_seed();
        let (listener_generation_tx, _) = new_listener_generation_channel();
        Self {
            browser: Arc::new(Mutex::new(None)),
            launch_lock: Arc::new(Mutex::new(())),
            page: Arc::new(Mutex::new(None)),
            pages: Arc::new(Mutex::new(Vec::new())),
            active_target_id: Arc::new(Mutex::new(None)),
            page_hook_states: Arc::new(Mutex::new(HashMap::new())),
            options,
            headless_mode: StdRwLock::new(initial_headless),
            identity_seed,
            identity_coverage,
            epoch_callback: Arc::new(Mutex::new(None)),
            observatory_callbacks: Arc::new(Mutex::new(ObservatoryCallbacks::default())),
            runtime_state_callbacks: Arc::new(Mutex::new(RuntimeStateCallbacks::default())),
            dialog_callbacks: Arc::new(Mutex::new(DialogCallbacks::default())),
            dialog_runtime: new_shared_dialog_runtime(),
            dialog_intercept: Arc::new(std::sync::Mutex::new(
                None::<rub_core::model::DialogInterceptPolicy>,
            )),
            download_callbacks: Arc::new(Mutex::new(DownloadCallbacks::default())),
            network_rule_runtime: Arc::new(tokio::sync::RwLock::new(NetworkRuleRuntime::default())),
            request_correlation: Arc::new(Mutex::new(RequestCorrelationRegistry::default())),
            observatory_pending_registries: Arc::new(Mutex::new(HashMap::new())),
            listener_generation_tx,
            is_external: Arc::new(Mutex::new(false)),
            connection_target: Arc::new(Mutex::new(None)),
            launch_policy_projection: Arc::new(StdRwLock::new(LaunchPolicyProjection {
                connection_target: None,
                stealth_coverage: Some(initial_stealth_coverage),
            })),
            #[cfg(test)]
            force_reconcile_runtime_callbacks_failure: Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
        }
    }

    fn current_headless(&self) -> bool {
        *self
            .headless_mode
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_current_headless(&self, headless: bool) {
        *self
            .headless_mode
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = headless;
    }

    fn current_options(&self) -> BrowserLaunchOptions {
        let mut options = self.options.clone();
        options.headless = self.current_headless();
        options
    }

    fn current_identity_policy(&self) -> IdentityPolicy {
        IdentityPolicy::from_options_with_seed(&self.current_options(), self.identity_seed)
    }

    async fn reset_identity_coverage(&self) {
        let identity_policy = self.current_identity_policy();
        let coverage = IdentityCoverageRegistry::new(&identity_policy);
        let projected = coverage.project();
        *self.identity_coverage.lock().await = coverage;
        self.update_stealth_coverage_projection(Some(projected));
    }

    fn update_connection_target_projection(&self, target: Option<ConnectionTarget>) {
        self.launch_policy_projection
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .connection_target = target;
    }

    fn update_stealth_coverage_projection(&self, coverage: Option<StealthCoverageInfo>) {
        self.launch_policy_projection
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stealth_coverage = coverage;
    }

    fn bump_listener_generation(&self) -> ListenerGeneration {
        let next = self.listener_generation_tx.borrow().saturating_add(1);
        self.listener_generation_tx.send_replace(next);
        next
    }

    fn retire_uncommitted_listener_generation(&self, generation: ListenerGeneration) {
        if self.listener_generation() == generation {
            self.bump_listener_generation();
        }
    }

    fn listener_generation(&self) -> ListenerGeneration {
        *self.listener_generation_tx.borrow()
    }

    pub fn current_listener_generation(&self) -> ListenerGeneration {
        self.listener_generation()
    }

    fn listener_generation_receiver(&self) -> ListenerGenerationRx {
        self.listener_generation_tx.subscribe()
    }

    /// Launch the browser if not already running.
    pub async fn ensure_browser(&self) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await
    }

    async fn ensure_browser_locked(&self) -> Result<(), RubError> {
        {
            let browser_guard = self.browser.lock().await;
            if browser_guard.is_some() {
                return Ok(());
            }
        }

        let existing_target = self.connection_target.lock().await.clone();
        let install = self
            .resolve_browser_authority_install(existing_target)
            .await?;
        self.install_runtime_state(
            install.browser,
            install.page,
            install.is_external,
            install.connection_target,
        )
        .await?;

        info!("Browser launched successfully");
        Ok(())
    }

    /// Set callback for epoch increment on CDP events.
    pub async fn set_epoch_callback(&self, callback: Box<dyn Fn() + Send + Sync>) {
        *self.epoch_callback.lock().await = Some(Arc::new(callback));
    }

    /// Set callback sinks for runtime observability event projection.
    pub async fn set_observatory_callbacks(
        &self,
        callbacks: ObservatoryCallbacks,
    ) -> Result<(), RubError> {
        self.reconfigure_runtime_callbacks(&self.observatory_callbacks, callbacks, "observatory")
            .await
    }

    /// Set callback sinks for page-derived runtime state projection.
    ///
    /// This must be truthful: swallowing hook-rebuild failures would leave the
    /// session projection stale while callers believe runtime callbacks are live.
    pub async fn set_runtime_state_callbacks(
        &self,
        callbacks: RuntimeStateCallbacks,
    ) -> Result<(), RubError> {
        self.reconfigure_runtime_callbacks(
            &self.runtime_state_callbacks,
            callbacks,
            "runtime_state",
        )
        .await
    }

    /// Set callback sinks for page-level JavaScript dialog runtime projection.
    pub async fn set_dialog_callbacks(&self, callbacks: DialogCallbacks) -> Result<(), RubError> {
        self.reconfigure_runtime_callbacks(&self.dialog_callbacks, callbacks, "dialog")
            .await
    }

    pub fn dialog_runtime(&self) -> SharedDialogRuntime {
        self.dialog_runtime.clone()
    }

    /// Arm a one-shot intercept policy for the next JavaScript dialog.
    ///
    /// When a `Page.javascriptDialogOpening` event fires, the CDP listener
    /// task consumes this policy and immediately calls `Page.handleJavaScriptDialog`
    /// — before Chrome's built-in handler auto-dismisses the dialog.
    pub fn set_dialog_intercept(
        &self,
        policy: rub_core::model::DialogInterceptPolicy,
    ) -> Result<(), RubError> {
        let mut guard = self.dialog_intercept.lock().map_err(|_| {
            RubError::domain_with_context(
                ErrorCode::InternalError,
                "dialog intercept state lock poisoned",
                serde_json::json!({
                    "operation": "set_dialog_intercept",
                }),
            )
        })?;
        *guard = Some(policy);
        Ok(())
    }

    /// Cancel any pending one-shot dialog intercept policy.
    pub fn clear_dialog_intercept(&self) -> Result<(), RubError> {
        let mut guard = self.dialog_intercept.lock().map_err(|_| {
            RubError::domain_with_context(
                ErrorCode::InternalError,
                "dialog intercept state lock poisoned",
                serde_json::json!({
                    "operation": "clear_dialog_intercept",
                }),
            )
        })?;
        *guard = None;
        Ok(())
    }

    fn recover_dialog_intercept_after_authority_reset(&self) {
        match self.dialog_intercept.lock() {
            Ok(mut guard) => {
                *guard = None;
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = None;
                self.dialog_intercept.clear_poison();
                tracing::warn!(
                    "Recovered poisoned dialog intercept state during browser authority reset"
                );
            }
        }
    }

    /// Set callback sinks for browser-level download runtime projection.
    pub async fn set_download_callbacks(
        &self,
        callbacks: DownloadCallbacks,
    ) -> Result<(), RubError> {
        self.reconfigure_runtime_callbacks(&self.download_callbacks, callbacks, "download")
            .await
    }

    /// Replace the current browser-side mirror of the session-scoped network
    /// rule list and resync Fetch interception across active pages.
    pub async fn sync_network_rules(&self, rules: Vec<NetworkRule>) -> Result<(), RubError> {
        self.ensure_browser().await?;
        self.network_rule_runtime.write().await.replace_rules(rules);
        self.sync_tabs_projection().await?;
        let pages = self.pages.lock().await.clone();
        crate::network_rules::sync_fetch_domain_for_pages(&pages, self.network_rule_runtime.clone())
            .await
    }

    /// Set connection target metadata for diagnostics on managed launches.
    pub async fn set_connection_target(&self, target: ConnectionTarget) {
        *self.connection_target.lock().await = Some(target.clone());
        self.update_connection_target_projection(Some(target));
    }

    pub fn launch_policy_info(&self) -> LaunchPolicyInfo {
        let cached_projection = self
            .launch_policy_projection
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let connection_target = self
            .connection_target
            .try_lock()
            .ok()
            .and_then(|guard| guard.clone())
            .or(cached_projection.connection_target.clone());
        let options = self.current_options();
        let stealth_cfg = self.current_identity_policy().stealth_config();
        let stealth_patches = crate::tab_projection::projected_stealth_patch_names(
            &options,
            connection_target.as_ref(),
            &stealth_cfg,
        );
        LaunchPolicyInfo {
            headless: options.headless,
            ignore_cert_errors: options.ignore_cert_errors,
            hide_infobars: options.hide_infobars,
            user_data_dir: self
                .current_options()
                .user_data_dir
                .as_ref()
                .map(|path| path.display().to_string()),
            connection_target,
            stealth_level: Some(if options.stealth {
                "L1".to_string()
            } else {
                "L0".to_string()
            }),
            stealth_patches: Some(stealth_patches),
            stealth_default_enabled: Some(options.stealth),
            humanize_enabled: None,
            humanize_speed: None,
            stealth_coverage: self
                .identity_coverage
                .try_lock()
                .ok()
                .map(|coverage| coverage.project())
                .or(cached_projection.stealth_coverage),
        }
    }

    /// Whether this manager is connected to an external browser.
    pub async fn is_external(&self) -> bool {
        *self.is_external.lock().await
    }

    /// Connect to an externally-running browser at the given WebSocket or HTTP URL.
    ///
    /// The daemon will NOT own the browser process — `close()` will only
    /// disconnect the CDP session, not kill the browser.
    pub async fn connect_to_external(
        &self,
        url: &str,
        target: ConnectionTarget,
    ) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        let (browser, page, connect_url) = crate::runtime::attach_external_browser(url).await?;
        self.replace_browser_authority(browser, page, true, Some(target))
            .await?;

        info!(
            external = true,
            url = url,
            resolved_ws_url = connect_url,
            "Connected to external browser"
        );
        Ok(())
    }

    /// Get the current active page.
    pub async fn page(&self) -> Result<Arc<Page>, RubError> {
        self.ensure_browser().await?;
        self.sync_tabs_projection().await?;

        self.page
            .lock()
            .await
            .clone()
            .ok_or_else(|| RubError::domain(ErrorCode::BrowserCrashed, "No active page"))
    }

    /// Handle a pending dialog on the page authority that actually surfaced it.
    ///
    /// Dialog runtime is session-scoped for projection convenience, but dialog
    /// actuation must still bind back to the originating page target. Falling
    /// back to the current active page would allow a background-tab dialog to
    /// be accepted or dismissed against the wrong page authority.
    pub async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> Result<(), RubError> {
        let target_id = crate::dialogs::pending_dialog(&self.dialog_runtime())
            .await
            .and_then(|dialog| dialog.tab_target_id);
        let page = if let Some(target_id) = target_id {
            self.page_for_target_id(&target_id).await.map_err(|error| {
                RubError::domain_with_context(
                    ErrorCode::InvalidInput,
                    format!("Pending JavaScript dialog target '{target_id}' is no longer live"),
                    serde_json::json!({
                        "pending_dialog_target_id": target_id,
                        "reason": error.to_string(),
                    }),
                )
            })?
        } else {
            self.page().await?
        };
        crate::dialogs::handle_dialog(&page, accept, prompt_text).await
    }

    /// Get one live page handle by stable target identity without mutating the
    /// active-tab authority.
    pub async fn page_for_target_id(&self, target_id: &str) -> Result<Arc<Page>, RubError> {
        self.ensure_browser().await?;
        self.sync_tabs_projection().await?;

        self.pages
            .lock()
            .await
            .iter()
            .find(|page| page.target_id().as_ref() == target_id)
            .cloned()
            .ok_or_else(|| {
                RubError::domain(
                    ErrorCode::TabNotFound,
                    format!("Tab target '{target_id}' is not present in the current session"),
                )
            })
    }

    /// Get all pages as TabInfo list.
    pub async fn tab_list(&self) -> Result<Vec<TabInfo>, RubError> {
        self.ensure_browser().await?;
        self.sync_tabs_projection().await?;

        let pages = self.pages.lock().await.clone();
        let active_target_id = self.active_target_id.lock().await.clone();

        let mut tabs = Vec::with_capacity(pages.len());
        for (index, page) in pages.iter().enumerate() {
            tabs.push(
                crate::tab_projection::tab_info_for_page(
                    index as u32,
                    page,
                    active_target_id.as_ref(),
                )
                .await,
            );
        }
        Ok(tabs)
    }

    /// Switch to a tab by index and mark it as the active tab.
    pub async fn switch_to_tab(&self, index: u32) -> Result<TabInfo, RubError> {
        self.ensure_browser().await?;
        self.sync_tabs_projection().await?;

        let pages = self.pages.lock().await.clone();
        let idx = index as usize;
        if idx >= pages.len() {
            return Err(crate::tab_projection::tab_not_found(index, pages.len()));
        }

        let target_page = pages[idx].clone();
        target_page.activate().await.map_err(|e| {
            RubError::Internal(format!("ActivateTarget failed for tab {index}: {e}"))
        })?;

        *self.active_target_id.lock().await = Some(target_page.target_id().clone());
        *self.page.lock().await = Some(target_page.clone());
        self.sync_tabs_projection().await?;

        Ok(crate::tab_projection::tab_info_for_page(
            index,
            &target_page,
            Some(target_page.target_id()),
        )
        .await)
    }

    /// Close a tab by index. If it is the last tab, create `about:blank` first.
    pub async fn close_tab_at(&self, index: Option<u32>) -> Result<Vec<TabInfo>, RubError> {
        self.ensure_browser().await?;
        self.sync_tabs_projection().await?;

        let pages_before = self.pages.lock().await.clone();
        let active_before = self.active_target_id.lock().await.clone();
        let active_index = active_before
            .as_ref()
            .and_then(|target| {
                pages_before
                    .iter()
                    .position(|page| page.target_id() == target)
            })
            .unwrap_or(0);
        let idx = index.map(|v| v as usize).unwrap_or(active_index);
        if idx >= pages_before.len() {
            return Err(crate::tab_projection::tab_not_found(
                idx as u32,
                pages_before.len(),
            ));
        }

        let target_page = pages_before[idx].clone();
        let closing_active = active_before
            .as_ref()
            .map(|target| target == target_page.target_id())
            .unwrap_or(false);
        if pages_before.len() == 1 {
            target_page.goto("about:blank").await.map_err(|e| {
                RubError::Internal(format!("Failed to reset last tab to about:blank: {e}"))
            })?;
            *self.active_target_id.lock().await = Some(target_page.target_id().clone());
            *self.page.lock().await = Some(target_page);
            return self.tab_list().await;
        }

        target_page
            .execute(CloseTargetParams::new(target_page.target_id().clone()))
            .await
            .map_err(|e| RubError::Internal(format!("CloseTarget failed: {e}")))?;

        self.sync_tabs_projection().await?;

        let pages_after = self.pages.lock().await.clone();
        if let Some(active_page) = self
            .select_active_page_after_close(
                &pages_after,
                active_before.as_ref(),
                closing_active,
                idx,
            )
            .await?
            && active_page.activate().await.is_ok()
        {
            *self.active_target_id.lock().await = Some(active_page.target_id().clone());
            *self.page.lock().await = Some(active_page);
        }

        self.tab_list().await
    }

    /// CDP health check: Browser.getVersion().
    pub async fn health_check(&self) -> Result<(), RubError> {
        self.ensure_browser().await?;
        let browser = self.browser_handle().await?;
        browser
            .execute(chromiumoxide::cdp::browser_protocol::browser::GetVersionParams::default())
            .await
            .map_err(|e| {
                RubError::domain(
                    ErrorCode::BrowserCrashed,
                    format!("Health check failed: {e}"),
                )
            })?;
        Ok(())
    }

    /// Recover from a browser crash by clearing local projections and relaunching.
    pub async fn recover_browser(&self) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        tracing::warn!("Browser crash detected, auto-restarting");
        self.bump_listener_generation();
        self.clear_local_browser_authority().await;
        self.ensure_browser_locked().await
    }

    /// Close the browser.
    /// If external, only disconnects the CDP session (browser keeps running).
    pub async fn close(&self) -> Result<(), RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        self.bump_listener_generation();
        self.release_current_browser_authority().await?;
        Ok(())
    }

    /// Relaunch a managed headless browser as a visible managed browser while
    /// keeping the same session/profile authority.
    pub async fn elevate_to_visible(&self) -> Result<LaunchPolicyInfo, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        if *self.is_external.lock().await {
            return Err(RubError::domain(
                ErrorCode::InvalidInput,
                "Takeover elevation is only supported for managed browser sessions",
            ));
        }

        let previous_options = self.current_options();
        if !previous_options.headless {
            return Ok(self.launch_policy_info());
        }

        self.bump_listener_generation();
        let restore_url = self.current_restore_url().await;
        self.release_current_browser_authority().await?;

        self.set_current_headless(false);
        self.reset_identity_coverage().await;

        if let Err(error) = self
            .relaunch_and_restore_visible_locked(restore_url.as_deref())
            .await
        {
            let rollback_cleanup = self.release_current_browser_authority().await;
            self.set_current_headless(previous_options.headless);
            self.reset_identity_coverage().await;
            let restored = self
                .relaunch_and_restore_visible_locked(restore_url.as_deref())
                .await;
            return Err(match restored {
                Ok(()) => RubError::domain_with_context(
                    ErrorCode::BrowserLaunchFailed,
                    format!("Failed to elevate session to a visible browser: {error}"),
                    serde_json::json!({
                        "restore_succeeded": true,
                        "rollback_cleanup_succeeded": rollback_cleanup.is_ok(),
                        "target_visibility": "headed",
                    }),
                ),
                Err(restore_error) => RubError::domain_with_context(
                    ErrorCode::BrowserLaunchFailed,
                    format!("Failed to elevate session to a visible browser: {error}"),
                    serde_json::json!({
                        "restore_succeeded": false,
                        "rollback_cleanup_succeeded": rollback_cleanup.is_ok(),
                        "rollback_cleanup_error": rollback_cleanup.err().map(|e| e.to_string()),
                        "restore_error": restore_error.to_string(),
                        "target_visibility": "headed",
                    }),
                ),
            });
        }

        Ok(self.launch_policy_info())
    }

    async fn current_restore_url(&self) -> Option<String> {
        let page = self.page.lock().await.clone();
        let current_url = match page {
            Some(page) => page.url().await.ok().flatten().map(|url| url.to_string()),
            None => None,
        };
        current_url.filter(|url| !url.is_empty() && url != "about:blank")
    }

    async fn relaunch_and_restore_visible_locked(
        &self,
        restore_url: Option<&str>,
    ) -> Result<(), RubError> {
        self.ensure_browser_locked().await?;
        if let Some(url) = restore_url {
            let page = self
                .page
                .lock()
                .await
                .clone()
                .ok_or_else(|| RubError::domain(ErrorCode::BrowserCrashed, "No active page"))?;
            crate::page::navigate(
                &page,
                url,
                LoadStrategy::Load,
                std::time::Duration::from_millis(rub_core::DEFAULT_WAIT_TIMEOUT_MS),
            )
            .await?;
            self.sync_tabs_projection().await?;
        }
        Ok(())
    }

    pub async fn cancel_download(&self, guid: &str) -> Result<(), RubError> {
        let browser = self.browser_handle().await?;
        crate::downloads::cancel_download(&browser, guid).await
    }

    async fn browser_handle(&self) -> Result<Arc<Browser>, RubError> {
        self.browser
            .lock()
            .await
            .clone()
            .ok_or_else(|| RubError::domain(ErrorCode::BrowserCrashed, "Browser is not available"))
    }

    async fn sync_tabs_projection(&self) -> Result<(), RubError> {
        let context = self
            .projection_context(self.browser_handle().await?, self.listener_generation())
            .await;
        crate::tab_projection::sync_tabs_projection_with(
            &context,
            self.pages.clone(),
            self.page.clone(),
            self.active_target_id.clone(),
        )
        .await
    }

    async fn spawn_target_listeners(
        &self,
        browser: Arc<Browser>,
        listener_generation: ListenerGeneration,
    ) {
        let context = self
            .projection_context(browser.clone(), listener_generation)
            .await;
        if let Ok(mut listener) = browser.event_listener::<EventTargetCreated>().await {
            let pages = self.pages.clone();
            let current_page = self.page.clone();
            let active_target_id = self.active_target_id.clone();
            let context = context.clone();
            tokio::spawn(async move {
                let mut generation_rx = context.listener_generation_rx.clone();
                while let Some(event) = crate::listener_generation::next_listener_event(
                    &mut listener,
                    context.listener_generation,
                    &mut generation_rx,
                )
                .await
                {
                    context.identity_coverage.lock().await.record_target(
                        event.target_info.target_id.as_ref().to_string(),
                        event.target_info.r#type.clone(),
                    );
                    let _ = crate::tab_projection::sync_tabs_projection_with(
                        &context,
                        pages.clone(),
                        current_page.clone(),
                        active_target_id.clone(),
                    )
                    .await;
                }
            });
        }

        if let Ok(mut listener) = browser.event_listener::<EventTargetDestroyed>().await {
            let pages = self.pages.clone();
            let current_page = self.page.clone();
            let active_target_id = self.active_target_id.clone();
            let context = context.clone();
            tokio::spawn(async move {
                let mut generation_rx = context.listener_generation_rx.clone();
                while let Some(event) = crate::listener_generation::next_listener_event(
                    &mut listener,
                    context.listener_generation,
                    &mut generation_rx,
                )
                .await
                {
                    context
                        .identity_coverage
                        .lock()
                        .await
                        .remove_target(event.target_id.as_ref());
                    context
                        .page_hook_states
                        .lock()
                        .await
                        .remove(event.target_id.as_ref());
                    let _ = crate::tab_projection::sync_tabs_projection_with(
                        &context,
                        pages.clone(),
                        current_page.clone(),
                        active_target_id.clone(),
                    )
                    .await;
                }
            });
        }

        if let Ok(mut listener) = browser.event_listener::<EventTargetInfoChanged>().await {
            let pages = self.pages.clone();
            let current_page = self.page.clone();
            let active_target_id = self.active_target_id.clone();
            let context = context.clone();
            tokio::spawn(async move {
                let mut generation_rx = context.listener_generation_rx.clone();
                while let Some(event) = crate::listener_generation::next_listener_event(
                    &mut listener,
                    context.listener_generation,
                    &mut generation_rx,
                )
                .await
                {
                    context.identity_coverage.lock().await.record_target(
                        event.target_info.target_id.as_ref().to_string(),
                        event.target_info.r#type.clone(),
                    );
                    let _ = crate::tab_projection::sync_tabs_projection_with(
                        &context,
                        pages.clone(),
                        current_page.clone(),
                        active_target_id.clone(),
                    )
                    .await;
                }
            });
        }
    }

    async fn spawn_epoch_listeners(
        &self,
        page: Arc<Page>,
        listener_generation: ListenerGeneration,
    ) {
        if let Ok(browser) = self.browser_handle().await {
            let context = self.projection_context(browser, listener_generation).await;
            let _ = crate::tab_projection::ensure_page_hooks(page, &context, false).await;
        }
    }

    async fn select_active_page_after_close(
        &self,
        pages_after: &[Arc<Page>],
        active_before: Option<&TargetId>,
        closing_active: bool,
        closed_index: usize,
    ) -> Result<Option<Arc<Page>>, RubError> {
        if pages_after.is_empty() {
            return Ok(None);
        }

        if closing_active {
            let fallback_index = closed_index.min(pages_after.len().saturating_sub(1));
            return Ok(pages_after.get(fallback_index).cloned());
        }

        if let Some(active_target) = active_before
            && let Some(page) = pages_after
                .iter()
                .find(|page| page.target_id() == active_target)
                .cloned()
        {
            return Ok(Some(page));
        }

        Ok(pages_after.first().cloned())
    }
}
