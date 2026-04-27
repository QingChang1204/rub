//! Browser launch, tab projection, and health checking.

mod control;
mod runtime_authority;
mod runtime_callbacks;
#[cfg(test)]
mod tests;

use chromiumoxide::Page;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::browser::GetVersionParams;
use chromiumoxide::cdp::browser_protocol::target::{
    CloseTargetParams, EventTargetCreated, EventTargetDestroyed, EventTargetInfoChanged, TargetId,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc, RwLock as StdRwLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{
    ConnectionTarget, DialogRuntimeInfo, LaunchPolicyInfo, LoadStrategy, NetworkRule,
    StealthCoverageInfo, TabInfo,
};

use crate::tab_projection::{EpochCallback, PageHookInstallState, ProjectionContext};
use crate::{
    dialogs::{
        DialogCallbacks, SharedDialogIntercept, SharedDialogRuntime, new_shared_dialog_runtime,
    },
    downloads::{
        DownloadCallbacks, SharedDownloadRuntimeProjection, new_shared_download_runtime_projection,
    },
    identity_coverage::IdentityCoverageRegistry,
    identity_policy::IdentityPolicy,
    listener_generation::{
        ListenerGeneration, ListenerGenerationRx, ListenerGenerationTx,
        new_listener_generation_channel,
    },
    managed_browser::{
        ManagedProfileDir, projected_managed_profile_path_for_scope, resolve_managed_profile_dir,
        shutdown_managed_browser,
    },
    network_rules::NetworkRuleRuntime,
    request_correlation::RequestCorrelationRegistry,
    runtime_observatory::ObservatoryCallbacks,
    runtime_state::RuntimeStateCallbacks,
    tab_projection::{CommittedTabProjection, LocalActiveTargetAuthority},
};

pub use crate::attachment::{CdpCandidate, discover_local_cdp};

#[derive(Debug, Clone)]
pub struct BrowserLaunchOptions {
    pub headless: bool,
    pub ignore_cert_errors: bool,
    pub user_data_dir: Option<PathBuf>,
    pub managed_profile_ephemeral: bool,
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
    authority_commit_in_progress: Arc<AtomicBool>,
    authority_release_in_progress: Arc<AtomicBool>,
    runtime_callback_reconfigure_in_progress: Arc<AtomicBool>,
    tab_projection: Arc<Mutex<CommittedTabProjection>>,
    managed_profile: Arc<Mutex<Option<ManagedProfileDir>>>,
    local_active_target_authority: Arc<Mutex<Option<LocalActiveTargetAuthority>>>,
    page_hook_states: Arc<Mutex<HashMap<String, PageHookInstallState>>>,
    runtime_callback_reconfigure_lock: Arc<Mutex<()>>,
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
    download_runtime: SharedDownloadRuntimeProjection,
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
    managed_browser_test_permit: Arc<Mutex<Option<tokio::sync::OwnedSemaphorePermit>>>,
    #[cfg(test)]
    force_reconcile_runtime_callbacks_failure: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    pause_runtime_callback_reconfigure_before_reconcile: Arc<AtomicBool>,
    #[cfg(test)]
    runtime_callback_reconfigure_paused: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    resume_runtime_callback_reconfigure: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    force_previous_authority_release_failure: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    force_current_authority_release_failure: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    runtime_state_replay_attempt_count: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    force_generation_bound_runtime_reconcile_failure: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    force_managed_profile_ownership_commit_failure: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    force_required_page_hook_install_failure: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    pause_authority_commit_after_projection: Arc<AtomicBool>,
    #[cfg(test)]
    authority_commit_projected: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    resume_authority_commit: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone, Default)]
struct LaunchPolicyProjection {
    connection_target: Option<ConnectionTarget>,
    stealth_coverage: Option<StealthCoverageInfo>,
}

impl BrowserManager {
    pub fn new(mut options: BrowserLaunchOptions) -> Self {
        let identity_seed = crate::fingerprint_profile::generate_session_seed();
        if options.user_data_dir.is_none() {
            options.user_data_dir = Some(projected_managed_profile_path_for_scope(&format!(
                "manager-{identity_seed:016x}"
            )));
            options.managed_profile_ephemeral = true;
        }
        let identity_policy = IdentityPolicy::from_options(&options);
        let initial_identity_coverage = IdentityCoverageRegistry::new(&identity_policy);
        let initial_stealth_coverage = initial_identity_coverage.project();
        let identity_coverage = Arc::new(Mutex::new(initial_identity_coverage));
        let initial_headless = options.headless;
        let (listener_generation_tx, _) = new_listener_generation_channel();
        Self {
            browser: Arc::new(Mutex::new(None)),
            launch_lock: Arc::new(Mutex::new(())),
            authority_commit_in_progress: Arc::new(AtomicBool::new(false)),
            authority_release_in_progress: Arc::new(AtomicBool::new(false)),
            runtime_callback_reconfigure_in_progress: Arc::new(AtomicBool::new(false)),
            tab_projection: Arc::new(Mutex::new(CommittedTabProjection::empty())),
            managed_profile: Arc::new(Mutex::new(None)),
            local_active_target_authority: Arc::new(Mutex::new(None)),
            page_hook_states: Arc::new(Mutex::new(HashMap::new())),
            runtime_callback_reconfigure_lock: Arc::new(Mutex::new(())),
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
            download_runtime: new_shared_download_runtime_projection(),
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
            managed_browser_test_permit: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            force_reconcile_runtime_callbacks_failure: Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            #[cfg(test)]
            pause_runtime_callback_reconfigure_before_reconcile: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            runtime_callback_reconfigure_paused: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            resume_runtime_callback_reconfigure: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            force_previous_authority_release_failure: Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            #[cfg(test)]
            force_current_authority_release_failure: Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            #[cfg(test)]
            runtime_state_replay_attempt_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            force_generation_bound_runtime_reconcile_failure: Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            #[cfg(test)]
            force_managed_profile_ownership_commit_failure: Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            #[cfg(test)]
            force_required_page_hook_install_failure: Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            #[cfg(test)]
            pause_authority_commit_after_projection: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            authority_commit_projected: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            resume_authority_commit: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn authority_commit_in_progress(&self) -> bool {
        self.authority_commit_in_progress.load(Ordering::SeqCst)
    }

    fn set_authority_commit_in_progress(&self, in_progress: bool) {
        self.authority_commit_in_progress
            .store(in_progress, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn runtime_callback_reconfigure_in_progress(&self) -> bool {
        self.runtime_callback_reconfigure_in_progress
            .load(Ordering::SeqCst)
    }

    fn set_runtime_callback_reconfigure_in_progress(&self, in_progress: bool) {
        self.runtime_callback_reconfigure_in_progress
            .store(in_progress, Ordering::SeqCst);
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

    /// Return whether a deferred runtime-state callback may still publish.
    ///
    /// Runtime-state callbacks can hand work to an async queue owned by the
    /// embedding daemon. The callback invocation itself is guarded in CDP, but
    /// the queued write must also prove that it still belongs to the same
    /// listener generation and is not crossing an authority/reconfigure fence.
    pub fn runtime_state_callback_publish_allowed(
        &self,
        listener_generation: ListenerGeneration,
    ) -> bool {
        self.listener_generation() == listener_generation
            && !self.authority_commit_in_progress()
            && !self
                .runtime_callback_reconfigure_in_progress
                .load(Ordering::SeqCst)
    }

    pub async fn runtime_state_callback_publish_allowed_for_active_target(
        &self,
        listener_generation: ListenerGeneration,
        active_target_id: Option<&str>,
    ) -> bool {
        if !self.runtime_state_callback_publish_allowed(listener_generation) {
            return false;
        }
        let Some(active_target_id) = active_target_id else {
            return true;
        };
        self.tab_projection
            .lock()
            .await
            .active_target_id
            .as_ref()
            .is_some_and(|target| target.as_ref() == active_target_id)
    }

    pub async fn publish_runtime_state_callback_if_active_target<F, Fut>(
        &self,
        listener_generation: ListenerGeneration,
        active_target_id: Option<&str>,
        publish: F,
    ) -> bool
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = bool> + Send,
    {
        if !self.runtime_state_callback_publish_allowed(listener_generation) {
            return false;
        }
        let projection = self.tab_projection.lock().await;
        if let Some(active_target_id) = active_target_id
            && projection
                .active_target_id
                .as_ref()
                .is_none_or(|target| target.as_ref() != active_target_id)
        {
            return false;
        }
        publish().await
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
        let mut degraded_runtime_fallback = None;
        let mut managed_reconnect_url = None;
        {
            let browser_guard = self.browser.lock().await;
            if let Some(browser) = browser_guard.clone() {
                drop(browser_guard);
                let was_external = *self.is_external.lock().await;
                let browser_live = timeout(
                    Duration::from_secs(2),
                    browser.execute(GetVersionParams::default()),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .is_some();
                if browser_live {
                    return Ok(());
                }
                warn!(
                    "Existing browser authority became unavailable; clearing stale handle and relaunching"
                );
                degraded_runtime_fallback = Some(self.snapshot_browser_runtime_fallback().await);
                if !was_external {
                    managed_reconnect_url = Some(browser.websocket_address().clone());
                }
                self.bump_listener_generation();
                self.clear_local_browser_authority().await;
            }
        }

        let existing_target = self.connection_target.lock().await.clone();
        let install = match self
            .resolve_browser_authority_install(existing_target, managed_reconnect_url)
            .await
        {
            Ok(install) => install,
            Err(error) => {
                if let Some(snapshot) = degraded_runtime_fallback {
                    self.restore_degraded_runtime_fallback_after_failed_authority_rebuild(snapshot)
                        .await;
                }
                return Err(error);
            }
        };
        if let Err(error) = self
            .install_runtime_state_locked(
                install.browser,
                install.page,
                install.is_external,
                install.connection_target,
                install.managed_profile,
                #[cfg(test)]
                install.managed_browser_test_permit,
            )
            .await
        {
            if let Some(snapshot) = degraded_runtime_fallback {
                self.restore_degraded_runtime_fallback_after_failed_authority_rebuild(snapshot)
                    .await;
            }
            return Err(error);
        }

        info!("Browser launched successfully");
        Ok(())
    }

    /// Set callback for epoch increment on CDP events.
    pub async fn set_epoch_callback(&self, callback: EpochCallback) {
        *self.epoch_callback.lock().await = Some(callback);
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

    pub async fn dialog_runtime_snapshot(&self) -> Result<DialogRuntimeInfo, RubError> {
        let _launch_guard = self.launch_lock.lock().await;
        if let Err(error) = self.ensure_browser_locked().await {
            if let Some(runtime) = self
                .degraded_dialog_runtime_after_failed_authority_rebuild()
                .await
            {
                return Ok(runtime);
            }
            return Err(error);
        }
        self.sync_tabs_projection().await?;
        Ok(self.dialog_runtime.read().await.clone())
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
        let policy = self.bind_dialog_intercept_policy(policy)?;
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

    fn bind_dialog_intercept_policy(
        &self,
        mut policy: rub_core::model::DialogInterceptPolicy,
    ) -> Result<rub_core::model::DialogInterceptPolicy, RubError> {
        if policy.target_tab_id.is_some() {
            return Ok(policy);
        }
        if self.authority_commit_in_progress() {
            return Err(dialog_intercept_wildcard_authority_error(
                "dialog_intercept_target_tab_authority_commit_in_progress",
                None,
            ));
        }
        let projection = self.tab_projection.try_lock().map_err(|_| {
            dialog_intercept_wildcard_authority_error(
                "dialog_intercept_target_tab_authority_unavailable",
                None,
            )
        })?;
        let page_count = projection.pages.len();
        let Some(page) = (page_count == 1).then(|| projection.pages[0].clone()) else {
            let reason = if page_count == 0 {
                "dialog_intercept_target_tab_authority_missing"
            } else {
                "dialog_intercept_target_tab_authority_ambiguous"
            };
            return Err(dialog_intercept_wildcard_authority_error(
                reason,
                Some(page_count),
            ));
        };
        policy.target_tab_id = Some(page.target_id().as_ref().to_string());
        Ok(policy)
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
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        self.network_rule_runtime.write().await.replace_rules(rules);
        self.sync_tabs_projection().await?;
        let pages = self.tab_projection.lock().await.pages.clone();
        crate::network_rules::sync_fetch_domain_for_pages(&pages, self.network_rule_runtime.clone())
            .await
    }

    /// Set connection target metadata for diagnostics on managed launches.
    pub async fn set_connection_target(&self, target: ConnectionTarget) {
        *self.connection_target.lock().await = Some(target.clone());
        self.update_connection_target_projection(Some(target));
    }

    #[cfg(test)]
    pub(super) async fn managed_profile_authority_for_test(&self) -> Option<ManagedProfileDir> {
        self.managed_profile.lock().await.clone()
    }

    pub fn launch_policy_info(&self) -> LaunchPolicyInfo {
        let cached_projection = self
            .launch_policy_projection
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let authority_commit_in_progress = self.authority_commit_in_progress();
        let connection_target = if authority_commit_in_progress {
            cached_projection.connection_target.clone()
        } else {
            self.connection_target
                .try_lock()
                .ok()
                .and_then(|guard| guard.clone())
                .or(cached_projection.connection_target.clone())
        };
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
            stealth_coverage: if authority_commit_in_progress {
                cached_projection.stealth_coverage
            } else {
                self.identity_coverage
                    .try_lock()
                    .ok()
                    .map(|coverage| coverage.project())
                    .or(cached_projection.stealth_coverage)
            },
        }
    }

    /// Whether this manager is connected to an external browser.
    pub async fn is_external(&self) -> bool {
        if self.authority_commit_in_progress() {
            let _launch_guard = self.launch_lock.lock().await;
        }
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
        self.replace_browser_authority_locked(browser, page, true, Some(target), None)
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
        let _launch_guard = self.launch_lock.lock().await;
        self.ensure_browser_locked().await?;
        self.sync_tabs_projection().await?;
        self.projected_active_page().await
    }

    async fn sync_tabs_projection(&self) -> Result<(), RubError> {
        let context = self
            .projection_context(self.browser_handle().await?, self.listener_generation())
            .await;
        crate::tab_projection::sync_tabs_projection_with(&context, self.tab_projection.clone())
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
            let tab_projection = self.tab_projection.clone();
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
                    if let Err(error) = crate::tab_projection::sync_tabs_projection_with(
                        &context,
                        tab_projection.clone(),
                    )
                    .await
                    {
                        warn!(
                            target_id = %event.target_info.target_id.as_ref(),
                            "failed to sync tab projection after target_created: {error}"
                        );
                    }
                }
            });
        }

        if let Ok(mut listener) = browser.event_listener::<EventTargetDestroyed>().await {
            let tab_projection = self.tab_projection.clone();
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
                    if let Err(error) = crate::tab_projection::sync_tabs_projection_with(
                        &context,
                        tab_projection.clone(),
                    )
                    .await
                    {
                        warn!(
                            target_id = %event.target_id.as_ref(),
                            "failed to sync tab projection after target_destroyed: {error}"
                        );
                    }
                }
            });
        }

        if let Ok(mut listener) = browser.event_listener::<EventTargetInfoChanged>().await {
            let tab_projection = self.tab_projection.clone();
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
                    if let Err(error) = crate::tab_projection::sync_tabs_projection_with(
                        &context,
                        tab_projection.clone(),
                    )
                    .await
                    {
                        warn!(
                            target_id = %event.target_info.target_id.as_ref(),
                            "failed to sync tab projection after target_info_changed: {error}"
                        );
                    }
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
            if let Err(error) = crate::tab_projection::ensure_page_hooks(page, &context, 0).await {
                warn!("failed to ensure page hooks for epoch listener page: {error}");
            }
        }
    }
}

impl BrowserManager {
    pub(super) async fn projected_continuity_page(&self) -> Option<Arc<Page>> {
        self.tab_projection.lock().await.continuity_page.clone()
    }

    pub(super) async fn projected_active_page(&self) -> Result<Arc<Page>, RubError> {
        let projection = self.tab_projection.lock().await.clone();
        if let Some(page) = projection.current_page {
            return Ok(page);
        }
        Err(active_page_authority_error(projection.pages.len()))
    }
}

pub(super) fn active_page_authority_error(projected_page_count: usize) -> RubError {
    if projected_page_count == 0 {
        RubError::domain(ErrorCode::BrowserCrashed, "No active page")
    } else {
        RubError::domain_with_context(
            ErrorCode::TabNotFound,
            "Active tab authority is unavailable because browser truth is ambiguous across live tabs",
            serde_json::json!({
                "reason": "active_tab_authority_unavailable",
                "projected_page_count": projected_page_count,
            }),
        )
    }
}

fn dialog_intercept_wildcard_authority_error(
    reason: &str,
    projected_page_count: Option<usize>,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::InvalidInput,
        "Cannot arm wildcard dialog intercept without a single authoritative tab; bind an explicit target tab instead.",
        serde_json::json!({
            "reason": reason,
            "projected_page_count": projected_page_count,
        }),
    )
}
